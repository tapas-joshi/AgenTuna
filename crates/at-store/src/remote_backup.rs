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

use std::collections::BTreeMap;
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

use crate::integrity::verify_store;
use crate::layout::{MANIFEST_FILE, MCP_FILE, RULES_DIR, SKILLS_DIR, StoreError, StoreLayout};
use crate::manifest::StoreManifest;
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

/// Outcome of one remote backup push or sync.
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
    /// Divergent local and preserved remote paths created by this sync.
    pub conflicts: Vec<RemoteConflict>,
}

/// One divergent item/file retained on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConflict {
    /// The existing local copy, never overwritten.
    pub local_path: PathBuf,
    /// The separate remote copy under `.state/remote-conflicts/`.
    pub remote_path: PathBuf,
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
    /// Remote deletion of locally-unmodified content requires explicit confirmation.
    #[error("remote deletion would remove unmodified local content; confirm deletions and retry")]
    DeletionConfirmationRequired,
    /// Snapshot path is invalid or escaped its expected root.
    #[error("invalid remote snapshot path: {0}")]
    InvalidSnapshotPath(PathBuf),
    /// A ChangeSet could not be validated or safely applied.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// A reviewed ChangeSet failed during safe filesystem application.
    #[error(transparent)]
    Apply(#[from] at_fs::changeset::ChangeSetApplyError),
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
    /// A settings read/write failed.
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
    /// The destination store already contains managed content and merge was not confirmed.
    #[error("remote restore requires an empty store unless merge is explicitly confirmed")]
    NonEmptyRestore,
    /// Snapshot manifest content does not match the declared hash for this item.
    #[error("remote snapshot item `{item}` failed integrity verification")]
    CorruptSnapshot {
        /// Stable id or path of the first invalid snapshot item.
        item: String,
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
            Self::DeletionConfirmationRequired => "confirmation_required",
            Self::InvalidSnapshotPath(_) => "invalid_snapshot",
            Self::ChangeSet(_) => "changeset",
            Self::Apply(_) => "apply",
            Self::Denied(_) => "network_denied",
            Self::NonEmptyRestore => "non_empty",
            Self::CorruptSnapshot { .. } => "corrupt_snapshot",
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
        conflicts: Vec::new(),
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
    // Two backups can start in the same instant (an injected clock, or coarse OS timers), so a
    // process-wide sequence number keeps their work directories apart.
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "agentuna-backup-{}-{}-{}",
        std::process::id(),
        ctx.now.unix_timestamp_nanos(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// Counts unresolved per-sync conflict folders under `<store>/.state/remote-conflicts/<device>/`.
///
/// A folder containing a regular `.agentuna-resolved` marker has been fully reviewed. The
/// marker is written as part of the reviewed conflict-resolution ChangeSet.
///
/// # Errors
/// [`BackupError::Io`] when the state directory cannot be read.
pub fn remote_conflict_count(store: &StoreLayout) -> Result<usize, BackupError> {
    let root = store.state_dir().join("remote-conflicts");
    let devices = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(BackupError::Io { path: root, source });
        }
    };
    let mut count = 0;
    for device in devices {
        let device = device.map_err(|source| BackupError::Io {
            path: root.clone(),
            source,
        })?;
        if !device
            .file_type()
            .map_err(|source| BackupError::Io {
                path: device.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let device_dir = device.path();
        let conflicts = match fs::read_dir(&device_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(BackupError::Io {
                    path: device_dir,
                    source,
                });
            }
        };
        for conflict in conflicts {
            let conflict = conflict.map_err(|source| BackupError::Io {
                path: device_dir.clone(),
                source,
            })?;
            let conflict_path = conflict.path();
            let conflict_type = conflict.file_type().map_err(|source| BackupError::Io {
                path: conflict_path.clone(),
                source,
            })?;
            if conflict_type.is_dir() {
                let marker = conflict_path.join(".agentuna-resolved");
                let resolved = match fs::symlink_metadata(&marker) {
                    Ok(metadata) => metadata.is_file() && !metadata.file_type().is_symlink(),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(source) => {
                        return Err(BackupError::Io {
                            path: marker,
                            source,
                        });
                    }
                };
                if !resolved {
                    count += 1;
                }
            }
        }
    }
    Ok(count)
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
/// Syncs the store with the configured remote using a three-way file merge.
///
/// `confirm_deletions` is the user's explicit confirmation for remote deletions of
/// locally-unmodified files. Divergent files remain untouched locally and are saved in
/// `.state/remote-conflicts/<device>/<timestamp>/` before the merged snapshot is pushed.
///
/// # Errors
/// Returns [`BackupError::DeletionConfirmationRequired`] without changing the store when a
/// remote deletion would remove locally-unmodified content and confirmation is absent.
pub fn sync_from_remote(
    request: &mut BackupRequest<'_>,
    confirm_deletions: bool,
) -> Result<BackupOutcome, BackupError> {
    let state = remote_backup_state(request.db.connection())?;
    let repo_url = state.repo_url.clone().ok_or(BackupError::Unconfigured)?;
    at_git::remote::validate_repo_url(&repo_url)?;
    let branch = state
        .branch
        .clone()
        .unwrap_or_else(|| DEFAULT_BRANCH.to_owned());
    let token = at_secrets::read_token(request.secrets).map_err(|error| {
        if matches!(error, at_secrets::SecretsError::NotFound { .. }) {
            BackupError::TokenMissing
        } else {
            BackupError::Secrets(error)
        }
    })?;
    let auth = GitAuth::token(token.expose_secret()).map_err(|error| match error {
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
    let head = ctx.remote_head(&repo_url, &auth)?;
    let push_branch = head.branch.clone().unwrap_or(branch);
    let (work, _guard) = stage_work(&mut ctx, &repo_url, &auth, head.commit.is_empty())?;
    let store_id = request.store.read_header()?.store_id.to_string();
    let remote_root = work.join(STORES_DIR).join(&store_id);
    let local_files = collect_snapshot(request.store.root())?;
    let mut remote_files = collect_snapshot(&remote_root)?;
    rename_remote_skill_collisions(&local_files, &mut remote_files, &device)?;
    let base_files = match state.last_commit.as_deref() {
        Some(commit) => read_commit_snapshot(request.local_runner, &work, commit, &store_id)?,
        None => BTreeMap::new(),
    };
    let mut writes = Vec::new();
    let mut deletes = Vec::new();
    let mut conflicts = BTreeMap::<PathBuf, Vec<u8>>::new();
    let mut deletion_confirmation_needed = false;
    let all_paths: std::collections::BTreeSet<PathBuf> = base_files
        .keys()
        .chain(local_files.keys())
        .chain(remote_files.keys())
        .cloned()
        .collect();
    for path in all_paths {
        let base = base_files.get(&path);
        let local = local_files.get(&path);
        let remote = remote_files.get(&path);
        if path == Path::new(MANIFEST_FILE)
            && let (Some(local), Some(remote)) = (local, remote)
        {
            let merged = merge_manifest_bytes(local, remote, base)?;
            if &merged != local {
                writes.push((path, merged));
            }
            continue;
        }
        if local == remote {
            continue;
        }
        match (base, local, remote) {
            // An unchanged local file accepts a remote edit.
            (Some(base), Some(local), Some(remote)) if local == base => {
                writes.push((path, remote.clone()));
            }
            // Local edit wins when remote did not touch the file.
            (Some(base), Some(_local), Some(remote)) if remote == base => {}
            // Independent additions or edits on both devices: preserve the remote copy.
            (_, Some(_), Some(remote)) => {
                conflicts.insert(path, remote.clone());
            }
            // A new remote file can be imported without replacing a local file.
            (None, None, Some(remote)) => writes.push((path, remote.clone())),
            // Remote deletion is explicit only if local still equals the common base.
            (Some(base), Some(local), None) if local == base => {
                if confirm_deletions {
                    deletes.push(path);
                } else {
                    deletion_confirmation_needed = true;
                }
            }
            // Keep local edits when the remote deleted their base version.
            (Some(_), Some(_), None) => {}
            // Local-only additions remain local and will be pushed.
            (None, Some(_), None) => {}
            // A remote deletion of an already absent local path is already merged.
            (_, None, None) => {}
            // If the local copy disappeared while remote edited, retain the remote version.
            (Some(_), None, Some(remote)) => writes.push((path, remote.clone())),
        }
    }
    if deletion_confirmation_needed && !confirm_deletions {
        return Err(BackupError::DeletionConfirmationRequired);
    }

    let conflict_root = request
        .store
        .state_dir()
        .join("remote-conflicts")
        .join(safe_device_component(&device))
        .join(format!(
            "{}-{}",
            request.now.unix_timestamp(),
            &uuid::Uuid::now_v7().to_string()[..8]
        ));
    let mut ops = Vec::with_capacity(writes.len() + deletes.len() + conflicts.len());
    let mut write_parents = std::collections::BTreeSet::new();
    for (path, bytes) in writes {
        if let Some(parent) = path.parent() {
            let full_parent = request.store.root().join(parent);
            if !full_parent.is_dir() && write_parents.insert(parent.to_path_buf()) {
                ensure_change_set_directory(request.store, request.db, request.now, parent)?;
            }
        }
        ops.push(change_write(
            &request.store.root().join(&path),
            &path,
            bytes,
        )?);
    }
    for path in deletes {
        let target = request.store.root().join(&path);
        let bytes = fs::read(&target).map_err(|source| BackupError::Io {
            path: target.clone(),
            source,
        })?;
        ops.push(at_model::FileOp::Delete {
            path,
            expected: at_model::ExpectedState::hash(at_fs::sha256_hex(&bytes))?,
        });
    }
    let conflict_count = conflicts.len();
    let mut conflict_paths = Vec::with_capacity(conflict_count);
    let mut conflict_parents = std::collections::BTreeSet::new();
    for (path, bytes) in conflicts {
        let conflict_path = conflict_root.join(&path);
        let relative = conflict_path
            .strip_prefix(request.store.root())
            .map_err(|_| BackupError::InvalidSnapshotPath(conflict_path.clone()))?
            .to_path_buf();
        conflict_paths.push(RemoteConflict {
            local_path: request.store.root().join(&path),
            remote_path: request.store.root().join(&relative),
        });
        if let Some(parent) = relative.parent()
            && conflict_parents.insert(parent.to_path_buf())
        {
            ensure_change_set_directory(request.store, request.db, request.now, parent)?;
        }
        ops.push(change_write(&conflict_path, &relative, bytes)?);
    }
    if !ops.is_empty() {
        let header = request.store.read_header()?;
        let operation_kind = at_model::OperationKind::parse("remote_backup")?;
        let change_set = at_model::ChangeSet {
            id: uuid::Uuid::now_v7().to_string(),
            preview_id: uuid::Uuid::now_v7().to_string(),
            operation_kind,
            scope: Some(header.scope),
            project_id: None,
            summary: format!("sync remote backup for {device}"),
            ops,
            created_at_unix_seconds: request.now.unix_timestamp().max(0) as u64,
        };
        change_set.validate()?;
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(request.db.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: request.store.root(),
            backup_folder: &request.store.backups_dir(),
            recorder: &mut recorder,
        };
        at_fs::changeset::apply(&change_set, &mut context)?;
    }

    // Rebuild the remote snapshot from the now-merged local store. The conflict copies are
    // beneath .state/ and therefore never enter the remote snapshot.
    write_snapshot(request.store, &remote_root)?;
    let committed = if at_git::remote::has_changes(ctx.local, &work)? {
        at_git::remote::stage_all(ctx.local, &work)?;
        let stamp = request
            .now
            .format(&Rfc3339)
            .unwrap_or_else(|_| request.now.unix_timestamp().to_string());
        let message = format!("sync {device} {stamp}\n\nDevice: {device}");
        Some(at_git::remote::commit_local(ctx.local, &work, &message)?)
    } else {
        None
    };
    if let Some(commit) = committed.clone() {
        ctx.push_remote(&work, &repo_url, &push_branch, &auth)?;
        let stamp = request
            .now
            .format(&Rfc3339)
            .unwrap_or_else(|_| request.now.unix_timestamp().to_string());
        let mut next = remote_backup_state(request.db.connection())?;
        next.last_commit = Some(commit);
        next.last_pull_at = Some(stamp.clone());
        next.last_push_at = Some(stamp);
        next.repo_url = Some(repo_url);
        next.branch = Some(push_branch);
        next.status = RemoteBackupStatus::Idle;
        next.error_kind = None;
        save_remote_backup_state(request.db.connection(), &next)?;
    } else {
        let mut next = remote_backup_state(request.db.connection())?;
        if !head.commit.is_empty() {
            next.last_commit = Some(head.commit.clone());
        }
        next.last_pull_at = Some(
            request
                .now
                .format(&Rfc3339)
                .unwrap_or_else(|_| request.now.unix_timestamp().to_string()),
        );
        next.repo_url = Some(repo_url);
        next.branch = Some(push_branch);
        next.status = RemoteBackupStatus::Idle;
        next.error_kind = None;
        save_remote_backup_state(request.db.connection(), &next)?;
    }
    record_ops_row(request.db, committed.is_some(), None);
    Ok(BackupOutcome {
        commit: committed.clone(),
        committed: committed.is_some(),
        pushed: committed.is_some(),
        status: RemoteBackupStatus::Idle,
        error_kind: (conflict_count > 0).then(|| "conflict".to_owned()),
        conflicts: conflict_paths,
    })
}

/// Restores a commit's snapshot into an empty store.
///
/// Every network git call is authorized by the shared network policy through `RemoteContext`.
/// Merge confirmation is handled by TC-018's three-way sync operation instead of overwriting
/// existing data here.
///
/// # Errors
/// Returns typed errors for offline access, invalid snapshots, non-empty stores, and safe-write
/// failures. All snapshot checks complete before the first destination mutation.
pub fn restore_from_remote(
    request: &mut BackupRequest<'_>,
    commit: &str,
    merge_confirmed: bool,
) -> Result<BackupOutcome, BackupError> {
    if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BackupError::InvalidSnapshotPath(PathBuf::from(commit)));
    }
    let state = remote_backup_state(request.db.connection())?;
    let repo_url = state.repo_url.clone().ok_or(BackupError::Unconfigured)?;
    at_git::remote::validate_repo_url(&repo_url)?;
    let token = at_secrets::read_token(request.secrets).map_err(|error| {
        if matches!(error, at_secrets::SecretsError::NotFound { .. }) {
            BackupError::TokenMissing
        } else {
            BackupError::Secrets(error)
        }
    })?;
    let auth = GitAuth::token(token.expose_secret()).map_err(|error| match error {
        RemoteError::AuthSetup { path, source } => BackupError::Io { path, source },
        other => BackupError::Git(other),
    })?;
    let current_bytes =
        fs::read(request.store.manifest_path()).map_err(|source| BackupError::Io {
            path: request.store.manifest_path(),
            source,
        })?;
    let current_manifest =
        StoreManifest::parse(&current_bytes).map_err(|_| BackupError::CorruptSnapshot {
            item: MANIFEST_FILE.to_owned(),
        })?;
    if !current_manifest.items().is_empty() {
        if merge_confirmed {
            return sync_from_remote(request, false);
        }
        return Err(BackupError::NonEmptyRestore);
    }

    let mut context = RemoteContext {
        policy: request.policy,
        log: request.log,
        runner: request.remote_runner,
        local: request.local_runner,
        redact: &|text| request.redactor.redact_text(text),
        now: request.now,
    };
    let (work, _guard) = stage_work(&mut context, &repo_url, &auth, false)?;
    let store_id = store_id_for_commit(request.local_runner, &work, commit)?;
    let files = read_commit_snapshot(request.local_runner, &work, commit, &store_id)?;
    let snapshot_manifest =
        files
            .get(Path::new(MANIFEST_FILE))
            .ok_or_else(|| BackupError::CorruptSnapshot {
                item: MANIFEST_FILE.to_owned(),
            })?;
    let manifest =
        StoreManifest::parse(snapshot_manifest).map_err(|_| BackupError::CorruptSnapshot {
            item: MANIFEST_FILE.to_owned(),
        })?;
    let snapshot_root = work.join(STORES_DIR).join(&store_id);
    let integrity = verify_store(
        &StoreLayout::new(request.store.scope(), &snapshot_root),
        crate::store_io::StoreAccess::ReadOnly,
    )
    .map_err(|_| BackupError::CorruptSnapshot {
        item: manifest
            .items()
            .first()
            .map_or_else(|| MANIFEST_FILE.to_owned(), |item| item.id.to_string()),
    })?;
    if integrity.has_issues() {
        let failing_item = integrity
            .snapshot
            .items
            .iter()
            .find(|item| item.state != crate::store_io::StoreItemState::Present)
            .map(|item| item.item.id.to_string())
            .unwrap_or_else(|| MANIFEST_FILE.to_owned());
        return Err(BackupError::CorruptSnapshot { item: failing_item });
    }

    let mut operations = Vec::with_capacity(files.len());
    let mut created_parents = std::collections::BTreeSet::new();
    for (relative, bytes) in files {
        validate_snapshot_path(&relative)?;
        if let Some(parent) = relative.parent()
            && !parent.as_os_str().is_empty()
            && created_parents.insert(parent.to_path_buf())
            && !request.store.root().join(parent).is_dir()
        {
            ensure_change_set_directory(request.store, request.db, request.now, parent)?;
        }
        operations.push(change_write(
            &request.store.root().join(&relative),
            &relative,
            bytes,
        )?);
    }
    let change_set = at_model::ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: uuid::Uuid::now_v7().to_string(),
        operation_kind: at_model::OperationKind::parse("remote_restore")?,
        scope: Some(request.store.scope()),
        project_id: None,
        summary: format!("restore remote backup {commit}"),
        ops: operations,
        created_at_unix_seconds: request.now.unix_timestamp().max(0) as u64,
    };
    change_set.validate()?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(request.db.connection(), 30);
    let mut apply_context = at_fs::changeset::ApplyContext {
        store_root: request.store.root(),
        backup_folder: &request.store.backups_dir(),
        recorder: &mut recorder,
    };
    at_fs::changeset::apply(&change_set, &mut apply_context)?;

    let mut next = remote_backup_state(request.db.connection())?;
    next.last_commit = Some(commit.to_owned());
    next.last_pull_at = Some(
        request
            .now
            .format(&Rfc3339)
            .unwrap_or_else(|_| request.now.unix_timestamp().to_string()),
    );
    next.status = RemoteBackupStatus::Idle;
    next.error_kind = None;
    save_remote_backup_state(request.db.connection(), &next)?;
    record_ops_row(request.db, true, None);
    Ok(BackupOutcome {
        commit: Some(commit.to_owned()),
        committed: false,
        pushed: false,
        status: RemoteBackupStatus::Idle,
        error_kind: None,
        conflicts: Vec::new(),
    })
}

fn store_id_for_commit(
    runner: &dyn at_git::local::LocalGitRunner,
    repo: &Path,
    commit: &str,
) -> Result<String, BackupError> {
    let args = [
        "ls-tree",
        "-r",
        "-z",
        "--name-only",
        commit,
        "--",
        STORES_DIR,
    ];
    let output = runner
        .run(&at_git::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: true,
        })
        .map_err(|error| BackupError::Git(RemoteError::Process(error)))?;
    if !output.success {
        return Err(BackupError::CorruptSnapshot {
            item: MANIFEST_FILE.to_owned(),
        });
    }
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| BackupError::CorruptSnapshot {
            item: MANIFEST_FILE.to_owned(),
        })?;
        let mut components = Path::new(path).components();
        if components
            .next()
            .is_some_and(|component| component.as_os_str() == STORES_DIR)
            && let Some(std::path::Component::Normal(store_id)) = components.next()
        {
            return Ok(store_id.to_string_lossy().into_owned());
        }
    }
    Err(BackupError::CorruptSnapshot {
        item: MANIFEST_FILE.to_owned(),
    })
}

