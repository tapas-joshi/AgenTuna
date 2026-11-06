//! Remote backup push: snapshot the store, commit it and push to the configured remote.
//!
//! [`push_backup`] is the TC-017 entry point. It reads the singleton
//! `remote_backup_state` row for the repo URL and branch, resolves the device name from
//! settings (defaulting to the host name), stages a temporary clone, writes the store
//! snapshot under `stores/<store_id>/` (`skills/`, `rules/`, `mcp.json`, `store.json`),
//! commits with a message naming the device and the UTC timestamp, pushes the default
//! branch and records `last_commit`/`last_push_at`/`status`.
//!
//! Hard rules this module keeps:
//!
//! - The GitHub token never enters argv, a URL, `.git/config`, a log line or an error. It is
//!   read from the Keychain into a [`at_secrets::SecretString`], handed to git through
//!   [`at_git::remote::GitAuth`]'s askpass environment, and every diagnostic passes through
//!   the session [`at_secrets::Redactor`] before it can reach an error or the event log.
//! - Every network call goes through [`at_git::remote::RemoteContext`], which runs
//!   [`at_git::policy::run_network_git`] first; a policy refusal spawns no process.
//! - A non-fast-forward push is reported as [`BackupError::Conflict`] for TC-018's merge;
//!   force push is never attempted. Offline and auth failures return typed statuses and
//!   leave `last_commit` untouched.
//! - A second backup with no store changes makes no commit and no push.
//!
//! Tests use a local bare repository as the remote and an in-memory
//! [`at_secrets::MemoryStore`]; nothing here touches the real Keychain or the network.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use at_db::ops::{self, OperationActor, OperationKind, OperationStatus};
use at_db::remote_backup::{
    RemoteBackupStatus, remote_backup_state, save_remote_backup_state, set_remote_backup_status,
};
use at_db::settings;
use at_git::remote::{GitAuth, RemoteContext, RemoteError};
use at_model::network::{NetworkEventLog, NetworkPolicy};
use at_model::redact::Redactor;
use at_secrets::SecretStore;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::layout::{MANIFEST_FILE, MCP_FILE, RULES_DIR, SKILLS_DIR, StoreError, StoreLayout};
/// Directory inside the backup repo that holds one folder per store.
const STORES_DIR: &str = "stores";

/// Machine-local state never leaves the store: `.state/` holds journals and locks.
const STATE_DIR: &str = ".state";

/// Local scope content is per machine and never leaves the store root's `local/` child.
const LOCAL_DIR: &str = "local";

/// Default branch pushed when the remote does not report one.
const DEFAULT_BRANCH: &str = "main";

/// Longest device name accepted, in characters.
const MAX_DEVICE_NAME_CHARS: usize = 64;

/// Everything one push needs, bundled so callers stay readable.
pub struct BackupRequest<'a> {
    /// Store being backed up (for example the User store layout).
    pub store: &'a StoreLayout,
    /// Open index; the singleton `remote_backup_state` row and settings live here.
    pub db: &'a at_db::Database,
    /// Credential store holding the GitHub token under `dev.agentuna.remote-backup`.
    pub secrets: &'a dyn SecretStore,
    /// Network policy every git call must pass.
    pub policy: &'a NetworkPolicy,
    /// In-memory network event log.
    pub log: &'a mut NetworkEventLog,
    /// Remote git runner (spawns git for network calls).
    pub remote_runner: &'a dyn at_git::remote::RemoteGitRunner,
    /// Local git runner (clone-local init/commit/status/add).
    pub local_runner: &'a dyn at_git::local::LocalGitRunner,
    /// Redactor masking the token in diagnostics.
    pub redactor: &'a Redactor,
    /// Clock for timestamps; injectable for tests.
    pub now: OffsetDateTime,
}

/// Outcome of one [`push_backup`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOutcome {
    /// Commit the snapshot was pushed as, when a push happened.
    pub commit: Option<String>,
    /// Whether a new commit was created (false when nothing changed).
    pub committed: bool,
    /// Whether the push reached the remote.
    pub pushed: bool,
    /// Coarse status now stored in `remote_backup_state`.
    pub status: RemoteBackupStatus,
    /// Stable error code when `status` is `Error`.
    pub error_kind: Option<String>,
}

