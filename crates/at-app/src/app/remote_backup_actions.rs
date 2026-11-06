//! Explicit, cancellable remote-backup actions and conflict review commands.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use at_db::Database;
use at_git::local::{LocalGitCall, LocalGitRunner, ProcessLocalGitRunner};
use at_git::remote::{GitAuth, ProcessRemoteGitRunner, RemoteContext, RemoteGitRunner};
use at_model::change_set::{ChangeSet, ExpectedState, FileOp, OperationKind};
use at_model::network::{
    DenialReason, NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger,
};
use at_model::redact::Redactor;
use at_secrets::SecretStore;
use at_store::layout::StoreLayout;
use serde::{Deserialize, Serialize};
use tauri::State;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use ts_rs::TS;

use super::super::bridge::{JobError, JobId, JobSpec};
use super::super::commands::{CommandError, CommandErrorCode};
use super::super::network::NetworkState;
use super::super::state::AppState;

const MAX_CONFLICT_GROUPS: usize = 1_000;
const MAX_CONFLICT_FILES: usize = 10_000;
const MAX_CONFLICT_DEPTH: usize = 32;
const RESTORE_CONFIRMATION_TTL: Duration = Duration::from_secs(5 * 60);

/// Stable failure class for remote-backup commands and job events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum RemoteBackupFailureCode {
    /// No repository has been configured.
    RepositoryNotConfigured,
    /// A GitHub credential is absent.
    TokenMissing,
    /// The stored GitHub credential was rejected.
    TokenRejected,
    /// Offline mode prevents the request.
    Offline,
    /// A network allow-list or policy rule refused the request.
    NetworkDenied,
    /// The remote moved ahead and must be synchronized first.
    RemoteConflict,
    /// Remote deletion needs a separate explicit confirmation.
    ConfirmDeletions,
    /// Restore target contains existing managed or unmanaged content.
    NonEmptyTarget,
    /// A reviewed conflict plan became stale.
    StaleReview,
    /// A store or remote snapshot could not be safely read.
    InvalidStore,
    /// Another remote-backup action failed.
    ActionFailed,
}

/// Actionable remedy associated with a typed remote-backup failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum RemoteBackupRemedy {
    /// Configure or choose a repository in Preferences.
    ConfigureRepository,
    /// Save a GitHub token in Preferences.
    SaveToken,
    /// Replace the rejected token in Preferences.
    ReplaceToken,
    /// Turn off offline mode and retry explicitly.
    TurnNetworkOn,
    /// Review network policy and host permissions.
    ReviewNetworkPolicy,
    /// Synchronize remote changes before pushing.
    SyncFirst,
    /// Confirm remote deletions and repeat the sync.
    ConfirmDeletions,
    /// Choose an empty store for restoration.
    ChooseEmptyStore,
    /// Reopen the conflict and review current copies.
    ReviewAgain,
    /// Retry the explicit action after checking the connection.
    Retry,
}

/// Secret-free remote-backup failure with a typed next step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{message}")]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupFailure {
    /// Stable category used by the UI.
    pub code: RemoteBackupFailureCode,
    /// Human-readable explanation that never contains credential material.
    pub message: String,
    /// Action the UI should offer next.
    pub remedy: RemoteBackupRemedy,
}

fn failure(
    code: RemoteBackupFailureCode,
    message: &'static str,
    remedy: RemoteBackupRemedy,
) -> RemoteBackupFailure {
    RemoteBackupFailure {
        code,
        message: message.to_owned(),
        remedy,
    }
}

/// Current remote-backup state, including derived conflict and offline information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupStatusView {
    /// Configured repository URL, if one is saved.
    pub repo_url: Option<String>,
    /// Device name written to commit metadata.
    pub device_name: String,
    /// Configured remote branch.
    pub branch: Option<String>,
    /// Time of the most recent successful push.
    pub last_push_at: Option<String>,
    /// Time of the most recent successful pull or sync.
    pub last_sync_at: Option<String>,
    /// Commit identifier of the most recent successful operation.
    pub last_commit: Option<String>,
    /// Number of unresolved local conflict folders.
    pub conflict_count: usize,
    /// Whether the central network policy currently blocks all requests.
    pub offline: bool,
    /// Last persisted operation status.
    pub status: String,
    /// Stable code for the last persisted failure.
    pub error_kind: Option<String>,
}

/// One commit available to restore from the configured remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupCommitView {
    /// Full commit object identifier.
    pub commit: String,
    /// RFC 3339 commit timestamp.
    pub committed_at: String,
    /// Redacted commit subject line.
    pub message: String,
}

/// One explicit, one-use grant to restore a commit into an empty store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConfirmationView {
    /// One-use token required by the Restore command.
    pub confirmation_token: String,
    /// Device name that identifies the local target.
    pub device_name: String,
    /// Repository that supplies the selected commit.
    pub repo_url: String,
    /// Commit the grant is bound to.
    pub commit: String,
}

/// One unresolved remote-backup sync conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConflictGroupView {
    /// Safe identifier formed from device and timestamp folder names.
    pub id: String,
    /// Device name associated with this sync.
    pub device_name: String,
    /// Timestamp folder name for this sync.
    pub timestamp: String,
    /// Files with a local copy and its preserved remote copy.
    pub files: Vec<RemoteBackupConflictFileView>,
}

/// Local and preserved remote paths for one conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConflictFileView {
    /// Store-relative path that has the local copy.
    pub path: String,
    /// Absolute path of the current local copy.
    pub local_path: String,
    /// Absolute path of the preserved remote copy under `.state/remote-conflicts`.
    pub remote_path: String,
    /// Whether the local copy still exists.
    pub local_exists: bool,
    /// Size of the preserved remote copy in bytes.
    pub remote_size: u64,
    /// Size of the current local copy in bytes, if it exists.
    pub local_size: Option<u64>,
}

/// One file effect in a reviewed conflict-resolution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConflictChangeView {
    /// Store-relative path affected by the plan.
    pub path: String,
    /// Planned operation (`write` or `delete`).
    pub action: String,
    /// Redacted unified diff, or a safe binary/large-file summary.
    pub diff: String,
    /// Existing file digest, if present.
    pub before_hash: Option<String>,
    /// New file digest, if the plan writes content.
    pub after_hash: Option<String>,
    /// Whether a replaced file is backed up by `at-fs`.
    pub backup_required: bool,
}

/// Reviewed conflict resolution; applying it requires this exact preview id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConflictPreviewView {
    /// Single-use identifier for the cached exact ChangeSet.
    pub preview_id: String,
    /// Conflict path and the chosen non-silent resolution.
    pub summary: String,
    /// Exact file changes shown to the user before apply.
    pub files: Vec<RemoteBackupConflictChangeView>,
    /// Where the remote copy will be archived for Keep both.
    pub archived_remote_path: Option<String>,
}

/// Result of applying one reviewed conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupConflictApplyView {
    /// Durable operation-history identifier.
    pub operation_id: String,
    /// Number of file operations applied.
    pub files_changed: usize,
}

// The variants mirror the three user-facing choices (`keep_local`, `keep_remote`, `keep_both`).
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictResolution {
    KeepLocal,
    KeepRemote,
    KeepBoth,
}

impl ConflictResolution {
    fn parse(text: &str) -> Result<Self, CommandError> {
        match text {
            "keep_local" => Ok(Self::KeepLocal),
            "keep_remote" => Ok(Self::KeepRemote),
            "keep_both" => Ok(Self::KeepBoth),
            _ => Err(CommandError::invalid_argument(
                "resolution",
                "choose keep_local, keep_remote or keep_both",
            )),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::KeepLocal => "keep local",
            Self::KeepRemote => "keep remote",
            Self::KeepBoth => "keep both",
        }
    }
}

struct RemoteBackupSetup {
    database: Arc<Mutex<Database>>,
    store: StoreLayout,
    repo_url: String,
    device_name: String,
    policy: NetworkPolicy,
}