fn safe_device_component(device: &str) -> String {
    device
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || "-_.".contains(ch) {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn rename_remote_skill_collisions(
    local_files: &BTreeMap<PathBuf, Vec<u8>>,
    remote_files: &mut BTreeMap<PathBuf, Vec<u8>>,
    device: &str,
) -> Result<(), BackupError> {
    let Some(local_bytes) = local_files.get(Path::new(MANIFEST_FILE)) else {
        return Ok(());
    };
    let Some(remote_bytes) = remote_files.get(Path::new(MANIFEST_FILE)) else {
        return Ok(());
    };
    let local: serde_json::Value = serde_json::from_slice(local_bytes)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let mut remote: serde_json::Value = serde_json::from_slice(remote_bytes)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let local_items = local
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let remote_items = remote
        .get_mut("items")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let mut local_ids = std::collections::BTreeSet::new();
    let mut used_slugs = std::collections::BTreeSet::new();
    for item in local_items {
        if item.get("kind").and_then(serde_json::Value::as_str) == Some("skill") {
            if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
                local_ids.insert(id.to_owned());
            }
            if let Some(slug) = item.get("slug").and_then(serde_json::Value::as_str) {
                used_slugs.insert(slug.to_owned());
            }
        }
    }
    for item in remote_items.iter() {
        if item.get("kind").and_then(serde_json::Value::as_str) == Some("skill")
            && let Some(slug) = item.get("slug").and_then(serde_json::Value::as_str)
        {
            used_slugs.insert(slug.to_owned());
        }
    }
    let safe_device: String = safe_device_component(device)
        .to_ascii_lowercase()
        .chars()
        .take(32)
        .collect();
    let mut renames = Vec::new();
    for item in remote_items.iter_mut() {
        if item.get("kind").and_then(serde_json::Value::as_str) != Some("skill") {
            continue;
        }
        let Some(id) = item.get("id").and_then(serde_json::Value::as_str) else {
            return Err(BackupError::InvalidSnapshotPath(PathBuf::from(
                MANIFEST_FILE,
            )));
        };
        let Some(slug) = item
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return Err(BackupError::InvalidSnapshotPath(PathBuf::from(
                MANIFEST_FILE,
            )));
        };
        if local_ids.contains(id)
            || !local_items.iter().any(|local_item| {
                local_item.get("kind").and_then(serde_json::Value::as_str) == Some("skill")
                    && local_item.get("slug").and_then(serde_json::Value::as_str)
                        == Some(slug.as_str())
            })
        {
            continue;
        }
        let suffix = format!("-{safe_device}");
        let base = format!(
            "{}{suffix}",
            slug.chars()
                .take(64usize.saturating_sub(suffix.len()))
                .collect::<String>()
        );
        let renamed = crate::layout::unique_slug(&base, |candidate| used_slugs.contains(candidate))
            .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?
            .to_string();
        used_slugs.insert(renamed.clone());
        let old_prefix = format!("skills/{slug}");
        let new_prefix = format!("skills/{renamed}");
        item["slug"] = serde_json::Value::String(renamed);
        item["path"] = serde_json::Value::String(new_prefix.clone());
        renames.push((old_prefix, new_prefix));
    }
    for (old_prefix, new_prefix) in renames {
        let moved: Vec<_> = remote_files
            .iter()
            .filter_map(|(path, bytes)| {
                let text = path.to_str()?;
                (text == old_prefix || text.starts_with(&format!("{old_prefix}/")))
                    .then(|| (path.clone(), bytes.clone()))
            })
            .collect();
        for (old, bytes) in moved {
            remote_files.remove(&old);
            let text = old
                .to_str()
                .ok_or_else(|| BackupError::InvalidSnapshotPath(old.clone()))?;
            let suffix = text
                .strip_prefix(&old_prefix)
                .ok_or_else(|| BackupError::InvalidSnapshotPath(old.clone()))?;
            remote_files.insert(PathBuf::from(format!("{new_prefix}{suffix}")), bytes);
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&remote)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    bytes.push(b'\n');
    remote_files.insert(PathBuf::from(MANIFEST_FILE), bytes);
    Ok(())
}

fn merge_manifest_bytes(
    local: &[u8],
    remote: &[u8],
    base: Option<&Vec<u8>>,
) -> Result<Vec<u8>, BackupError> {
    let mut local_value: serde_json::Value = serde_json::from_slice(local)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let remote_value: serde_json::Value = serde_json::from_slice(remote)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    let base_value: Option<serde_json::Value> = base
        .map(|bytes| serde_json::from_slice(bytes))
        .transpose()
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    for key in ["items", "disabled"] {
        let local_entries = local_value
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
        let remote_entries = remote_value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
        let identity_key = if key == "items" { "id" } else { "item_id" };
        let base_entries = base_value
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_array);
        if key == "items" {
            let remote_ids: std::collections::BTreeSet<&str> = remote_entries
                .iter()
                .filter_map(|entry| entry.get(identity_key)?.as_str())
                .collect();
            local_entries.retain(|entry| {
                let Some(id) = entry.get(identity_key).and_then(serde_json::Value::as_str) else {
                    return true;
                };
                let base_entry = base_entries.and_then(|entries| {
                    entries.iter().find(|base| {
                        base.get(identity_key).and_then(serde_json::Value::as_str) == Some(id)
                    })
                });
                remote_ids.contains(id) || base_entry != Some(entry)
            });
        }
        let mut known: std::collections::BTreeSet<String> = local_entries
            .iter()
            .filter_map(|entry| entry.get(identity_key)?.as_str().map(str::to_owned))
            .collect();
        for entry in remote_entries {
            let Some(id) = entry.get(identity_key).and_then(serde_json::Value::as_str) else {
                return Err(BackupError::InvalidSnapshotPath(PathBuf::from(
                    MANIFEST_FILE,
                )));
            };
            if known.insert(id.to_owned()) {
                local_entries.push(entry.clone());
            }
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&local_value)
        .map_err(|_| BackupError::InvalidSnapshotPath(PathBuf::from(MANIFEST_FILE)))?;
    bytes.push(b'\n');
    Ok(bytes)
}

struct DirectoryOperationRecorder<'a>(at_fs::changeset::DbOperationRecorder<'a>);