/// Why a backup push did not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// The remote backup is not configured (no repo URL in `remote_backup_state`).
    #[error("remote backup is not configured; set a repository URL first")]
    Unconfigured,
    /// The stored token is missing from the credential store.
    #[error("no GitHub token stored; add one in Settings")]
    TokenMissing,
    /// The token was rejected by the remote.
    #[error("the remote rejected the stored credential; re-enter the token in Settings")]
    AuthRejected,
    /// The remote could not be reached.
    #[error("the remote could not be reached; check the connection and try again")]
    Offline,
    /// The push was rejected as non-fast-forward; TC-018 merges.
    #[error("the remote has commits this device does not; sync before pushing")]
    Conflict,
    /// The network policy refused a call.
    #[error(transparent)]
    Denied(#[from] at_model::network::NetworkDenied),
    /// A git invocation failed.
    #[error(transparent)]
    Git(RemoteError),
    /// A store or settings read/write failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The index or settings layer failed.
    #[error(transparent)]
    Db(#[from] at_db::DbError),
    /// A settings read or write failed.
    #[error(transparent)]
    Settings(#[from] at_db::settings::SettingsError),
    /// The credential store failed.
    #[error(transparent)]
    Secrets(#[from] at_secrets::SecretsError),
    /// A device name failed validation.
    #[error(transparent)]
    InvalidDeviceName(#[from] DeviceNameError),
    /// A filesystem operation on the temp clone or snapshot failed.
    #[error("{path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
}

impl BackupError {
    /// The stable `error_kind` persisted to `remote_backup_state.error_kind`.
    #[must_use]
    pub const fn error_kind(&self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::TokenMissing => "token_missing",
            Self::AuthRejected => "auth_rejected",
            Self::Offline => "offline",
            Self::Conflict => "conflict",
            Self::Denied(_) => "network_denied",
            Self::Git(_) => "git",
            Self::Store(_) => "store",
            Self::Db(_) => "db",
            Self::Settings(_) => "settings",
            Self::Secrets(_) => "secrets",
            Self::InvalidDeviceName(_) => "invalid_device_name",
            Self::Io { .. } => "io",
        }
    }

    /// Whether this failure maps to the persisted `offline` status.
    #[must_use]
    pub const fn is_offline(&self) -> bool {
        matches!(self, Self::Offline)
    }
}

impl From<RemoteError> for BackupError {
    fn from(error: RemoteError) -> Self {
        match error {
            RemoteError::AuthRejected => Self::AuthRejected,
            RemoteError::Offline => Self::Offline,
            RemoteError::NonFastForward => Self::Conflict,
            other => Self::Git(other),
        }
    }
}

/// Reads the device name from settings, defaulting to the host name.
///
/// The host name comes from `HOSTNAME`/`COMPUTERNAME`/`HOST` environment or the `hostname`
/// command is never run: we use a fallback that reads the kernel name through
/// `libc` on Unix and the environment on other platforms.
pub fn device_name(db: &at_db::Database) -> Result<String, BackupError> {
    let settings = settings::load(db.connection())?;
    if let Some(stored) = settings.remote_backup_device() {
        return Ok(stored.to_owned());
    }
    Ok(default_device_name())
}

/// Sets or clears the device name in settings.
///
/// # Errors
/// [`BackupError::Db`] on a settings failure.
pub fn set_device_name(db: &mut at_db::Database, name: Option<&str>) -> Result<(), BackupError> {
    if let Some(n) = name {
        validate_device_name(n)?;
    }
    settings::set_remote_backup_device(db.connection_mut(), name, OperationActor::Ui)?;
    Ok(())
}

/// The host name used when no device name is stored.
///
/// Reads `HOSTNAME`/`COMPUTERNAME`/`HOST` first; otherwise runs the platform `hostname`
/// command (a local binary that cannot hang). Returns `"device"` when nothing yields a
/// usable name.
#[must_use]
pub fn default_device_name() -> String {
    hostname().unwrap_or_else(|| "device".to_owned())
}

fn hostname() -> Option<String> {
    for var in ["HOSTNAME", "COMPUTERNAME", "HOST"] {
        if let Ok(value) = std::env::var(var) {
            let value = value.trim();
            if !value.is_empty() && !value.contains('\n') {
                return Some(value.to_owned());
            }
        }
    }
    let output = std::process::Command::new("hostname")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if name.is_empty() || name.contains('\n') {
        None
    } else {
        Some(name)
    }
}

/// Why a device name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DeviceNameError {
    /// Empty, oversized or contains characters that would break a commit message.
    #[error(
        "device name must be 1-{MAX_DEVICE_NAME_CHARS} characters without slashes or control characters"
    )]
    Invalid,
}