enum RemoteBackupOperation {
    Backup,
    Sync { confirm_deletions: bool },
    Restore { commit: String },
}

struct CachedConflictPlan {
    store_root: PathBuf,
    change_set: ChangeSet,
    preview: at_engine::preview::Preview,
    view: RemoteBackupConflictPreviewView,
}

struct RestoreGrant {
    commit: String,
    repo_url: String,
    store_root: PathBuf,
    expires: Instant,
}

static CONFLICT_PLANS: LazyLock<Mutex<BTreeMap<String, CachedConflictPlan>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static RESTORE_GRANTS: LazyLock<Mutex<BTreeMap<String, RestoreGrant>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Returns the current remote-backup state without making a network request.
#[tauri::command]
pub fn status(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
) -> Result<RemoteBackupStatusView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let store = store_for_state(&state, &database)?;
    let remote = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|_| CommandError::internal("remote backup status is unavailable"))?;
    let device_name = at_store::remote_backup::device_name(&database)
        .map_err(|_| CommandError::internal("remote backup device name is unavailable"))?;
    let conflict_count = at_store::remote_backup::remote_conflict_count(&store)
        .map_err(|_| CommandError::internal("remote conflict count is unavailable"))?;
    Ok(RemoteBackupStatusView {
        repo_url: remote.repo_url,
        device_name,
        branch: remote.branch,
        last_push_at: remote.last_push_at,
        last_sync_at: remote.last_pull_at,
        last_commit: remote.last_commit,
        conflict_count,
        offline: network.policy().offline(),
        status: remote.status.as_str().to_owned(),
        error_kind: remote.error_kind,
    })
}

/// Starts an explicit, cancellable backup push and returns its bridge job id.
#[tauri::command]
pub fn backup_now(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
) -> Result<JobId, RemoteBackupFailure> {
    let setup = prepare_setup(&state, &network, NetworkAction::RemoteBackupPush)?;
    start_remote_job(
        &state,
        &network,
        setup,
        RemoteBackupOperation::Backup,
        "Backup remote store",
    )
}

/// Starts an explicit, cancellable pull/merge/push; deletions need a second user confirmation.
#[tauri::command]
pub fn sync_now(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    confirm_deletions: bool,
) -> Result<JobId, RemoteBackupFailure> {
    let setup = prepare_setup(&state, &network, NetworkAction::RemoteBackupPull)?;
    start_remote_job(
        &state,
        &network,
        setup,
        RemoteBackupOperation::Sync { confirm_deletions },
        "Sync remote backup",
    )
}

/// Lists recent commits from the configured repository for the explicit Restore flow.
#[tauri::command]
pub async fn restore_commits(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
) -> Result<Vec<RemoteBackupCommitView>, RemoteBackupFailure> {
    let setup = prepare_setup(&state, &network, NetworkAction::RemoteBackupPull)?;
    let network = network.inner().clone();
    tokio::task::spawn_blocking(move || list_remote_commits(setup, network.clone()))
        .await
        .map_err(|_| {
            failure(
                RemoteBackupFailureCode::ActionFailed,
                "Could not read the remote commit list.",
                RemoteBackupRemedy::Retry,
            )
        })?
}

/// Creates a short-lived one-use confirmation token for one commit and an empty store.
#[tauri::command]
pub fn restore_confirmation(
    state: State<'_, AppState>,
    commit: String,
) -> Result<RemoteBackupConfirmationView, RemoteBackupFailure> {
    validate_commit(&commit)?;
    let database = state.database().map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The remote-backup database is unavailable.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    let store = store_for_state(&state, &database).map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The target store is unavailable.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    store_is_empty_target(&store)?;
    let remote =
        at_db::remote_backup::remote_backup_state(database.connection()).map_err(|_| {
            failure(
                RemoteBackupFailureCode::InvalidStore,
                "Remote backup settings are unavailable.",
                RemoteBackupRemedy::Retry,
            )
        })?;
    let repo_url = checked_repo_url(remote.repo_url.as_deref()).map_err(|_| {
        failure(
            RemoteBackupFailureCode::RepositoryNotConfigured,
            "Choose a valid repository in Preferences before restoring.",
            RemoteBackupRemedy::ConfigureRepository,
        )
    })?;
    let device_name = at_store::remote_backup::device_name(&database).map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The target device name is unavailable.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    let store_root = canonical_store_root(&store).map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The target store path is unavailable.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    let confirmation_token = issue_restore_grant(&commit, &repo_url, &store_root);
    Ok(RemoteBackupConfirmationView {
        confirmation_token,
        device_name,
        repo_url,
        commit,
    })
}

/// Restores the selected commit after consuming its explicit confirmation token.
#[tauri::command]
pub fn restore(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    commit: String,
    confirmation_token: String,
) -> Result<JobId, RemoteBackupFailure> {
    validate_commit(&commit)?;
    let setup = prepare_setup(&state, &network, NetworkAction::RemoteBackupPull)?;
    consume_restore_grant(
        &confirmation_token,
        &commit,
        &setup.repo_url,
        setup.store.root(),
    )?;
    store_is_empty_target(&setup.store)?;
    start_remote_job(
        &state,
        &network,
        setup,
        RemoteBackupOperation::Restore { commit },
        "Restore remote backup",
    )
}

/// Lists both sides of every unresolved conflict in the local `.state/remote-conflicts` tree.
#[tauri::command]
pub fn conflicts_list(
    state: State<'_, AppState>,
) -> Result<Vec<RemoteBackupConflictGroupView>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let store = store_for_state(&state, &database)?;
    list_conflicts(&store)
}

/// Reveals one listed local or preserved remote conflict copy in the system file manager.
#[tauri::command]
pub async fn conflicts_reveal(
    state: State<'_, AppState>,
    group_id: String,
    relative_path: String,
    side: String,
) -> Result<(), CommandError> {
    // Resolve the path in a scope of its own: the database guard must not be held across the
    // await below, or the command future is not `Send`.
    let path = {
        let database = state.database().map_err(CommandError::unavailable)?;
        let store = store_for_state(&state, &database)?;
        conflict_side_path(&store, &group_id, Path::new(&relative_path), &side)?
    };
    tokio::task::spawn_blocking(move || reveal_path(&path))
        .await
        .map_err(|_| CommandError::internal("file-manager request failed"))?
}

/// Builds a read-only diff preview for one explicit conflict resolution.
#[tauri::command]
pub fn conflicts_resolve_preview(
    state: State<'_, AppState>,
    group_id: String,
    relative_path: String,
    resolution: String,
) -> Result<RemoteBackupConflictPreviewView, CommandError> {
    let choice = ConflictResolution::parse(&resolution)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let store = store_for_state(&state, &database)?;
    let secrets = at_secrets::KeychainStore::new();
    let plan = build_conflict_plan(
        &store,
        &secrets,
        &group_id,
        Path::new(&relative_path),
        choice,
    )?;
    let preview_id = plan.view.preview_id.clone();
    let view = plan.view.clone();
    let mut plans = lock(&CONFLICT_PLANS);
    plans.insert(preview_id, plan);
    while plans.len() > 128 {
        if let Some(oldest) = plans.keys().next().cloned() {
            plans.remove(&oldest);
        }
    }
    Ok(view)
}

/// Applies exactly one reviewed ChangeSet after verifying its preview is still current.
#[tauri::command]
pub fn conflicts_resolve_apply(
    state: State<'_, AppState>,
    preview_id: String,
) -> Result<RemoteBackupConflictApplyView, CommandError> {
    let plan = lock(&CONFLICT_PLANS).remove(&preview_id).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "This conflict review expired; reopen the conflict and review current copies.",
        )
    })?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let store = store_for_state(&state, &database)?;
    if store.root() != plan.store_root {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The target store changed; reopen the conflict and review current copies.",
        ));
    }
    let change_set = plan.change_set.clone();
    let outcome = apply_conflict_plan(&store, &mut database, plan)?;
    state.record_writes(&change_set, store.root());
    Ok(RemoteBackupConflictApplyView {
        operation_id: outcome.operation_id,
        files_changed: outcome.completed,
    })
}