impl at_fs::changeset::OperationRecorder for DirectoryOperationRecorder<'_> {
    fn begin(&mut self, set: &at_model::ChangeSet) -> Result<String, String> {
        at_fs::changeset::OperationRecorder::begin(&mut self.0, set)
    }

    fn file_completed(
        &mut self,
        id: &str,
        path: &Path,
        before: Option<&str>,
        after: Option<&str>,
        backup: Option<&Path>,
    ) -> Result<(), String> {
        if before.is_none() && after.is_none() {
            return Ok(());
        }
        at_fs::changeset::OperationRecorder::file_completed(
            &mut self.0,
            id,
            path,
            before,
            after,
            backup,
        )
    }

    fn finish(&mut self, id: &str, succeeded: bool, manual_review: bool) -> Result<(), String> {
        at_fs::changeset::OperationRecorder::finish(&mut self.0, id, succeeded, manual_review)
    }
}

fn ensure_change_set_directory(
    store: &StoreLayout,
    db: &at_db::Database,
    now: OffsetDateTime,
    relative: &Path,
) -> Result<(), BackupError> {
    let set = at_model::ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: uuid::Uuid::now_v7().to_string(),
        operation_kind: at_model::OperationKind::parse("remote_backup")?,
        scope: Some(store.scope()),
        project_id: None,
        summary: "create remote conflict directory".to_owned(),
        ops: vec![at_model::FileOp::Mkdir {
            path: relative.to_path_buf(),
            mode: Some(0o700),
            parents: true,
        }],
        created_at_unix_seconds: now.unix_timestamp().max(0) as u64,
    };
    let mut recorder = DirectoryOperationRecorder(at_fs::changeset::DbOperationRecorder::new(
        db.connection(),
        30,
    ));
    let mut context = at_fs::changeset::ApplyContext {
        store_root: store.root(),
        backup_folder: &store.backups_dir(),
        recorder: &mut recorder,
    };
    at_fs::changeset::apply(&set, &mut context)?;
    Ok(())
}