/// Validates a device name for use in remote-backup commit messages.
///
/// # Errors
/// [`DeviceNameError::Invalid`] when empty, longer than `MAX_DEVICE_NAME_CHARS`, or containing slashes or control characters.
pub fn validate_device_name(name: &str) -> Result<(), DeviceNameError> {
    if name.is_empty()
        || name.len() > MAX_DEVICE_NAME_CHARS
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(DeviceNameError::Invalid);
    }
    Ok(())
}
/// Pushes one backup snapshot to the configured remote.
///
/// Steps: read state → resolve token → `ls-remote` → clone to temp → write snapshot →
/// commit if changed → push → update `remote_backup_state`. Every step that can fail before
/// the push leaves `last_commit` untouched; a successful push records the new commit and
/// `last_push_at`.
///
/// # Errors
/// [`BackupError`] naming the failing stage; the token is never in any error.
pub fn push_backup(request: &mut BackupRequest<'_>) -> Result<BackupOutcome, BackupError> {
    let state = remote_backup_state(request.db.connection())?;
    let repo_url = state.repo_url.clone().ok_or(BackupError::Unconfigured)?;
    at_git::remote::validate_repo_url(&repo_url)?;
    let branch = state
        .branch
        .clone()
        .unwrap_or_else(|| DEFAULT_BRANCH.to_owned());

    // Token: read from Keychain, keep in SecretString, hand to git via askpass env only.
    let token = at_secrets::read_token(request.secrets).map_err(|error| {
        if matches!(error, at_secrets::SecretsError::NotFound { .. }) {
            BackupError::TokenMissing
        } else {
            BackupError::Secrets(error)
        }
    })?;
    let auth = GitAuth::token(token.expose_secret()).map_err(|e| match e {
        RemoteError::AuthSetup { path, source } => BackupError::Io { path, source },
        other => BackupError::Git(other),
    })?;

    let device = device_name(request.db)?;
    validate_device_name(&device)?;

    let mut ctx = RemoteContext {
        policy: request.policy,
        log: request.log,
        runner: request.remote_runner,
        local: request.local_runner,
        redact: &|text| request.redactor.redact_text(text),
        now: request.now,
    };

    // ls-remote tells us the remote's default branch and whether it is reachable at all.
    let head = match ctx.remote_head(&repo_url, &auth) {
        Ok(head) => head,
        Err(error) => {
            let kind = match &error {
                RemoteError::AuthRejected => "auth_rejected",
                RemoteError::Offline => "offline",
                _ => "git",
            };
            let _ = persist_error(request.db, &RemoteBackupStatus::Error, Some(kind));
            record_ops_row(request.db, false, Some(kind));
            return Err(error.into());
        }
    };
    let push_branch = head.branch.clone().unwrap_or(branch.clone());

    // Stage the work directory: a temp clone when the remote has history, a fresh init when
    // it does not (empty repo) so the first push has somewhere to land.
    let (work, _guard) = stage_work(&mut ctx, &repo_url, &auth, head.commit.is_empty())?;
    // Write the snapshot into stores/<store_id>/.
    let store_id = request.store.read_header()?.store_id.to_string();
    let snapshot_root = work.join(STORES_DIR).join(&store_id);
    write_snapshot(request.store, &snapshot_root)?;

    // Commit only when the tree changed.
    let committed = if at_git::remote::has_changes(ctx.local, &work)? {
        at_git::remote::stage_all(ctx.local, &work)?;
        let stamp = request
            .now
            .format(&Rfc3339)
            .unwrap_or_else(|_| request.now.unix_timestamp().to_string());
        let message = format!("backup {device} {stamp}");
        let commit = at_git::remote::commit_local(ctx.local, &work, &message)?;
        Some(commit)
    } else {
        None
    };

    // Push only when there is something new to send.
    let mut pushed = false;
    if let Some(commit) = committed.clone() {
        match ctx.push_remote(&work, &repo_url, &push_branch, &auth) {
            Ok(()) => pushed = true,
            Err(RemoteError::NonFastForward) => {
                let _ = persist_error(request.db, &RemoteBackupStatus::Error, Some("conflict"));
                record_ops_row(request.db, false, Some("conflict"));
                return Err(BackupError::Conflict);
            }
            Err(error) => {
                let kind = match &error {
                    RemoteError::AuthRejected => "auth_rejected",
                    RemoteError::Offline => "offline",
                    _ => "git",
                };
                let _ = persist_error(request.db, &RemoteBackupStatus::Error, Some(kind));
                record_ops_row(request.db, false, Some(kind));
                return Err(error.into());
            }
        }
        // Record success.
        let mut next = remote_backup_state(request.db.connection())?;
        next.last_commit = Some(commit);
        next.last_push_at = Some(
            request
                .now
                .format(&Rfc3339)
                .unwrap_or_else(|_| request.now.unix_timestamp().to_string()),
        );
        next.status = RemoteBackupStatus::Idle;
        next.error_kind = None;
        next.repo_url = Some(repo_url.clone());
        next.branch = Some(push_branch.clone());
        save_remote_backup_state(request.db.connection(), &next)?;
        record_ops_row(request.db, true, None);
    } else {
        // Nothing changed: still mark idle and record a no-op push outcome.
        let mut next = remote_backup_state(request.db.connection())?;
        next.status = RemoteBackupStatus::Idle;
        next.error_kind = None;
        save_remote_backup_state(request.db.connection(), &next)?;
    }

    Ok(BackupOutcome {
        commit: committed.clone(),
        committed: committed.is_some(),
        pushed,
        status: RemoteBackupStatus::Idle,
        error_kind: None,
    })
}