fn store_for_state(state: &AppState, database: &Database) -> Result<StoreLayout, CommandError> {
    let mut paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application store paths are unavailable"))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("application store settings are unavailable"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    StoreLayout::for_scope(&paths, at_model::Scope::User)
        .map_err(|_| CommandError::unavailable("the User store is unavailable"))
}

fn canonical_store_root(store: &StoreLayout) -> io::Result<PathBuf> {
    fs::canonicalize(store.root())
}

fn canonical_store(store: &StoreLayout) -> Result<StoreLayout, RemoteBackupFailure> {
    let root = canonical_store_root(store).map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The User store is unavailable; initialize it before remote backup.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    Ok(StoreLayout::new(store.scope(), root))
}

fn checked_repo_url(repo_url: Option<&str>) -> Result<String, RemoteBackupFailure> {
    let Some(repo_url) = repo_url else {
        return Err(failure(
            RemoteBackupFailureCode::RepositoryNotConfigured,
            "Choose a GitHub repository in Preferences before using remote backup.",
            RemoteBackupRemedy::ConfigureRepository,
        ));
    };
    at_git::remote::validate_repo_url(repo_url).map_err(|_| {
        failure(
            RemoteBackupFailureCode::RepositoryNotConfigured,
            "The saved repository URL is invalid; update it in Preferences.",
            RemoteBackupRemedy::ConfigureRepository,
        )
    })?;
    if let Ok(parsed) = url::Url::parse(repo_url)
        && (!parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some())
    {
        return Err(failure(
            RemoteBackupFailureCode::RepositoryNotConfigured,
            "The repository URL must not contain credentials, a query or a fragment.",
            RemoteBackupRemedy::ConfigureRepository,
        ));
    }
    Ok(repo_url.to_owned())
}

fn prepare_setup(
    state: &AppState,
    network: &NetworkState,
    action: NetworkAction,
) -> Result<RemoteBackupSetup, RemoteBackupFailure> {
    let database = state.database_handle().map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The remote-backup database is unavailable.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    let (store, repo_url, device_name) = {
        let database_guard = database.lock().map_err(|_| {
            failure(
                RemoteBackupFailureCode::InvalidStore,
                "The remote-backup database is unavailable.",
                RemoteBackupRemedy::Retry,
            )
        })?;
        let store = store_for_state(state, &database_guard).map_err(|_| {
            failure(
                RemoteBackupFailureCode::InvalidStore,
                "The User store is unavailable.",
                RemoteBackupRemedy::Retry,
            )
        })?;
        let store = canonical_store(&store)?;
        let remote = at_db::remote_backup::remote_backup_state(database_guard.connection())
            .map_err(|_| {
                failure(
                    RemoteBackupFailureCode::InvalidStore,
                    "Remote backup settings are unavailable.",
                    RemoteBackupRemedy::Retry,
                )
            })?;
        let repo_url = checked_repo_url(remote.repo_url.as_deref())?;
        let device_name = at_store::remote_backup::device_name(&database_guard).map_err(|_| {
            failure(
                RemoteBackupFailureCode::InvalidStore,
                "The remote-backup device name is unavailable.",
                RemoteBackupRemedy::Retry,
            )
        })?;
        if !store.manifest_path().is_file() {
            return Err(failure(
                RemoteBackupFailureCode::InvalidStore,
                "The User store has no valid store manifest; initialize it before backup.",
                RemoteBackupRemedy::Retry,
            ));
        }
        (store, repo_url, device_name)
    };
    let host = network_host(&repo_url).ok_or_else(|| {
        failure(
            RemoteBackupFailureCode::NetworkDenied,
            "The remote repository has no valid network host.",
            RemoteBackupRemedy::ReviewNetworkPolicy,
        )
    })?;
    network
        .authorize(action, &host, NetworkTrigger::User)
        .map_err(|denied| network_failure(&denied))?;
    Ok(RemoteBackupSetup {
        database,
        store,
        repo_url,
        device_name,
        policy: network.policy(),
    })
}

fn network_host(repo_url: &str) -> Option<String> {
    at_model::network::NetworkHost::from_url(repo_url)
        .map(|host| host.to_string())
        .or_else(|| {
            (repo_url.starts_with("file://") || Path::new(repo_url).is_absolute())
                .then(|| "localhost".to_owned())
        })
}

fn network_failure(denied: &at_model::network::NetworkDenied) -> RemoteBackupFailure {
    match denied.reason {
        DenialReason::Offline => failure(
            RemoteBackupFailureCode::Offline,
            "Offline mode is on; turn it off before connecting to the remote.",
            RemoteBackupRemedy::TurnNetworkOn,
        ),
        _ => failure(
            RemoteBackupFailureCode::NetworkDenied,
            "The network policy refused this remote-backup request.",
            RemoteBackupRemedy::ReviewNetworkPolicy,
        ),
    }
}

fn backup_failure(error: at_store::remote_backup::BackupError) -> RemoteBackupFailure {
    use at_store::remote_backup::BackupError;
    match error {
        BackupError::Unconfigured => failure(
            RemoteBackupFailureCode::RepositoryNotConfigured,
            "Choose a GitHub repository in Preferences before using remote backup.",
            RemoteBackupRemedy::ConfigureRepository,
        ),
        BackupError::TokenMissing => failure(
            RemoteBackupFailureCode::TokenMissing,
            "No GitHub token is saved; add one in Preferences.",
            RemoteBackupRemedy::SaveToken,
        ),
        BackupError::AuthRejected => failure(
            RemoteBackupFailureCode::TokenRejected,
            "GitHub rejected the saved token; replace it in Preferences.",
            RemoteBackupRemedy::ReplaceToken,
        ),
        BackupError::Offline => failure(
            RemoteBackupFailureCode::Offline,
            "The remote could not be reached; check offline mode and retry.",
            RemoteBackupRemedy::TurnNetworkOn,
        ),
        BackupError::Conflict => failure(
            RemoteBackupFailureCode::RemoteConflict,
            "The remote has newer commits; sync before pushing again.",
            RemoteBackupRemedy::SyncFirst,
        ),
        BackupError::DeletionConfirmationRequired => failure(
            RemoteBackupFailureCode::ConfirmDeletions,
            "Remote deletions would remove unchanged local files; confirm and retry the sync.",
            RemoteBackupRemedy::ConfirmDeletions,
        ),
        BackupError::NonEmptyRestore => failure(
            RemoteBackupFailureCode::NonEmptyTarget,
            "Restore is available only when the target store is empty.",
            RemoteBackupRemedy::ChooseEmptyStore,
        ),
        BackupError::Denied(denied) => network_failure(&denied),
        BackupError::InvalidSnapshotPath(_) | BackupError::CorruptSnapshot { .. } => failure(
            RemoteBackupFailureCode::InvalidStore,
            "The remote snapshot is invalid; choose another commit or contact support.",
            RemoteBackupRemedy::Retry,
        ),
        _ => failure(
            RemoteBackupFailureCode::ActionFailed,
            "Remote backup did not finish. Check the connection and retry.",
            RemoteBackupRemedy::Retry,
        ),
    }
}