fn change_write(
    target: &Path,
    relative: &Path,
    bytes: Vec<u8>,
) -> Result<at_model::FileOp, BackupError> {
    let expected = match fs::read(target) {
        Ok(existing) => at_model::ExpectedState::hash(at_fs::sha256_hex(&existing))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => at_model::ExpectedState::absent(),
        Err(source) => {
            return Err(BackupError::Io {
                path: target.to_path_buf(),
                source,
            });
        }
    };
    Ok(at_model::FileOp::Write {
        path: relative.to_path_buf(),
        bytes,
        expected,
        follow_symlink: false,
        backup: true,
    })
}

fn collect_snapshot(
    root: &Path,
) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>, BackupError> {
    let mut files = std::collections::BTreeMap::new();
    collect_snapshot_dir(root, root, &mut files)?;
    Ok(files)
}

fn collect_snapshot_dir(
    root: &Path,
    directory: &Path,
    files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), BackupError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(BackupError::Io {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| BackupError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name();
        if is_excluded_from_backup(&name.to_string_lossy()) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|source| BackupError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(BackupError::Io {
                path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing symlink in sync snapshot",
                ),
            });
        }
        if metadata.is_dir() {
            collect_snapshot_dir(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BackupError::InvalidSnapshotPath(path.clone()))?
                .to_path_buf();
            files.insert(
                relative,
                fs::read(&path).map_err(|source| BackupError::Io {
                    path: path.clone(),
                    source,
                })?,
            );
        } else {
            return Err(BackupError::Io {
                path,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported sync snapshot entry",
                ),
            });
        }
    }
    Ok(())
}