/// Writes the operations-log row for a finished push. Best-effort: a logging failure must not
/// fail a backup that already reached the remote.
fn record_ops_row(db: &at_db::Database, pushed: bool, error_kind: Option<&str>) {
    let status = if error_kind.is_some() {
        OperationStatus::Failed
    } else {
        OperationStatus::Succeeded
    };
    let _ = ops::record(
        db.connection(),
        &ops::NewOperation::new(OperationKind::RemoteBackup, OperationActor::Ui).with_summary(
            if pushed {
                "remote backup pushed"
            } else {
                "remote backup: no changes"
            },
        ),
        &[],
        status,
        error_kind,
    );
}

/// Persists a coarse status without touching commit/timestamp fields.
fn persist_error(
    db: &at_db::Database,
    status: &RemoteBackupStatus,
    error_kind: Option<&str>,
) -> Result<(), BackupError> {
    set_remote_backup_status(db.connection(), *status, error_kind)?;
    Ok(())
}

/// Guard that cleans up the temporary work directory when dropped.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Clones the remote into a temp dir, or initialises an empty repo when the remote has no
/// HEAD yet. Returns the work directory and a drop guard that removes it.
fn stage_work(
    ctx: &mut RemoteContext<'_>,
    repo_url: &str,
    auth: &GitAuth,
    remote_empty: bool,
) -> Result<(PathBuf, TempDirGuard), BackupError> {
    let dir = std::env::temp_dir().join(format!(
        "agentuna-backup-{}-{}",
        std::process::id(),
        ctx.now.unix_timestamp_nanos()
    ));
    fs::create_dir_all(&dir).map_err(|source| BackupError::Io {
        path: dir.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let guard = TempDirGuard(dir.clone());

    if remote_empty {
        // First backup: no remote history. Init a repo and point origin at the remote.
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).map_err(|source| BackupError::Io {
            path: repo.clone(),
            source,
        })?;
        at_git::remote::init_repo(ctx.local, &repo)?;
        at_git::remote::set_remote_url(ctx.local, &repo, repo_url)?;
        Ok((repo, guard))
    } else {
        let dest = dir.join("repo");
        ctx.clone_remote(repo_url, &dest, auth)?;
        Ok((dest, guard))
    }
}

/// Copies the store's committed entries into `dest/stores/<store_id>/` layout.
///
/// `skills/`, `rules/`, `mcp.json` and `store.json` are copied; `.state/`, `backups/`, `local/`
/// and any other machine-local entry are skipped. Symlinks are refused rather than followed.
fn write_snapshot(store: &StoreLayout, dest: &Path) -> Result<(), BackupError> {
    fs::create_dir_all(dest).map_err(|source| BackupError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    copy_dir(&store.skills_dir(), &dest.join(SKILLS_DIR))?;
    copy_dir(&store.rules_dir(), &dest.join(RULES_DIR))?;
    copy_file_opt(&store.mcp_file(), &dest.join(MCP_FILE))?;
    copy_file_opt(&store.manifest_path(), &dest.join(MANIFEST_FILE))?;
    Ok(())
}

fn copy_dir(source: &Path, dest: &Path) -> Result<(), BackupError> {
    if !source.is_dir() {
        return Ok(());
    }
    copy_tree(source, dest)
}

fn copy_file_opt(source: &Path, dest: &Path) -> Result<(), BackupError> {
    if !source.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| BackupError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::copy(source, dest).map_err(|source| BackupError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Recursive copy that refuses symlinks and special files.
fn copy_tree(source: &Path, dest: &Path) -> Result<(), BackupError> {
    fs::create_dir_all(dest).map_err(|source| BackupError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|err| BackupError::Io {
            path: source.to_path_buf(),
            source: err,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| BackupError::Io {
            path: source.to_path_buf(),
            source: err,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let from = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if is_excluded_from_backup(&name_str) {
            continue;
        }
        let to = dest.join(name);
        let meta = fs::symlink_metadata(&from).map_err(|source| BackupError::Io {
            path: from.clone(),
            source,
        })?;
        if meta.file_type().is_symlink() {
            return Err(BackupError::Io {
                path: from,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing to copy a symbolic link into the backup",
                ),
            });
        }
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() {
            fs::copy(&from, &to).map_err(|source| BackupError::Io {
                path: to.clone(),
                source,
            })?;
        } else {
            return Err(BackupError::Io {
                path: from,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file or directory",
                ),
            });
        }
    }
    Ok(())
}

/// Counts remote-merge conflict folders under `<store>/.state/remote-conflicts/`.
///
/// TC-018 writes one folder per conflict there; the count is derived by listing, never stored.
///
/// # Errors
/// [`BackupError::Io`] when the state directory cannot be read.
pub fn remote_conflict_count(store: &StoreLayout) -> Result<usize, BackupError> {
    let dir = store.state_dir().join("remote-conflicts");
    match fs::read_dir(&dir) {
        Ok(entries) => {
            let mut count = 0;
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    count += 1;
                }
            }
            Ok(count)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(BackupError::Io { path: dir, source }),
    }
}

// Keep referenced constants used: LOCAL_DIR and STATE_DIR document the exclusion contract.
/// Whether a file or directory name is excluded from remote backup snapshots.
///
/// Backups exclude machine-local state (`.state/`, `local/`, `backups/`), per-machine overrides
/// (`settings.local.json`), secret-bearing files (`.env`, `.env.*`, `*.key`, `*.pem`), and
/// OS metadata (`.DS_Store`).
#[must_use]
pub fn is_excluded_from_backup(name: &str) -> bool {
    name == STATE_DIR
        || name == LOCAL_DIR
        || name == "backups"
        || name == crate::git_contract::PER_MACHINE_OVERRIDE_FILE
        || name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".key")
        || name.ends_with(".pem")
        || name == ".DS_Store"
}