fn start_remote_job(
    state: &AppState,
    network: &NetworkState,
    setup: RemoteBackupSetup,
    operation: RemoteBackupOperation,
    name: &'static str,
) -> Result<JobId, RemoteBackupFailure> {
    let network = network.clone();
    let spec = JobSpec::new_cancellable(name, move |progress, cancel| async move {
        progress.fraction(0.05, "Preparing the remote backup");
        let cancellation = cancel.cancellation_flag();
        let device_name = setup.device_name.clone();
        let worker_progress = progress.clone();
        let worker_network = network.clone();
        let worker = tokio::task::spawn_blocking(move || {
            worker_progress.fraction(0.2, "Connecting to the configured remote");
            let remote_runner = ProcessRemoteGitRunner::default().with_cancel(cancellation);
            let local_runner = ProcessLocalGitRunner::default();
            let mut log = NetworkEventLog::new();
            let result = match setup.database.lock() {
                Ok(mut database) => {
                    let secrets = at_secrets::KeychainStore::new();
                    let redactor = redactor_for_secrets(&secrets);
                    perform_remote_operation(
                        &setup,
                        &mut database,
                        &secrets,
                        &redactor,
                        &mut log,
                        &remote_runner,
                        &local_runner,
                        operation,
                    )
                    .map_err(backup_failure)
                }
                Err(_) => Err(failure(
                    RemoteBackupFailureCode::InvalidStore,
                    "The remote-backup database is unavailable.",
                    RemoteBackupRemedy::Retry,
                )),
            };
            (result, log.to_vec())
        })
        .await
        .map_err(|_| JobError::RemoteBackup {
            failure: failure(
                RemoteBackupFailureCode::ActionFailed,
                "Remote backup did not finish. Check the connection and retry.",
                RemoteBackupRemedy::Retry,
            ),
        })?;
        let (result, events) = worker;
        for event in events {
            worker_network.record(event);
        }
        let outcome = result.map_err(|failure| JobError::RemoteBackup { failure })?;
        progress.fraction(
            1.0,
            if outcome.pushed {
                format!("Backup for {device_name} completed.")
            } else {
                format!("Remote backup for {device_name} is up to date.")
            },
        );
        Ok(())
    });
    state
        .bridge()
        .spawn_job(spec)
        .map(|job| job.id().clone())
        .map_err(|_| {
            failure(
                RemoteBackupFailureCode::ActionFailed,
                "The background job service is unavailable.",
                RemoteBackupRemedy::Retry,
            )
        })
}

// One argument per injected collaborator (policy, log, runners, secrets); bundling them would only
// move the same list into a struct used once.
#[allow(clippy::too_many_arguments)]
fn perform_remote_operation(
    setup: &RemoteBackupSetup,
    database: &mut Database,
    secrets: &dyn SecretStore,
    redactor: &Redactor,
    log: &mut NetworkEventLog,
    remote_runner: &dyn RemoteGitRunner,
    local_runner: &dyn LocalGitRunner,
    operation: RemoteBackupOperation,
) -> Result<at_store::remote_backup::BackupOutcome, at_store::remote_backup::BackupError> {
    let mut request = at_store::remote_backup::BackupRequest {
        store: &setup.store,
        db: database,
        secrets,
        policy: &setup.policy,
        log,
        remote_runner,
        local_runner,
        redactor,
        now: OffsetDateTime::now_utc(),
    };
    match operation {
        RemoteBackupOperation::Backup => at_store::remote_backup::push_backup(&mut request),
        RemoteBackupOperation::Sync { confirm_deletions } => {
            at_store::remote_backup::sync_from_remote(&mut request, confirm_deletions)
        }
        RemoteBackupOperation::Restore { commit } => {
            at_store::remote_backup::restore_from_remote(&mut request, &commit, false)
        }
    }
}

fn redactor_for_secrets(secrets: &dyn SecretStore) -> Redactor {
    let mut redactor = Redactor::new();
    if let Ok(token) = at_secrets::read_token(secrets) {
        redactor.insert_value(token.expose_secret());
    }
    redactor
}