fn read_commit_snapshot(
    runner: &dyn at_git::local::LocalGitRunner,
    repo: &Path,
    commit: &str,
    store_id: &str,
) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>, BackupError> {
    let prefix = format!("{STORES_DIR}/{store_id}");
    let args = [
        "ls-tree",
        "-r",
        "-z",
        "--name-only",
        commit,
        "--",
        prefix.as_str(),
    ];
    let output = runner
        .run(&at_git::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: true,
        })
        .map_err(|error| BackupError::Git(RemoteError::Process(error)))?;
    if !output.success {
        return Err(BackupError::Git(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |value| value.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(512)
                .collect(),
        }));
    }
    let mut files = std::collections::BTreeMap::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let full = std::str::from_utf8(raw)
            .map_err(|_| BackupError::InvalidSnapshotPath(repo.to_path_buf()))?;
        let rel = full
            .strip_prefix(&format!("{prefix}/"))
            .ok_or_else(|| BackupError::InvalidSnapshotPath(PathBuf::from(full)))?;
        let path = Path::new(rel);
        validate_snapshot_path(path)?;
        let spec = format!("{commit}:{full}");
        let args = ["show", spec.as_str()];
        let result = runner
            .run(&at_git::local::LocalGitCall {
                repo,
                args: &args,
                stdin: None,
                literal_pathspecs: true,
            })
            .map_err(|error| BackupError::Git(RemoteError::Process(error)))?;
        if !result.success {
            return Err(BackupError::Git(RemoteError::CommandFailed {
                args: "show <snapshot-file>".to_owned(),
                code: result
                    .code
                    .map_or_else(|| "signal".to_owned(), |value| value.to_string()),
                stderr: String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .take(512)
                    .collect(),
            }));
        }
        files.insert(path.to_path_buf(), result.stdout);
    }
    Ok(files)
}