fn list_remote_commits(
    setup: RemoteBackupSetup,
    network: NetworkState,
) -> Result<Vec<RemoteBackupCommitView>, RemoteBackupFailure> {
    let secrets = at_secrets::KeychainStore::new();
    let token = at_secrets::read_token(&secrets).map_err(|_| {
        failure(
            RemoteBackupFailureCode::TokenMissing,
            "No GitHub token is saved; add one in Preferences.",
            RemoteBackupRemedy::SaveToken,
        )
    })?;
    let auth = GitAuth::token(token.expose_secret()).map_err(|_| {
        failure(
            RemoteBackupFailureCode::ActionFailed,
            "Could not prepare GitHub authentication.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    let redactor = redactor_for_secrets(&secrets);
    let local = ProcessLocalGitRunner::default();
    let remote = ProcessRemoteGitRunner::default();
    let mut log = NetworkEventLog::new();
    let mut context = RemoteContext {
        policy: &setup.policy,
        log: &mut log,
        runner: &remote,
        local: &local,
        redact: &|text| redactor.redact_text(text),
        now: OffsetDateTime::now_utc(),
    };
    let head = context
        .remote_head(&setup.repo_url, &auth)
        .map_err(|error| backup_failure(error.into()))?;
    if head.commit.is_empty() {
        for event in log.to_vec() {
            network.record(event);
        }
        return Ok(Vec::new());
    }
    let clone =
        std::env::temp_dir().join(format!("agentuna-remote-history-{}", uuid::Uuid::now_v7()));
    let _guard = TempDirectory::new(clone.clone());
    context
        .clone_remote(&setup.repo_url, &clone, &auth)
        .map_err(|error| backup_failure(error.into()))?;
    let output = local
        .run(&LocalGitCall {
            repo: &clone,
            args: &["log", "--format=%H%x1f%ct%x1f%s", "-n", "30"],
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(|_| {
            failure(
                RemoteBackupFailureCode::ActionFailed,
                "Could not read remote commit history.",
                RemoteBackupRemedy::Retry,
            )
        })?;
    if !output.success {
        return Err(failure(
            RemoteBackupFailureCode::ActionFailed,
            "Could not read remote commit history.",
            RemoteBackupRemedy::Retry,
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let commits = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\u{1f}');
            let commit = fields.next()?;
            let timestamp = fields.next()?;
            let message = fields.next()?;
            if validate_commit(commit).is_err() {
                return None;
            }
            let seconds = timestamp.parse::<i64>().ok()?;
            let committed_at = OffsetDateTime::from_unix_timestamp(seconds)
                .ok()
                .and_then(|date| date.format(&Rfc3339).ok())
                .unwrap_or_else(|| timestamp.to_owned());
            Some(RemoteBackupCommitView {
                commit: commit.to_owned(),
                committed_at,
                message: redactor.redact_text(message),
            })
        })
        .collect();
    for event in log.to_vec() {
        network.record(event);
    }
    Ok(commits)
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn validate_commit(commit: &str) -> Result<(), RemoteBackupFailure> {
    if (commit.len() == 40 || commit.len() == 64)
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(failure(
            RemoteBackupFailureCode::InvalidStore,
            "The selected commit identifier is invalid.",
            RemoteBackupRemedy::ReviewAgain,
        ))
    }
}

fn issue_restore_grant(commit: &str, repo_url: &str, store_root: &Path) -> String {
    let token = uuid::Uuid::now_v7().to_string();
    let grant = RestoreGrant {
        commit: commit.to_owned(),
        repo_url: repo_url.to_owned(),
        store_root: store_root.to_path_buf(),
        expires: Instant::now() + RESTORE_CONFIRMATION_TTL,
    };
    let mut grants = lock(&RESTORE_GRANTS);
    grants.retain(|_, grant| grant.expires > Instant::now());
    grants.insert(token.clone(), grant);
    while grants.len() > 128 {
        if let Some(oldest) = grants.keys().next().cloned() {
            grants.remove(&oldest);
        }
    }
    token
}

fn consume_restore_grant(
    token: &str,
    commit: &str,
    repo_url: &str,
    store_root: &Path,
) -> Result<(), RemoteBackupFailure> {
    let grant = lock(&RESTORE_GRANTS).remove(token).ok_or_else(|| {
        failure(
            RemoteBackupFailureCode::NonEmptyTarget,
            "Restore needs a fresh explicit confirmation for this commit.",
            RemoteBackupRemedy::ChooseEmptyStore,
        )
    })?;
    if grant.expires <= Instant::now()
        || grant.commit != commit
        || grant.repo_url != repo_url
        || grant.store_root != store_root
    {
        return Err(failure(
            RemoteBackupFailureCode::NonEmptyTarget,
            "Restore confirmation expired or no longer matches this target.",
            RemoteBackupRemedy::ChooseEmptyStore,
        ));
    }
    Ok(())
}

fn store_is_empty_target(store: &StoreLayout) -> Result<(), RemoteBackupFailure> {
    let manifest = at_store::manifest::StoreManifest::load(store).map_err(|_| {
        failure(
            RemoteBackupFailureCode::InvalidStore,
            "The target store manifest is invalid; no files were restored.",
            RemoteBackupRemedy::Retry,
        )
    })?;
    if !manifest.manifest.items().is_empty() || !manifest.manifest.disabled().is_empty() {
        return Err(non_empty_target_failure());
    }
    for directory in [store.skills_dir(), store.rules_dir()] {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(non_empty_target_failure());
            }
            Ok(_) => match fs::read_dir(&directory) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        return Err(non_empty_target_failure());
                    }
                }
                Err(_) => return Err(invalid_store_failure()),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(invalid_store_failure()),
        }
    }
    let mcp_path = store.mcp_file();
    match fs::symlink_metadata(&mcp_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(non_empty_target_failure());
        }
        Ok(_) => {
            let bytes =
                at_fs::symlink::read_nofollow(&mcp_path).map_err(|_| invalid_store_failure())?;
            let mcp = at_store::mcp_store::McpStore::parse(&bytes)
                .map_err(|_| invalid_store_failure())?;
            if !mcp.servers.is_empty() || !mcp.extra.is_empty() {
                return Err(non_empty_target_failure());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(invalid_store_failure()),
    }
    for entry in fs::read_dir(store.root()).map_err(|_| invalid_store_failure())? {
        let entry = entry.map_err(|_| invalid_store_failure())?;
        let name = entry.file_name();
        if ![
            "store.json",
            "mcp.json",
            "skills",
            "rules",
            "backups",
            ".state",
            ".gitignore",
        ]
        .iter()
        .any(|allowed| name == *allowed)
        {
            return Err(non_empty_target_failure());
        }
        let file_type = entry.file_type().map_err(|_| invalid_store_failure())?;
        if file_type.is_symlink() {
            return Err(non_empty_target_failure());
        }
    }
    Ok(())
}

fn non_empty_target_failure() -> RemoteBackupFailure {
    failure(
        RemoteBackupFailureCode::NonEmptyTarget,
        "Restore is available only when the target store is empty.",
        RemoteBackupRemedy::ChooseEmptyStore,
    )
}

fn invalid_store_failure() -> RemoteBackupFailure {
    failure(
        RemoteBackupFailureCode::InvalidStore,
        "The target store could not be safely inspected.",
        RemoteBackupRemedy::Retry,
    )
}

fn list_conflicts(store: &StoreLayout) -> Result<Vec<RemoteBackupConflictGroupView>, CommandError> {
    let root = store.state_dir().join("remote-conflicts");
    let root_type = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(CommandError::internal("conflict folders are unavailable")),
    };
    if root_type.is_symlink() || !root_type.is_dir() {
        return Err(CommandError::internal(
            "conflict folder is not a safe directory",
        ));
    }
    let mut groups = Vec::new();
    let mut devices = read_sorted_directory(&root)?;
    for device in devices.drain(..) {
        let device_type = device
            .file_type()
            .map_err(|_| CommandError::internal("conflict folder is unavailable"))?;
        if device_type.is_symlink() || !device_type.is_dir() {
            continue;
        }
        let device_name = safe_component_name(&device.file_name())
            .ok_or_else(|| CommandError::internal("conflict device folder is invalid"))?;
        let mut timestamps = read_sorted_directory(&device.path())?;
        for timestamp in timestamps.drain(..) {
            let timestamp_type = timestamp
                .file_type()
                .map_err(|_| CommandError::internal("conflict folder is unavailable"))?;
            if timestamp_type.is_symlink() || !timestamp_type.is_dir() {
                continue;
            }
            if is_resolved_group(&timestamp.path())? {
                continue;
            }
            let timestamp_name = safe_component_name(&timestamp.file_name())
                .ok_or_else(|| CommandError::internal("conflict timestamp folder is invalid"))?;
            let mut files = Vec::new();
            collect_conflict_files(store, &timestamp.path(), &timestamp.path(), 0, &mut files)?;
            if files.is_empty() {
                continue;
            }
            if groups.len() >= MAX_CONFLICT_GROUPS {
                return Err(CommandError::internal("too many remote conflict folders"));
            }
            groups.push(RemoteBackupConflictGroupView {
                id: format!("{device_name}/{timestamp_name}"),
                device_name: device_name.clone(),
                timestamp: timestamp_name,
                files,
            });
        }
    }
    groups.sort_by(|left, right| {
        (&left.device_name, &left.timestamp).cmp(&(&right.device_name, &right.timestamp))
    });
    Ok(groups)
}

fn read_sorted_directory(directory: &Path) -> Result<Vec<fs::DirEntry>, CommandError> {
    let entries = fs::read_dir(directory)
        .map_err(|_| CommandError::internal("conflict folder is unavailable"))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CommandError::internal("conflict folder is unavailable"))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn safe_component_name(name: &std::ffi::OsStr) -> Option<String> {
    let value = name.to_str()?;
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return None;
    }
    Some(value.to_owned())
}

fn is_resolved_group(group_root: &Path) -> Result<bool, CommandError> {
    let marker = group_root.join(".agentuna-resolved");
    match fs::symlink_metadata(marker) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                Err(CommandError::internal(
                    "remote conflict resolution marker is invalid",
                ))
            } else {
                Ok(true)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(CommandError::internal(
            "remote conflict resolution marker is unavailable",
        )),
    }
}

fn collect_conflict_files(
    store: &StoreLayout,
    group_root: &Path,
    directory: &Path,
    depth: usize,
    output: &mut Vec<RemoteBackupConflictFileView>,
) -> Result<(), CommandError> {
    if depth > MAX_CONFLICT_DEPTH {
        return Err(CommandError::internal("remote conflict folder is too deep"));
    }
    for entry in read_sorted_directory(directory)? {
        let name = entry.file_name();
        if name == ".agentuna-resolved"
            || name.to_string_lossy().starts_with(".agentuna-remote-copy-")
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|_| CommandError::internal("remote conflict item is unavailable"))?;
        if file_type.is_symlink() {
            return Err(CommandError::internal(
                "remote conflict folders cannot contain symbolic links",
            ));
        }
        if file_type.is_dir() {
            collect_conflict_files(store, group_root, &entry.path(), depth + 1, output)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(CommandError::internal(
                "remote conflict item is not a regular file",
            ));
        }
        if output.len() >= MAX_CONFLICT_FILES {
            return Err(CommandError::internal("too many remote conflict files"));
        }
        let remote_path = entry.path();
        let relative = remote_path
            .strip_prefix(group_root)
            .map_err(|_| CommandError::internal("remote conflict path escaped its folder"))?;
        let relative = path_to_safe_text(relative)
            .ok_or_else(|| CommandError::internal("remote conflict path is invalid"))?;
        let local_relative = Path::new(&relative);
        let local_path = store.root().join(local_relative);
        let local_metadata = match fs::symlink_metadata(&local_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CommandError::internal(
                    "local conflict copy is a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_file() => Some(metadata),
            Ok(_) => {
                return Err(CommandError::internal(
                    "local conflict copy is not a regular file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(CommandError::internal("local conflict copy is unavailable")),
        };
        let remote_size = entry
            .metadata()
            .map_err(|_| CommandError::internal("remote conflict item is unavailable"))?
            .len();
        output.push(RemoteBackupConflictFileView {
            path: relative,
            local_path: local_path.display().to_string(),
            remote_path: remote_path.display().to_string(),
            local_exists: local_metadata.is_some(),
            remote_size,
            local_size: local_metadata.map(|metadata| metadata.len()),
        });
    }
    Ok(())
}

fn path_to_safe_text(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let text = path.to_str()?;
    if text.contains('\\') || text.len() > 4096 {
        None
    } else {
        Some(text.to_owned())
    }
}

fn resolve_group_root(store: &StoreLayout, group_id: &str) -> Result<PathBuf, CommandError> {
    let mut parts = group_id.split('/');
    let device = parts.next().unwrap_or_default();
    let timestamp = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || safe_component_name(std::ffi::OsStr::new(device)).is_none()
        || safe_component_name(std::ffi::OsStr::new(timestamp)).is_none()
    {
        return Err(CommandError::invalid_argument(
            "group_id",
            "must identify a known device/timestamp conflict folder",
        ));
    }
    let group = store
        .state_dir()
        .join("remote-conflicts")
        .join(device)
        .join(timestamp);
    ensure_no_symlink_path(store.root(), &group, true)?;
    if is_resolved_group(&group)? {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "This conflict was already resolved; refresh the conflict list.",
        ));
    }
    Ok(group)
}

fn validate_relative_path(path: &Path) -> Result<(), CommandError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.to_string_lossy().contains('\\')
        || path.to_string_lossy().len() > 4096
    {
        return Err(CommandError::invalid_argument(
            "relative_path",
            "must be a safe store-relative path",
        ));
    }
    Ok(())
}

fn ensure_no_symlink_path(root: &Path, path: &Path, must_exist: bool) -> Result<(), CommandError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CommandError::invalid_argument("path", "must stay within the User store"))?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(CommandError::invalid_argument(
            "path",
            "must stay within the User store",
        ));
    }
    let mut current = root.to_path_buf();
    let parts = relative.components().collect::<Vec<_>>();
    for (index, component) in parts.iter().enumerate() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (index + 1 < parts.len() && !metadata.is_dir())
                {
                    return Err(CommandError::invalid_argument(
                        "path",
                        "must not cross symbolic links or non-directories",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !must_exist => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CommandError::new(
                    CommandErrorCode::NotFound,
                    "The remote conflict copy no longer exists.",
                ));
            }
            Err(_) => return Err(CommandError::internal("conflict path is unavailable")),
        }
    }
    Ok(())
}

fn conflict_side_path(
    store: &StoreLayout,
    group_id: &str,
    relative_path: &Path,
    side: &str,
) -> Result<PathBuf, CommandError> {
    validate_relative_path(relative_path)?;
    let group = resolve_group_root(store, group_id)?;
    let remote = group.join(relative_path);
    ensure_no_symlink_path(store.root(), &remote, true)?;
    let path = match side {
        "remote" => remote,
        "local" => {
            let path = store.root().join(relative_path);
            ensure_no_symlink_path(store.root(), &path, true)?;
            path
        }
        _ => {
            return Err(CommandError::invalid_argument(
                "side",
                "choose local or remote",
            ));
        }
    };
    if !path.is_file() {
        return Err(CommandError::new(
            CommandErrorCode::NotFound,
            "The selected conflict copy no longer exists.",
        ));
    }
    Ok(path)
}

fn build_conflict_plan(
    store: &StoreLayout,
    secrets: &dyn SecretStore,
    group_id: &str,
    relative_path: &Path,
    resolution: ConflictResolution,
) -> Result<CachedConflictPlan, CommandError> {
    validate_relative_path(relative_path)?;
    let group_root = resolve_group_root(store, group_id)?;
    let remote_path = group_root.join(relative_path);
    ensure_no_symlink_path(store.root(), &remote_path, true)?;
    if !remote_path.is_file() {
        return Err(CommandError::new(
            CommandErrorCode::NotFound,
            "The remote conflict copy no longer exists.",
        ));
    }
    let local_path = store.root().join(relative_path);
    ensure_no_symlink_path(store.root(), &local_path, false)?;
    let local_bytes = read_optional_regular(&local_path)?;
    let remote_bytes = at_fs::symlink::read_nofollow(&remote_path)
        .map_err(|_| CommandError::internal("remote conflict copy is unavailable"))?;
    let mut operations = Vec::new();
    let mut archived_remote_path = None;
    match resolution {
        ConflictResolution::KeepLocal => {
            operations.push(delete_operation(store, &remote_path, &group_root)?);
        }
        ConflictResolution::KeepRemote => {
            operations.push(write_operation(
                store,
                &local_path,
                relative_path,
                remote_bytes,
                local_bytes.as_deref(),
                true,
            )?);
            operations.push(delete_operation(store, &remote_path, &group_root)?);
        }
        ConflictResolution::KeepBoth => {
            let archive_name = format!(".agentuna-remote-copy-{}", uuid::Uuid::now_v7());
            let archive_path = group_root.join(&archive_name);
            let archive_relative = group_root
                .strip_prefix(store.root())
                .map_err(|_| CommandError::internal("conflict folder escaped the store"))?
                .join(archive_name);
            ensure_no_symlink_path(store.root(), &archive_path, false)?;
            operations.push(write_operation(
                store,
                &archive_path,
                &archive_relative,
                remote_bytes,
                None,
                false,
            )?);
            operations.push(delete_operation(store, &remote_path, &group_root)?);
            archived_remote_path = Some(archive_path.display().to_string());
        }
    }
    let mut remaining = Vec::new();
    collect_conflict_files(store, &group_root, &group_root, 0, &mut remaining)?;
    if remaining
        .iter()
        .all(|file| Path::new(&file.remote_path) == remote_path)
    {
        let marker_relative = group_root
            .strip_prefix(store.root())
            .map_err(|_| CommandError::internal("conflict folder escaped the store"))?
            .join(".agentuna-resolved");
        operations.push(FileOp::Write {
            path: marker_relative,
            bytes: format!(
                "resolved={}\n",
                OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "unknown".to_owned())
            )
            .into_bytes(),
            expected: ExpectedState::absent(),
            follow_symlink: false,
            backup: false,
        });
    }
    let now = OffsetDateTime::now_utc();
    let mut change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: String::new(),
        operation_kind: OperationKind::parse("remote_backup")
            .map_err(|_| CommandError::internal("remote-backup operation kind is unavailable"))?,
        scope: Some(store.scope()),
        project_id: None,
        summary: "resolve one remote-backup conflict after review".to_owned(),
        ops: operations,
        created_at_unix_seconds: u64::try_from(now.unix_timestamp()).unwrap_or_default(),
    };
    let mut preview = at_engine::preview::preview(&change_set, store.root())
        .map_err(|_| CommandError::internal("could not prepare a safe conflict review"))?;
    change_set.preview_id.clone_from(&preview.id);
    preview = at_engine::preview::preview(&change_set, store.root())
        .map_err(|_| CommandError::internal("could not prepare a safe conflict review"))?;
    let redactor = redactor_for_secrets(secrets);
    let files = preview
        .files
        .iter()
        .map(|file| RemoteBackupConflictChangeView {
            path: file.path.display().to_string(),
            action: file.action.clone(),
            diff: match &file.diff {
                at_engine::preview::DiffKind::Unified { text, .. } => redactor.redact_text(text),
                at_engine::preview::DiffKind::Binary { size } => {
                    format!("Binary file; {size} bytes.")
                }
                at_engine::preview::DiffKind::TooLarge { size } => {
                    format!("File is too large to display; {size} bytes.")
                }
                at_engine::preview::DiffKind::Unchanged => "No content change.".to_owned(),
                _ => "File change preview is unavailable.".to_owned(),
            },
            before_hash: file.before_hash.clone(),
            after_hash: file.after_hash.clone(),
            backup_required: file.backup_required,
        })
        .collect();
    let preview_id = preview.id.clone();
    let view = RemoteBackupConflictPreviewView {
        preview_id,
        summary: format!("{} conflict resolution", resolution.label()),
        files,
        archived_remote_path,
    };
    Ok(CachedConflictPlan {
        store_root: store.root().to_path_buf(),
        change_set,
        preview,
        view,
    })
}

fn read_optional_regular(path: &Path) -> Result<Option<Vec<u8>>, CommandError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            CommandError::internal("local conflict copy is not a regular file"),
        ),
        Ok(_) => at_fs::symlink::read_nofollow(path)
            .map(Some)
            .map_err(|_| CommandError::internal("local conflict copy is unavailable")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CommandError::internal("local conflict copy is unavailable")),
    }
}