fn validate_snapshot_path(path: &Path) -> Result<(), BackupError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(BackupError::InvalidSnapshotPath(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use at_db::remote_backup::{RemoteBackupState, save_remote_backup_state};
    use at_git::local::LocalGitRunner;
    use at_git::local::{LocalGitCall, ProcessLocalGitRunner};
    use at_git::remote::{GitAuth, ProcessRemoteGitRunner};
    use at_secrets::{MemoryStore, save_token};
    use tempfile::TempDir;

    #[test]
    fn divergent_remote_edit_is_preserved_and_second_sync_is_noop() {
        let sandbox = TempDir::new().expect("sandbox");
        let bare = sandbox.path().join("remote.git");
        fs::create_dir_all(&bare).expect("bare directory");
        let local = ProcessLocalGitRunner::default();
        let init = local
            .run(&LocalGitCall {
                repo: &bare,
                args: &["init", "--bare", "-b", "main"],
                stdin: None,
                literal_pathspecs: true,
            })
            .expect("initialize bare remote");
        assert!(init.success);
        let remote_url = format!("file://localhost{}", bare.display());
        let store = StoreLayout::new(at_model::Scope::User, sandbox.path().join("store"));
        store.init().expect("initialize store");
        let skill = store.skills_dir().join("sample").join("SKILL.md");
        fs::create_dir_all(skill.parent().expect("skill parent")).expect("skill directory");
        fs::write(&skill, b"base\n").expect("seed skill");
        let locally_modified_deleted = store.skills_dir().join("modified-deleted/SKILL.md");
        fs::create_dir_all(
            locally_modified_deleted
                .parent()
                .expect("modified-deleted parent"),
        )
        .expect("modified-deleted directory");
        fs::write(&locally_modified_deleted, b"base version\n").expect("seed deleted item");

        let mut db = at_db::Database::open_in_memory().expect("database");
        set_device_name(&mut db, Some("laptop")).expect("device name");
        save_remote_backup_state(
            db.connection(),
            &RemoteBackupState {
                repo_url: Some(remote_url.clone()),
                branch: Some("main".to_owned()),
                ..RemoteBackupState::default()
            },
        )
        .expect("remote state");
        let secrets = MemoryStore::new();
        let fake_token = concat!("ghp_", "0123456789abcdef", "0123456789abcdef");
        save_token(&secrets, fake_token).expect("fake token");
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::default();
        let remote = ProcessRemoteGitRunner::default();
        let redactor = Redactor::new();
        let now = OffsetDateTime::now_utc();
        {
            let mut request = BackupRequest {
                store: &store,
                db: &db,
                secrets: &secrets,
                policy: &policy,
                log: &mut log,
                remote_runner: &remote,
                local_runner: &local,
                redactor: &redactor,
                now,
            };
            let first = push_backup(&mut request).expect("initial backup");
            assert!(first.pushed);
        }

        let remote_work = sandbox.path().join("remote-work");
        let auth = GitAuth::none();
        {
            let mut context = RemoteContext {
                policy: &policy,
                log: &mut log,
                runner: &remote,
                local: &local,
                redact: &|text| text.to_owned(),
                now,
            };
            context
                .clone_remote(&remote_url, &remote_work, &auth)
                .expect("clone for second device");
            let store_id = store.read_header().expect("header").store_id.to_string();
            let remote_skill = remote_work
                .join(STORES_DIR)
                .join(&store_id)
                .join("skills/sample/SKILL.md");
            fs::write(&remote_skill, b"remote edit\n").expect("remote edit");
            let remote_only = remote_work
                .join(STORES_DIR)
                .join(&store_id)
                .join("skills/remote-only/SKILL.md");
            fs::create_dir_all(remote_only.parent().expect("remote-only parent"))
                .expect("remote-only directory");
            fs::write(&remote_only, b"remote only\n").expect("remote-only content");
            let remote_deleted = remote_work
                .join(STORES_DIR)
                .join(&store_id)
                .join("skills/modified-deleted/SKILL.md");
            fs::remove_file(&remote_deleted).expect("remote deletion of modified item");
            at_git::remote::stage_all(&local, &remote_work).expect("stage remote edit");
            at_git::remote::commit_local(&local, &remote_work, "second device edit")
                .expect("commit remote edit");
            context
                .push_remote(&remote_work, &remote_url, "main", &auth)
                .expect("push remote edit");
        }
        {
            let mut request = BackupRequest {
                store: &store,
                db: &db,
                secrets: &secrets,
                policy: &policy,
                log: &mut log,
                remote_runner: &remote,
                local_runner: &local,
                redactor: &redactor,
                now,
            };
            let local_only = store.skills_dir().join("local-only").join("SKILL.md");
            fs::create_dir_all(local_only.parent().expect("local-only parent"))
                .expect("local-only directory");
            fs::write(&local_only, b"local only\n").expect("local-only content");
            fs::write(
                &locally_modified_deleted,
                b"local edit after remote deletion\n",
            )
            .expect("modify remote-deleted item locally");
            fs::write(&skill, b"local edit\n").expect("local edit");
            let sync = sync_from_remote(&mut request, false).expect("merge divergent edits");
            assert_eq!(
                fs::read(&locally_modified_deleted).expect("locally modified deletion retained"),
                b"local edit after remote deletion\n"
            );
            assert_eq!(
                fs::read(store.skills_dir().join("remote-only/SKILL.md"))
                    .expect("remote-only merged"),
                b"remote only\n"
            );
            assert_eq!(
                fs::read(&local_only).expect("local-only retained"),
                b"local only\n"
            );
            assert_eq!(sync.conflicts.len(), 1);
            assert!(sync.pushed);
            assert_eq!(fs::read(&skill).expect("local copy"), b"local edit\n");
            assert_eq!(remote_conflict_count(&store).expect("conflict count"), 1);
            let conflict_root = store.state_dir().join("remote-conflicts");
            let preserved = fs::read_dir(conflict_root)
                .expect("device conflict folder")
                .flat_map(|entry| {
                    fs::read_dir(entry.expect("device entry").path()).expect("conflict entries")
                })
                .flat_map(|entry| {
                    let path = entry
                        .expect("conflict folder")
                        .path()
                        .join("skills/sample/SKILL.md");
                    std::iter::once(path)
                })
                .find(|path| path.is_file())
                .expect("preserved remote file");
            assert_eq!(fs::read(preserved).expect("remote copy"), b"remote edit\n");

            let second = sync_from_remote(&mut request, false).expect("idempotent sync");
            assert!(!second.committed);
            assert_eq!(remote_conflict_count(&store).expect("stable conflicts"), 1);
        }
        let deletion_work = sandbox.path().join("deletion-work");
        {
            let mut context = RemoteContext {
                policy: &policy,
                log: &mut log,
                runner: &remote,
                local: &local,
                redact: &|text| text.to_owned(),
                now,
            };
            context
                .clone_remote(&remote_url, &deletion_work, &auth)
                .expect("clone for remote deletion");
            let deleted_remote_path = deletion_work
                .join(STORES_DIR)
                .join(store.read_header().expect("header").store_id.to_string())
                .join("skills/sample/SKILL.md");
            fs::remove_file(&deleted_remote_path).expect("delete remote skill");
            at_git::remote::stage_all(&local, &deletion_work).expect("stage deletion");
            at_git::remote::commit_local(&local, &deletion_work, "delete remote skill")
                .expect("commit deletion");
            context
                .push_remote(&deletion_work, &remote_url, "main", &auth)
                .expect("push deletion");
        }
        let mut request = BackupRequest {
            store: &store,
            db: &db,
            secrets: &secrets,
            policy: &policy,
            log: &mut log,
            remote_runner: &remote,
            local_runner: &local,
            redactor: &redactor,
            now,
        };
        assert!(matches!(
            sync_from_remote(&mut request, false),
            Err(BackupError::DeletionConfirmationRequired)
        ));
        sync_from_remote(&mut request, true).expect("confirmed deletion");
        assert!(!skill.exists());
    }

    #[test]
    fn remote_add_with_skill_slug_collision_gets_device_suffix() {
        let item = |id: &str| {
            serde_json::json!({
                "id": id,
                "kind": "skill",
                "slug": "sample",
                "path": "skills/sample",
                "hash": "0".repeat(64)
            })
        };
        let local_manifest = serde_json::json!({
            "schema_version": 1,
            "store_id": "store",
            "scope": "user",
            "updated_at": "2026-01-01T00:00:00Z",
            "items": [item("itm_local")],
            "disabled": []
        });
        let remote_manifest = serde_json::json!({
            "schema_version": 1,
            "store_id": "store",
            "scope": "user",
            "updated_at": "2026-01-01T00:00:00Z",
            "items": [item("itm_remote")],
            "disabled": []
        });
        let mut local = BTreeMap::new();
        local.insert(
            PathBuf::from(MANIFEST_FILE),
            serde_json::to_vec(&local_manifest).expect("local manifest"),
        );
        let mut remote = BTreeMap::new();
        remote.insert(
            PathBuf::from(MANIFEST_FILE),
            serde_json::to_vec(&remote_manifest).expect("remote manifest"),
        );
        remote.insert(PathBuf::from("skills/sample/SKILL.md"), b"remote".to_vec());
        rename_remote_skill_collisions(&local, &mut remote, "Laptop").expect("rename collision");
        assert!(remote.contains_key(Path::new("skills/sample-laptop/SKILL.md")));
        assert!(!remote.contains_key(Path::new("skills/sample/SKILL.md")));
        let merged = merge_manifest_bytes(
            local.get(Path::new(MANIFEST_FILE)).expect("local bytes"),
            remote.get(Path::new(MANIFEST_FILE)).expect("remote bytes"),
            None,
        )
        .expect("merge manifest");
        let parsed: serde_json::Value = serde_json::from_slice(&merged).expect("merged JSON");
        let remote_item = parsed["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|entry| entry["id"] == "itm_remote")
            .expect("remote item");
        assert_eq!(remote_item["slug"], "sample-laptop");
    }

    #[test]
    fn conflict_count_counts_each_timestamped_sync_folder() {
        let sandbox = TempDir::new().expect("sandbox");
        let store = StoreLayout::new(at_model::Scope::User, sandbox.path().join("store"));
        store.init().expect("initialize store");
        let conflict_root = store.state_dir().join("remote-conflicts");
        fs::create_dir_all(conflict_root.join("laptop/first")).expect("first conflict");
        fs::create_dir_all(conflict_root.join("laptop/second")).expect("second conflict");
        fs::create_dir_all(conflict_root.join("desktop/third")).expect("third conflict");
        fs::write(
            conflict_root.join("laptop/first/.agentuna-resolved"),
            b"resolved\n",
        )
        .expect("resolved marker");
        assert_eq!(remote_conflict_count(&store).expect("count folders"), 2);
    }
    #[test]
    fn restore_from_bare_remote_into_fresh_store_and_refuse_nonempty_target() {
        let sandbox = TempDir::new().expect("sandbox");
        let bare = sandbox.path().join("restore-remote.git");
        fs::create_dir_all(&bare).expect("remote folder");
        let local = ProcessLocalGitRunner::default();
        let initialized = local
            .run(&LocalGitCall {
                repo: &bare,
                args: &["init", "--bare", "-b", "main"],
                stdin: None,
                literal_pathspecs: true,
            })
            .expect("initialize bare remote");
        assert!(initialized.success);
        let remote_url = format!("file://localhost{}", bare.display());
        let source = StoreLayout::new(at_model::Scope::User, sandbox.path().join("source"));
        source.init().expect("source store");
        let skill_file = source.skills_dir().join("echo/SKILL.md");
        fs::create_dir_all(skill_file.parent().expect("skill parent")).expect("skill dir");
        fs::write(
            &skill_file,
            "---\nname: Echo\ndescription: fixture\n---\nBody\n",
        )
        .expect("skill content");
        let loaded = StoreManifest::load(&source).expect("load source manifest");
        let mut manifest = loaded.manifest;
        let slug = crate::layout::Slug::parse("echo").expect("slug");
        manifest
            .insert_item(crate::manifest::ManifestItem::new(
                at_model::ItemId::new(),
                at_model::ItemKind::Skill,
                slug,
                crate::manifest::StorePath::parse("skills/echo").expect("item path"),
                crate::manifest::hash_skill_folder(&source.skills_dir().join("echo"))
                    .expect("content hash"),
            ))
            .expect("manifest item");
        manifest
            .save(
                &source,
                at_fs::ExpectedState::Hash(loaded.hash.to_string()),
                at_fs::BackupPolicy::None,
            )
            .expect("save manifest");
        let mut database = at_db::Database::open_in_memory().expect("database");
        // The default device name comes from the host name, which CI runners may not make valid.
        set_device_name(&mut database, Some("fixture")).expect("device name");
        save_remote_backup_state(
            database.connection(),
            &RemoteBackupState {
                repo_url: Some(remote_url.clone()),
                branch: Some("main".to_owned()),
                ..RemoteBackupState::default()
            },
        )
        .expect("remote config");
        let secrets = MemoryStore::new();
        save_token(
            &secrets,
            concat!("ghp_", "0123456789abcdef", "0123456789abcdef"),
        )
        .expect("fixture token");
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::default();
        let remote = ProcessRemoteGitRunner::default();
        let redactor = Redactor::new();
        let now = OffsetDateTime::now_utc();
        let commit = {
            let mut request = BackupRequest {
                store: &source,
                db: &database,
                secrets: &secrets,
                policy: &policy,
                log: &mut log,
                remote_runner: &remote,
                local_runner: &local,
                redactor: &redactor,
                now,
            };
            push_backup(&mut request)
                .expect("push fixture backup")
                .commit
                .expect("backup commit")
        };
        let destination = StoreLayout::new(at_model::Scope::User, sandbox.path().join("fresh"));
        destination.init().expect("fresh store");
        let before_offline = fs::read(destination.manifest_path()).expect("empty manifest");
        let offline_policy = policy.clone().with_offline(true);
        let mut offline_log = NetworkEventLog::default();
        {
            let mut offline_request = BackupRequest {
                store: &destination,
                db: &database,
                secrets: &secrets,
                policy: &offline_policy,
                log: &mut offline_log,
                remote_runner: &remote,
                local_runner: &local,
                redactor: &redactor,
                now,
            };
            assert!(restore_from_remote(&mut offline_request, &commit, false).is_err());
            assert_eq!(
                fs::read(destination.manifest_path()).expect("unchanged empty manifest"),
                before_offline
            );
        }
        let mut request = BackupRequest {
            store: &destination,
            db: &database,
            secrets: &secrets,
            policy: &policy,
            log: &mut log,
            remote_runner: &remote,
            local_runner: &local,
            redactor: &redactor,
            now,
        };
        restore_from_remote(&mut request, &commit, false).expect("restore snapshot");
        assert_eq!(
            fs::read(destination.skills_dir().join("echo/SKILL.md")).expect("restored skill"),
            fs::read(skill_file).expect("source skill")
        );
        assert_eq!(
            at_db::remote_backup::remote_backup_state(database.connection())
                .expect("remote state")
                .last_commit
                .as_deref(),
            Some(commit.as_str())
        );
        assert!(matches!(
            restore_from_remote(&mut request, &commit, false),
            Err(BackupError::NonEmptyRestore)
        ));
        let corrupted_work = sandbox.path().join("corrupt-writer");
        let corrupt_commit = {
            let auth = GitAuth::none();
            let mut context = RemoteContext {
                policy: &policy,
                log: &mut log,
                runner: &remote,
                local: &local,
                redact: &|text| text.to_owned(),
                now,
            };
            context
                .clone_remote(&remote_url, &corrupted_work, &auth)
                .expect("clone remote for corrupt fixture");
            let store_id = source
                .read_header()
                .expect("source header")
                .store_id
                .to_string();
            let corrupt_skill = corrupted_work
                .join(STORES_DIR)
                .join(store_id)
                .join("skills/echo/SKILL.md");
            fs::write(&corrupt_skill, b"corrupted snapshot\n").expect("corrupt skill");
            at_git::remote::stage_all(&local, &corrupted_work).expect("stage corruption");
            let commit = at_git::remote::commit_local(&local, &corrupted_work, "corrupt fixture")
                .expect("commit corruption");
            context
                .push_remote(&corrupted_work, &remote_url, "main", &auth)
                .expect("push corrupt fixture");
            commit
        };
        let corrupt_destination =
            StoreLayout::new(at_model::Scope::User, sandbox.path().join("corrupt-target"));
        corrupt_destination.init().expect("corrupt target store");
        let corrupt_empty = fs::read(corrupt_destination.manifest_path()).expect("empty manifest");
        let mut corrupt_request = BackupRequest {
            store: &corrupt_destination,
            db: &database,
            secrets: &secrets,
            policy: &policy,
            log: &mut log,
            remote_runner: &remote,
            local_runner: &local,
            redactor: &redactor,
            now,
        };
        assert!(matches!(
            restore_from_remote(&mut corrupt_request, &corrupt_commit, false),
            Err(BackupError::CorruptSnapshot { .. })
        ));
        assert_eq!(
            fs::read(corrupt_destination.manifest_path()).expect("still empty"),
            corrupt_empty
        );
    }
}