fn write_operation(
    store: &StoreLayout,
    target: &Path,
    relative: &Path,
    bytes: Vec<u8>,
    existing: Option<&[u8]>,
    backup: bool,
) -> Result<FileOp, CommandError> {
    let expected = if let Some(existing) = existing {
        ExpectedState::hash(at_fs::sha256_hex(existing))
            .map_err(|_| CommandError::internal("could not review file content"))?
    } else {
        ExpectedState::absent()
    };
    ensure_no_symlink_path(store.root(), target, false)?;
    Ok(FileOp::Write {
        path: relative.to_path_buf(),
        bytes,
        expected,
        follow_symlink: false,
        backup,
    })
}

fn delete_operation(
    store: &StoreLayout,
    target: &Path,
    group_root: &Path,
) -> Result<FileOp, CommandError> {
    ensure_no_symlink_path(store.root(), target, true)?;
    let relative = target
        .strip_prefix(store.root())
        .map_err(|_| CommandError::internal("conflict path escaped the User store"))?;
    if !target.starts_with(group_root) {
        return Err(CommandError::internal(
            "conflict deletion escaped its reviewed folder",
        ));
    }
    let bytes = read_optional_regular(target)?.ok_or_else(|| {
        CommandError::new(CommandErrorCode::NotFound, "conflict file disappeared")
    })?;
    let expected = ExpectedState::hash(at_fs::sha256_hex(&bytes))
        .map_err(|_| CommandError::internal("could not review conflict file"))?;
    Ok(FileOp::Delete {
        path: relative.to_path_buf(),
        expected,
    })
}

struct ConflictApplyOutcome {
    operation_id: String,
    completed: usize,
}

fn apply_conflict_plan(
    store: &StoreLayout,
    database: &mut Database,
    plan: CachedConflictPlan,
) -> Result<ConflictApplyOutcome, CommandError> {
    if store.root() != plan.store_root {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The target store changed; reopen the conflict and review current copies.",
        ));
    }
    let current = at_engine::preview::preview(&plan.change_set, store.root()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "Conflict contents changed; reopen the conflict and review current copies.",
        )
    })?;
    if current.id != plan.change_set.preview_id || current.id != plan.preview.id {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "Conflict contents changed; reopen the conflict and review current copies.",
        ));
    }
    let roots = at_fs::ApplyRoots::new([store.root().to_path_buf()])
        .map_err(|_| CommandError::internal("User store authorization failed"))?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: store.root(),
        backup_folder: &store.backups_dir(),
        recorder: &mut recorder,
    };
    let outcome = at_fs::apply_with_roots(&plan.change_set, &mut context, &roots).map_err(
        |error| match error {
            at_fs::ChangeSetApplyError::Stale(_) => CommandError::new(
                CommandErrorCode::Conflict,
                "Conflict contents changed; reopen the conflict and review current copies.",
            ),
            _ => CommandError::internal("could not apply the reviewed conflict resolution"),
        },
    )?;
    Ok(ConflictApplyOutcome {
        operation_id: outcome.operation_id,
        completed: outcome.completed,
    })
}

fn reveal_path(path: &Path) -> Result<(), CommandError> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .status();
    result
        .map_err(|_| CommandError::unavailable("could not open the file manager"))?
        .success()
        .then_some(())
        .ok_or_else(|| CommandError::unavailable("could not open the file manager"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use at_git::local::ProcessLocalGitRunner;
    use at_git::remote::{ProcessRemoteGitRunner, RemoteError};
    use at_model::Scope;
    use at_model::network::{NetworkEventLog, NetworkPolicy};
    use at_secrets::{MemoryStore, SecretStore};
    use at_store::layout::StoreLayout;
    use tempfile::TempDir;

    use super::{
        ConflictResolution, RemoteBackupFailureCode, RemoteBackupOperation, RemoteBackupSetup,
        apply_conflict_plan, backup_failure, build_conflict_plan, consume_restore_grant,
        issue_restore_grant, list_conflicts, perform_remote_operation, redactor_for_secrets,
        store_is_empty_target,
    };

    struct Fixture {
        _temp: TempDir,
        store: StoreLayout,
        database: at_db::Database,
        secrets: MemoryStore,
        group_id: String,
        item_path: &'static str,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().expect("temporary fixture");
            let root = temp.path().join("user-store");
            fs::create_dir_all(&root).expect("create store root");
            let root = fs::canonicalize(root).expect("canonical store root");
            let store = StoreLayout::new(Scope::User, root);
            store.init().expect("initialize user store");
            let item_path = "skills/sample/SKILL.md";
            let local = store.root().join(item_path);
            fs::create_dir_all(local.parent().expect("local parent")).expect("local folder");
            fs::write(&local, b"local version\n").expect("local copy");
            let group_id = "laptop/2026-11-06-abcdef12".to_owned();
            let remote = store
                .state_dir()
                .join("remote-conflicts")
                .join(&group_id)
                .join(item_path);
            fs::create_dir_all(remote.parent().expect("remote parent")).expect("remote folder");
            fs::write(&remote, b"remote version\n").expect("remote copy");
            Self {
                _temp: temp,
                store,
                database: at_db::Database::open_in_memory().expect("database"),
                secrets: MemoryStore::new(),
                group_id,
                item_path,
            }
        }
    }

    #[test]
    fn keep_remote_applies_reviewed_change_set_and_records_operation() {
        let mut fixture = Fixture::new();
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepRemote,
        )
        .expect("review remote copy");
        assert!(!plan.view.files.is_empty());
        let outcome = apply_conflict_plan(&fixture.store, &mut fixture.database, plan)
            .expect("apply reviewed resolution");
        assert_eq!(
            fs::read(fixture.store.root().join(fixture.item_path)).expect("local file"),
            b"remote version\n"
        );
        assert!(
            !fixture
                .store
                .state_dir()
                .join("remote-conflicts")
                .join(&fixture.group_id)
                .join(fixture.item_path)
                .exists()
        );
        assert!(
            at_db::ops::operation_detail(fixture.database.connection(), &outcome.operation_id,)
                .expect("operation query")
                .is_some()
        );
        assert_eq!(
            at_store::remote_backup::remote_conflict_count(&fixture.store)
                .expect("unresolved count"),
            0
        );
    }

    #[test]
    fn keep_local_preserves_current_bytes_and_marks_the_group_resolved() {
        let mut fixture = Fixture::new();
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepLocal,
        )
        .expect("review local copy");
        apply_conflict_plan(&fixture.store, &mut fixture.database, plan)
            .expect("apply reviewed resolution");
        assert_eq!(
            fs::read(fixture.store.root().join(fixture.item_path)).expect("local file"),
            b"local version\n"
        );
        assert_eq!(
            list_conflicts(&fixture.store).expect("conflict list").len(),
            0
        );
        assert_eq!(
            at_store::remote_backup::remote_conflict_count(&fixture.store)
                .expect("unresolved count"),
            0
        );
    }

    #[test]
    fn keep_both_preserves_local_and_archives_remote_copy() {
        let mut fixture = Fixture::new();
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepBoth,
        )
        .expect("review both copies");
        let archive = plan
            .view
            .archived_remote_path
            .clone()
            .expect("archive path");
        apply_conflict_plan(&fixture.store, &mut fixture.database, plan)
            .expect("apply reviewed resolution");
        assert_eq!(
            fs::read(fixture.store.root().join(fixture.item_path)).expect("local file"),
            b"local version\n"
        );
        assert_eq!(
            fs::read(archive).expect("archived remote copy"),
            b"remote version\n"
        );
        assert_eq!(
            list_conflicts(&fixture.store).expect("conflict list").len(),
            0
        );
        assert_eq!(
            at_store::remote_backup::remote_conflict_count(&fixture.store)
                .expect("unresolved count"),
            0
        );
    }

    #[test]
    fn resolving_one_file_does_not_hide_other_conflicts_in_its_group() {
        let mut fixture = Fixture::new();
        let second_path = "rules/second.md";
        let second_remote = fixture
            .store
            .state_dir()
            .join("remote-conflicts")
            .join(&fixture.group_id)
            .join(second_path);
        fs::create_dir_all(second_remote.parent().expect("second parent")).expect("second folder");
        fs::write(second_remote, b"second remote\n").expect("second conflict");
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepLocal,
        )
        .expect("review local copy");
        apply_conflict_plan(&fixture.store, &mut fixture.database, plan)
            .expect("apply reviewed resolution");
        let groups = list_conflicts(&fixture.store).expect("conflict list");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 1);
        assert_eq!(groups[0].files[0].path, second_path);
        assert_eq!(
            at_store::remote_backup::remote_conflict_count(&fixture.store)
                .expect("unresolved count"),
            1
        );
    }

    #[test]
    fn stale_resolution_cannot_overwrite_an_external_edit() {
        let mut fixture = Fixture::new();
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepRemote,
        )
        .expect("review remote copy");
        fs::write(
            fixture.store.root().join(fixture.item_path),
            b"new external version\n",
        )
        .expect("external edit");
        assert!(apply_conflict_plan(&fixture.store, &mut fixture.database, plan).is_err());
        assert_eq!(
            fs::read(fixture.store.root().join(fixture.item_path)).expect("current file"),
            b"new external version\n"
        );
    }

    #[test]
    fn restore_refuses_any_managed_or_unmanaged_content() {
        let fixture = Fixture::new();
        let error = store_is_empty_target(&fixture.store).expect_err("non-empty target");
        assert_eq!(error.code, RemoteBackupFailureCode::NonEmptyTarget);
    }

    #[test]
    fn restore_confirmation_is_bound_to_target_commit_and_consumed_once() {
        let temp = TempDir::new().expect("temporary fixture");
        let root = temp.path().join("user-store");
        fs::create_dir_all(&root).expect("create store root");
        let root = fs::canonicalize(root).expect("canonical store root");
        let commit = "a".repeat(40);
        let token = issue_restore_grant(&commit, "https://github.com/acme/backup.git", &root);
        consume_restore_grant(&token, &commit, "https://github.com/acme/backup.git", &root)
            .expect("matching confirmation");
        assert!(
            consume_restore_grant(&token, &commit, "https://github.com/acme/backup.git", &root)
                .is_err()
        );
    }

    #[test]
    fn conflict_diff_and_typed_failure_never_expose_the_saved_token() {
        let fixture = Fixture::new();
        let token = concat!("ghp_", "remote-backup-sentinel-0123456789");
        at_secrets::save_token(&fixture.secrets, token).expect("store fake token");
        fs::write(
            fixture
                .store
                .state_dir()
                .join("remote-conflicts")
                .join(&fixture.group_id)
                .join(fixture.item_path),
            format!("remote content {token}\n"),
        )
        .expect("secret-bearing fixture content");
        let plan = build_conflict_plan(
            &fixture.store,
            &fixture.secrets,
            &fixture.group_id,
            Path::new(fixture.item_path),
            ConflictResolution::KeepRemote,
        )
        .expect("review redacted diff");
        let output = serde_json::to_string(&plan.view).expect("serialize preview");
        assert!(!output.contains(token));
        let error = backup_failure(at_store::remote_backup::BackupError::Git(
            RemoteError::CommandFailed {
                args: "push".to_owned(),
                code: "1".to_owned(),
                stderr: token.to_owned(),
            },
        ));
        assert!(!error.message.contains(token));
        assert!(!format!("{error:?}").contains(token));
        let redactor = redactor_for_secrets(&fixture.secrets);
        assert_eq!(redactor.redact_text(token), "***");
    }

    struct BackupFixture {
        _temp: TempDir,
        setup: RemoteBackupSetup,
        database: Arc<Mutex<at_db::Database>>,
        secrets: MemoryStore,
        token: String,
    }

    impl BackupFixture {
        fn new() -> Self {
            let temp = TempDir::new().expect("temporary fixture");
            let remote_path = temp.path().join("remote.git");
            let output = Command::new("git")
                .args([
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    remote_path.to_str().expect("UTF-8 temp path"),
                ])
                .output()
                .expect("initialize local bare remote");
            assert!(
                output.status.success(),
                "local bare remote initialization failed"
            );
            let root = temp.path().join("user-store");
            fs::create_dir_all(&root).expect("create store root");
            let root = fs::canonicalize(root).expect("canonical store root");
            let store = StoreLayout::new(Scope::User, root);
            store.init().expect("initialize User store");
            let database = Arc::new(Mutex::new(
                at_db::Database::open_in_memory().expect("database"),
            ));
            let mut database_guard = database.lock().expect("database lock");
            let remote_url = remote_path.to_string_lossy().into_owned();
            at_db::remote_backup::save_remote_backup_state(
                database_guard.connection(),
                &at_db::remote_backup::RemoteBackupState {
                    repo_url: Some(remote_url.clone()),
                    branch: Some("main".to_owned()),
                    status: at_db::remote_backup::RemoteBackupStatus::Idle,
                    ..at_db::remote_backup::RemoteBackupState::default()
                },
            )
            .expect("configure bare remote");
            at_store::remote_backup::set_device_name(&mut database_guard, Some("laptop"))
                .expect("configure device");
            drop(database_guard);
            let secrets = MemoryStore::new();
            let token = concat!("ghp_", "local-remote-sentinel-0123456789").to_owned();
            at_secrets::save_token(&secrets, &token).expect("store fake GitHub token");
            let setup = RemoteBackupSetup {
                database: Arc::clone(&database),
                store,
                repo_url: remote_url,
                device_name: "laptop".to_owned(),
                policy: NetworkPolicy::default(),
            };
            Self {
                _temp: temp,
                setup,
                database,
                secrets,
                token,
            }
        }
    }

    #[test]
    fn backup_action_uses_local_bare_remote_and_records_safe_status() {
        let fixture = BackupFixture::new();
        let mut database = fixture.database.lock().expect("database lock");
        let secrets: &dyn SecretStore = &fixture.secrets;
        let redactor = redactor_for_secrets(secrets);
        let mut log = NetworkEventLog::new();
        let remote_runner = ProcessRemoteGitRunner::default();
        let local_runner = ProcessLocalGitRunner::default()
            .with_env("GIT_AUTHOR_NAME", "AgenTuna test")
            .with_env("GIT_AUTHOR_EMAIL", "agentuna@example.invalid")
            .with_env("GIT_COMMITTER_NAME", "AgenTuna test")
            .with_env("GIT_COMMITTER_EMAIL", "agentuna@example.invalid");
        let outcome = perform_remote_operation(
            &fixture.setup,
            &mut database,
            secrets,
            &redactor,
            &mut log,
            &remote_runner,
            &local_runner,
            RemoteBackupOperation::Backup,
        )
        .expect("push to local bare remote");
        assert!(outcome.committed);
        assert!(outcome.pushed);
        assert!(!format!("{outcome:?}").contains(&fixture.token));
        assert!(!format!("{log:?}").contains(&fixture.token));
        let remote = at_db::remote_backup::remote_backup_state(database.connection())
            .expect("remote status");
        assert!(remote.last_commit.is_some());
        assert_eq!(
            remote.status,
            at_db::remote_backup::RemoteBackupStatus::Idle
        );
    }

    #[test]
    fn failed_remote_error_is_mapped_to_secret_free_typed_remedy() {
        let token = concat!("ghp_", "remote-failure-sentinel-0123456789");
        let error = backup_failure(at_store::remote_backup::BackupError::Git(
            RemoteError::CommandFailed {
                args: "push".to_owned(),
                code: "1".to_owned(),
                stderr: token.to_owned(),
            },
        ));
        assert_eq!(error.code, RemoteBackupFailureCode::ActionFailed);
        assert_eq!(error.remedy, super::RemoteBackupRemedy::Retry);
        assert!(!error.message.contains(token));
        assert!(!format!("{error:?}").contains(token));
    }
}
