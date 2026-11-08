//! Typed command adapters for the Sync & drift screen and its backup catalogue.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use at_db::drift::DriftEvent;
use at_engine::drift::{DriftContent, DriftService, DriftTarget, targets_from_index};
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, OperationKind, Scope};
use at_store::layout::{Slug, StoreLayout};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::drift::{DriftWatcherControl, DriftWatcherStatusView};
use super::state::AppState;

const PREVIEW_MAX_BYTES: usize = 1024 * 1024;
const BACKUP_LIST_LIMIT: usize = 500;

/// Open drift rows and the current watcher condition for the Sync screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct DriftSnapshotView {
    /// Unresolved drift rows, newest first.
    pub rows: Vec<DriftRowView>,
    /// Number of registered deployment paths that currently match their baselines.
    pub in_sync_count: usize,
    /// Number of indexed deployment paths inspected.
    pub watched_count: usize,
    /// Current native watcher condition.
    pub watcher: DriftWatcherStatusView,
}

/// One unresolved drift event, enriched with its live filesystem classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct DriftRowView {
    /// Stable drift event row id.
    pub event_id: i64,
    /// Current classification: drifted, link_replaced_by_file, link_removed, file_deleted,
    /// store_file_edited_while_linked or unreadable.
    pub kind: String,
    /// Canonical item id, when the deployment still maps to one.
    pub item_id: Option<String>,
    /// Canonical item name or the target filename when no item remains.
    pub item_name: String,
    /// Native agent id.
    pub agent: String,
    /// Scope of the recorded deployment.
    pub scope: String,
    /// Exact path that drifted.
    pub path: String,
    /// RFC 3339 time the drift was first recorded.
    pub detected_at: String,
    /// Number of filesystem entries represented by the notice.
    pub changed_file_count: usize,
    /// Whether the item can be adopted into the canonical store.
    pub can_adopt: bool,
    /// Whether AgenTuna's canonical version can be restored to disk.
    pub can_restore: bool,
    /// Registered managed-link destination, when present.
    pub link_target: Option<String>,
}

/// Exact comparison data for a drift row before the user chooses an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct DriftPreviewView {
    /// Drift event being reviewed.
    pub event_id: i64,
    /// Exact deployed path.
    pub path: String,
    /// Canonical store path, when the item remains available.
    pub canonical_path: Option<String>,
    /// Whether verified baseline bytes are available from the canonical store or a retained backup.
    pub previous_version_available: bool,
    /// Unified diff from verified baseline bytes to the current on-disk file.
    pub diff: String,
    /// Why the baseline cannot be shown when its bytes were not retained.
    pub baseline_unavailable_reason: Option<String>,
    /// Current on-disk UTF-8 content when it can be shown without a fabricated diff.
    pub on_disk_content: Option<String>,
    /// Whether displayed content was redacted through the process secret redactor.
    pub redacted: bool,
}

/// A catalogue backup listed in the Backups tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BackupRowView {
    /// Stable backup catalogue id.
    pub id: String,
    /// Original destination path.
    pub original_path: Option<String>,
    /// Backup creation time (RFC 3339).
    pub created_at: String,
    /// Stored payload size in bytes.
    pub size: Option<i64>,
    /// Operation that produced this backup, when known.
    pub operation_id: Option<String>,
    /// Whether the payload can be verified and restored.
    pub restorable: bool,
    /// Why the payload cannot be restored, when unavailable.
    pub unavailable_reason: Option<String>,
}

/// Backup contents and the exact diff for an explicit restore confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BackupPreviewView {
    /// Backup catalogue id.
    pub id: String,
    /// Content-bound preview identity the restore must echo.
    pub preview_id: String,
    /// Destination path recorded by the original operation.
    pub original_path: String,
    /// Backup creation time (RFC 3339).
    pub created_at: String,
    /// Unified diff from current destination to the saved backup payload.
    pub diff: String,
    /// Digest of the destination bytes at preview time; `None` when it was absent.
    pub destination_hash: Option<String>,
    /// Whether the destination currently exists.
    pub destination_exists: bool,
    /// Whether the diff text was redacted through the process secret redactor.
    pub redacted: bool,
}

/// Preview identity supplied with an explicit backup restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BackupRestoreRequest {
    /// Backup catalogue id.
    pub backup_id: String,
    /// Identity returned by `backup_preview`.
    pub preview_id: String,
    /// Destination digest returned by `backup_preview`; `None` when it was absent.
    pub destination_hash: Option<String>,
}

/// Result of successfully restoring one backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct BackupRestoreResultView {
    /// Backup catalogue id that was restored.
    pub backup_id: String,
    /// File restored to its original location.
    pub path: String,
    /// Journaled operation id for the restore.
    pub operation_id: String,
}

/// Loads unresolved drift rows, live classifications, and watcher state.
#[tauri::command]
pub async fn drift_snapshot(
    app: tauri::AppHandle,
    watcher: State<'_, DriftWatcherControl>,
) -> Result<DriftSnapshotView, CommandError> {
    let status = watcher.status();
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        build_snapshot(&state, status)
    })
    .await
}

/// Reconciles current targets without emitting detection toasts, then reloads the screen.
#[tauri::command]
pub async fn drift_rescan(
    app: tauri::AppHandle,
    watcher: State<'_, DriftWatcherControl>,
) -> Result<DriftSnapshotView, CommandError> {
    let status = watcher.status();
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        {
            let database = state.database().map_err(CommandError::unavailable)?;
            let connection = database.connection();
            let open_events = at_db::drift::open_drift_events(connection, None)
                .map_err(|_| CommandError::internal("could not read open drift events"))?;
            let open_paths = open_events
                .into_iter()
                .map(|event| event.path)
                .collect::<HashSet<_>>();
            let mut targets = targets_from_index(connection).map_err(|_| {
                CommandError::internal("could not read registered deployment targets")
            })?;
            targets
                .retain(|target| !open_paths.contains(&target.path.to_string_lossy().into_owned()));
            let service = DriftService {
                database: &database,
                targets: &targets,
            };
            service
                .reconcile()
                .map_err(|_| CommandError::internal("could not reconcile watched files"))?;
        }
        build_snapshot(&state, status)
    })
    .await
}

/// Pauses or resumes native filesystem watching.
#[tauri::command]
pub fn drift_watcher_set_paused(
    watcher: State<'_, DriftWatcherControl>,
    paused: bool,
) -> Result<DriftWatcherStatusView, CommandError> {
    watcher
        .set_paused(paused)
        .map_err(|_| CommandError::unavailable("the filesystem watcher could not change state"))
}

/// Loads a redacted diff when the recorded baseline bytes are still available.
#[tauri::command]
pub async fn drift_preview(
    app: tauri::AppHandle,
    event_id: i64,
) -> Result<DriftPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        drift_preview_inner(&state, event_id)
    })
    .await
}

/// Bridge-facing body of [`drift_preview`], callable without a Tauri app.
///
/// # Errors
/// See [`drift_preview`].
pub fn drift_preview_inner(
    state: &AppState,
    event_id: i64,
) -> Result<DriftPreviewView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let event = at_db::drift::drift_event(database.connection(), event_id)
        .map_err(|_| CommandError::internal("could not load the drift event"))?
        .filter(DriftEvent::is_open)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The drift item is no longer open.",
            )
        })?;
    let item = item_for_event(database.connection(), &event)?;
    let context = item
        .as_ref()
        .map(|item| store_context(state, &database, item, event.scope))
        .transpose()?;
    let target = ensure_registered_target(database.connection(), &event)?;
    let disk_path = PathBuf::from(&event.path);
    let preview_path = drift_preview_path(&target, &disk_path)?;
    let disk_bytes = read_preview_node(&preview_path)
        .map_err(|_| CommandError::unavailable("the on-disk file cannot be read for preview"))?;
    let canonical_path = context
        .as_ref()
        .and_then(|context| item.as_ref().map(|item| (&context.layout, item)))
        .map(|(layout, item)| item_store_path(layout, item))
        .transpose()?;
    let store_bytes = canonical_path
        .as_deref()
        .map(read_preview_node)
        .transpose()
        .map_err(|_| {
            CommandError::unavailable("the canonical store item cannot be read for preview")
        })?
        .flatten();
    let canonical_hash = match target.content {
        DriftContent::File => store_bytes
            .as_deref()
            .map(at_store::hash_bytes)
            .map(|hash| hash.to_string()),
        DriftContent::RuleFile => store_bytes
            .as_deref()
            .map(at_store::hash_rule_bytes)
            .map(|hash| hash.to_string()),
        DriftContent::SkillFolder => canonical_path
            .as_deref()
            .and_then(|path| at_store::hash_skill_folder(path).ok())
            .map(|hash| hash.to_string()),
        _ => None,
    };
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let (baseline_bytes, baseline_unavailable_reason) = recover_drift_baseline(
        database.connection(),
        paths,
        &event,
        target.content,
        store_bytes.as_deref(),
        canonical_hash.as_deref(),
    )?;
    let previous_version_available = baseline_bytes.is_some();
    let diff = baseline_bytes
        .as_deref()
        .map(|baseline| {
            at_fs::diff::unified(
                &disk_path.display().to_string(),
                Some(baseline),
                disk_bytes.as_deref().unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let (diff, diff_redacted) = redact(state, diff);
    let (on_disk_content, content_redacted) = match disk_bytes {
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(content) => {
                let (content, redacted) = redact(state, content);
                (Some(content), redacted)
            }
            Err(_) => (None, false),
        },
        None => (None, false),
    };
    Ok(DriftPreviewView {
        event_id,
        path: event.path,
        canonical_path: canonical_path.map(|path| path.display().to_string()),
        previous_version_available,
        diff,
        baseline_unavailable_reason,
        on_disk_content,
        redacted: diff_redacted || content_redacted,
    })
}

/// Adopts the changed deployment into the canonical AgenTuna store.
#[tauri::command]
pub async fn drift_adopt(app: tauri::AppHandle, event_id: i64) -> Result<(), CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        drift_adopt_inner(&state, event_id)
    })
    .await
}

/// Bridge-facing body of [`drift_adopt`], callable without a Tauri app.
///
/// # Errors
/// See [`drift_adopt`].
pub fn drift_adopt_inner(state: &AppState, event_id: i64) -> Result<(), CommandError> {
    resolve_drift(state, event_id, true)
}

/// Restores canonical AgenTuna content over the changed deployment.
#[tauri::command]
pub async fn drift_restore(app: tauri::AppHandle, event_id: i64) -> Result<(), CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        drift_restore_inner(&state, event_id)
    })
    .await
}

/// Bridge-facing body of [`drift_restore`], callable without a Tauri app.
///
/// # Errors
/// See [`drift_restore`].
pub fn drift_restore_inner(state: &AppState, event_id: i64) -> Result<(), CommandError> {
    resolve_drift(state, event_id, false)
}

/// Lists the most recent safe-to-inspect file backups.
#[tauri::command]
pub async fn backups_list(app: tauri::AppHandle) -> Result<Vec<BackupRowView>, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        backups_list_inner(&state)
    })
    .await
}

/// Bridge-facing body of [`backups_list`], callable without a Tauri app.
///
/// # Errors
/// See [`backups_list`].
pub fn backups_list_inner(state: &AppState) -> Result<Vec<BackupRowView>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let rows = at_db::backups::list_backups(
        database.connection(),
        at_db::backups::BackupQuery {
            limit: Some(BACKUP_LIST_LIMIT),
            ..Default::default()
        },
    )
    .map_err(|_| CommandError::internal("could not read the backup catalogue"))?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    Ok(rows
        .into_iter()
        .map(|backup| {
            let (restorable, unavailable_reason) = match backup_payload(paths, &backup) {
                Ok(_) => (true, None),
                Err(reason) => (false, Some(reason.to_string())),
            };
            BackupRowView {
                id: backup.id,
                original_path: backup.original_path,
                created_at: backup.created_at,
                size: backup.size,
                operation_id: backup.operation_id,
                restorable,
                unavailable_reason,
            }
        })
        .collect())
}

/// Previews restoring the catalogue's saved bytes, including when the destination was deleted.
#[tauri::command]
pub async fn backup_preview(
    app: tauri::AppHandle,
    backup_id: String,
) -> Result<BackupPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        backup_preview_inner(&state, &backup_id)
    })
    .await
}

/// Bridge-facing body of [`backup_preview`], callable without a Tauri app.
///
/// # Errors
/// See [`backup_preview`].
pub fn backup_preview_inner(
    state: &AppState,
    backup_id: &str,
) -> Result<BackupPreviewView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let backup = backup_by_id(database.connection(), backup_id)?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let old_bytes = backup_payload(paths, &backup)?;
    let original_path = backup.original_path.clone().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "Backup destination is unavailable.",
        )
    })?;
    let path = PathBuf::from(&original_path);
    let current_bytes = read_preview_node(&path)
        .map_err(|_| CommandError::unavailable("the destination cannot be read for preview"))?;
    let diff = at_fs::diff::unified(
        &path.display().to_string(),
        current_bytes.as_deref(),
        &old_bytes,
    );
    let (diff, redacted) = redact(state, diff);
    let destination_hash = current_bytes.as_deref().map(at_fs::sha256_hex);
    let preview_id = preview_identity(
        &backup.id,
        &at_fs::sha256_hex(&old_bytes),
        destination_hash.as_deref(),
    );
    Ok(BackupPreviewView {
        id: backup.id,
        preview_id,
        original_path,
        created_at: backup.created_at,
        diff,
        destination_hash,
        destination_exists: current_bytes.is_some(),
        redacted,
    })
}

/// Restores one verified catalogue backup using a journaled, conflict-checked ChangeSet.
#[tauri::command]
pub async fn backup_restore(
    app: tauri::AppHandle,
    request: BackupRestoreRequest,
) -> Result<BackupRestoreResultView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        backup_restore_inner(&state, &request)
    })
    .await
}

/// Bridge-facing body of [`backup_restore`], callable without a Tauri app.
///
/// # Errors
/// See [`backup_restore`].
pub fn backup_restore_inner(
    state: &AppState,
    request: &BackupRestoreRequest,
) -> Result<BackupRestoreResultView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let backup = backup_by_id(database.connection(), &request.backup_id)?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let bytes = backup_payload(paths, &backup)?;
    let original_path = backup.original_path.clone().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "Backup destination is unavailable.",
        )
    })?;
    let path = PathBuf::from(&original_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(CommandError::invalid_argument(
            "backup_id",
            "backup destination is not a normalized absolute path",
        ));
    }
    let (current_hash, target_state) = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let current = fs::read(&path).map_err(|_| {
                CommandError::unavailable("the current destination cannot be read safely")
            })?;
            let hash = at_fs::sha256_hex(&current);
            let expected = ExpectedState::hash(&hash)
                .map_err(|_| CommandError::internal("the destination hash is invalid"))?;
            (Some(hash), expected)
        }
        Ok(_) => {
            return Err(CommandError::invalid_argument(
                "backup_id",
                "destination is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, ExpectedState::absent()),
        Err(_) => {
            return Err(CommandError::unavailable(
                "the destination cannot be inspected safely",
            ));
        }
    };
    let expected_preview = preview_identity(
        &backup.id,
        &at_fs::sha256_hex(&bytes),
        current_hash.as_deref(),
    );
    if request.preview_id != expected_preview || request.destination_hash != current_hash {
        return Err(CommandError::new(
            CommandErrorCode::ExternalWriteConflict,
            "The destination changed since this backup was previewed. Preview it again before restoring.",
        ));
    }
    let authorized_roots = authorized_roots(&database, paths)?;
    let operation_id = at_db::ops::OperationId::new().to_string();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let change_set = ChangeSet {
        id: operation_id.clone(),
        preview_id: format!("pv_{}", uuid::Uuid::now_v7()),
        operation_kind: OperationKind::parse("restore")
            .map_err(|_| CommandError::internal("backup restore operation kind is unavailable"))?,
        scope: Some(restore_scope(paths, &path)),
        project_id: project_for_path(database.connection(), &path)?,
        summary: "Restore file from backup".to_owned(),
        ops: vec![FileOp::Write {
            path: path.clone(),
            bytes: bytes.clone(),
            expected: target_state,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: u64::try_from(now).unwrap_or_default(),
    };
    let backup_root = paths.backup_dir().to_path_buf();
    let applied = {
        let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: paths.user_store(),
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots(&change_set, &mut context, &authorized_roots).map_err(|error| {
            // A stale destination and an authorization/IO refusal are different problems; the
            // user needs the real one, not a catch-all "it changed".
            CommandError::new(
                CommandErrorCode::Conflict,
                format!("The backup could not be restored: {error}"),
            )
        })?
    };
    at_db::backups::mark_restored(database.connection(), &backup.id)
        .map_err(|_| CommandError::internal("could not update the backup restore history"))?;
    // A restored path can be an agent-native deployment target; its bytes now differ from the
    // baseline the last write recorded.
    at_engine::drift::sync_written_target(database.connection(), None, &path, None).map_err(
        |error| {
            CommandError::internal(format!(
                "deployment baselines could not be refreshed after restore: {error}"
            ))
        },
    )?;
    record_recent_write(state, &path, &bytes);
    state.invalidate_scope_cache();
    Ok(BackupRestoreResultView {
        backup_id: backup.id,
        path: original_path,
        operation_id: applied.operation_id,
    })
}

/// Bridge-facing body of the Sync & drift row projection, callable without a Tauri app.
///
/// The watcher condition is the only part of [`drift_snapshot`] that needs a live watcher, so
/// this body reports it as paused; every row and its classification is the same.
///
/// # Errors
/// See [`drift_snapshot`].
pub fn drift_snapshot_inner(state: &AppState) -> Result<DriftSnapshotView, CommandError> {
    build_snapshot(
        state,
        DriftWatcherStatusView {
            status: "paused".to_owned(),
            path: None,
            error: None,
        },
    )
}

fn build_snapshot(
    state: &AppState,
    watcher: DriftWatcherStatusView,
) -> Result<DriftSnapshotView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    let events = at_db::drift::open_drift_events(connection, None)
        .map_err(|_| CommandError::internal("could not read open drift events"))?;
    let targets = targets_from_index(connection)
        .map_err(|_| CommandError::internal("could not read registered deployment targets"))?;
    let items = at_db::shared::items_by_ids(
        connection,
        &events
            .iter()
            .filter_map(|event| event.item_id.as_deref()?.parse::<ItemId>().ok())
            .collect::<Vec<_>>(),
    )
    .map_err(|_| CommandError::internal("could not read drift item names"))?;
    let item_names = items
        .into_iter()
        .map(|item| (item.id.to_string(), item.name))
        .collect::<std::collections::HashMap<_, _>>();
    let mut rows = Vec::with_capacity(events.len());
    for event in events {
        let target = targets
            .iter()
            .find(|target| target_matches_event(target, &event));
        let kind = target.map_or_else(|| classify_unknown_event(&event), classify_target);
        let item_name = event
            .item_id
            .as_ref()
            .and_then(|id| item_names.get(id))
            .cloned()
            .or_else(|| {
                Path::new(&event.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Unknown item".to_owned());
        let item_exists = event
            .item_id
            .as_ref()
            .is_some_and(|id| item_names.contains_key(id));
        let link_target = target
            .and_then(|target| target.link_target.as_ref())
            .map(|path| path.display().to_string());
        rows.push(DriftRowView {
            event_id: event.id,
            kind,
            item_id: event.item_id.clone(),
            item_name,
            agent: event.agent_id.to_string(),
            scope: event.scope.wire_name().to_owned(),
            path: event.path.clone(),
            detected_at: event.detected_at,
            changed_file_count: 1,
            can_adopt: item_exists && target.is_some(),
            can_restore: item_exists && target.is_some(),
            link_target,
        });
    }
    let in_sync_count = targets
        .iter()
        .filter(|target| target_is_in_sync(target))
        .count();
    Ok(DriftSnapshotView {
        rows,
        in_sync_count,
        watched_count: targets.len(),
        watcher,
    })
}

fn target_matches_event(target: &DriftTarget, event: &DriftEvent) -> bool {
    target.path.to_string_lossy() == event.path
        && target.scope == event.scope
        && target.agent_id == event.agent_id
        && target.item_id.map(|id| id.to_string()).as_deref() == event.item_id.as_deref()
}

fn classify_unknown_event(event: &DriftEvent) -> String {
    match fs::symlink_metadata(&event.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => "file_deleted".to_owned(),
        Err(_) => "unreadable".to_owned(),
        Ok(metadata) if metadata.file_type().is_symlink() => "drifted".to_owned(),
        Ok(_) if event.actual_hash.is_none() => "unreadable".to_owned(),
        Ok(_) => "drifted".to_owned(),
    }
}

fn classify_target(target: &DriftTarget) -> String {
    let metadata = match fs::symlink_metadata(&target.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return if target.link_target.is_some() {
                "link_removed"
            } else {
                "file_deleted"
            }
            .to_owned();
        }
        Err(_) => return "unreadable".to_owned(),
    };
    if target.link_target.is_some() && !metadata.file_type().is_symlink() {
        return "link_replaced_by_file".to_owned();
    }
    let path = if metadata.file_type().is_symlink() {
        match fs::read_link(&target.path) {
            Ok(path) if path.is_absolute() => path,
            Ok(path) => target
                .path
                .parent()
                .map_or(path.clone(), |parent| parent.join(path)),
            Err(_) => return "unreadable".to_owned(),
        }
    } else {
        target.path.clone()
    };
    let Some(actual_hash) = hash_target(&path, target.content) else {
        return "unreadable".to_owned();
    };
    if target.expected_hash.as_deref() == Some(actual_hash.as_str()) {
        return "drifted".to_owned();
    }
    if target.store_side_of_link {
        "store_file_edited_while_linked".to_owned()
    } else {
        "drifted".to_owned()
    }
}

fn target_is_in_sync(target: &DriftTarget) -> bool {
    let Ok(metadata) = fs::symlink_metadata(&target.path) else {
        return false;
    };
    let path = if metadata.file_type().is_symlink() {
        match fs::read_link(&target.path) {
            Ok(path) if path.is_absolute() => path,
            Ok(path) => target
                .path
                .parent()
                .map_or(path.clone(), |parent| parent.join(path)),
            Err(_) => return false,
        }
    } else {
        target.path.clone()
    };
    target.expected_hash.as_deref() == hash_target(&path, target.content).as_deref()
}

fn hash_target(path: &Path, content: DriftContent) -> Option<String> {
    match content {
        DriftContent::File => fs::read(path)
            .ok()
            .map(|bytes| at_store::hash_bytes(&bytes).to_string()),
        DriftContent::SkillFolder => at_store::hash_skill_folder(path)
            .ok()
            .map(|hash| hash.to_string()),
        DriftContent::RuleFile => fs::read(path)
            .ok()
            .map(|bytes| at_store::hash_rule_bytes(&bytes).to_string()),
        _ => None,
    }
}

fn item_for_event(
    connection: &rusqlite::Connection,
    event: &DriftEvent,
) -> Result<Option<at_db::shared::ItemRow>, CommandError> {
    let Some(raw_id) = event.item_id.as_deref() else {
        return Ok(None);
    };
    let Ok(item_id) = ItemId::from_str(raw_id) else {
        return Ok(None);
    };
    at_db::shared::items_by_ids(connection, &[item_id])
        .map(|mut items| items.pop())
        .map_err(|_| CommandError::internal("could not load the canonical drift item"))
}

struct StoreContext {
    paths: at_store::paths::AppPaths,
    layout: StoreLayout,
}

fn store_context(
    state: &AppState,
    database: &at_db::Database,
    item: &at_db::shared::ItemRow,
    scope: Scope,
) -> Result<StoreContext, CommandError> {
    let mut paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?
        .clone();
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load store path settings"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    if matches!(scope, Scope::Project | Scope::Local) {
        let project_id = item.project_id.as_deref().ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The project store is unavailable.",
            )
        })?;
        let project = at_db::projects::find_project_by_id(database.connection(), project_id)
            .map_err(|_| CommandError::internal("could not read the drift project's path"))?
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::NotFound,
                    "The project is no longer registered.",
                )
            })?;
        paths = paths
            .for_project(Path::new(&project.path))
            .map_err(|_| CommandError::unavailable("the project store is unavailable"))?;
        paths.apply_settings_global_store(settings.global_store_path());
    }
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("the canonical store is unavailable"))?;
    Ok(StoreContext { paths, layout })
}

fn item_store_path(
    layout: &StoreLayout,
    item: &at_db::shared::ItemRow,
) -> Result<PathBuf, CommandError> {
    match item.kind {
        ItemKind::Skill => {
            let slug = Slug::parse(&item.slug)
                .map_err(|_| CommandError::internal("the canonical skill slug is invalid"))?;
            Ok(layout.skill_dir(&slug))
        }
        ItemKind::Rule => {
            let relative = item.source_path.as_deref().map(Path::new).map_or_else(
                || PathBuf::from("rules").join(&item.slug),
                Path::to_path_buf,
            );
            if relative.is_absolute()
                || relative.components().any(|part| {
                    matches!(
                        part,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            {
                return Err(CommandError::internal("the indexed rule path is invalid"));
            }
            let path = layout.root().join(relative);
            if !path.starts_with(layout.root()) {
                return Err(CommandError::internal(
                    "the indexed rule path is outside its store",
                ));
            }
            Ok(path)
        }
        ItemKind::McpServer => Ok(layout.mcp_file()),
    }
}

fn ensure_registered_target(
    connection: &rusqlite::Connection,
    event: &DriftEvent,
) -> Result<DriftTarget, CommandError> {
    let item_id = event
        .item_id
        .as_deref()
        .and_then(|id| id.parse::<ItemId>().ok());
    let targets = targets_from_index(connection)
        .map_err(|_| CommandError::internal("could not read registered deployment targets"))?;
    targets
        .into_iter()
        .find(|target| {
            target.path.to_string_lossy() == event.path
                && target.scope == event.scope
                && target.agent_id == event.agent_id
                && target.item_id == item_id
        })
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "The drift target is no longer registered.",
            )
        })
}

fn resolve_drift(state: &AppState, event_id: i64, adopt: bool) -> Result<(), CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let event = at_db::drift::drift_event(database.connection(), event_id)
        .map_err(|_| CommandError::internal("could not load the drift event"))?
        .filter(DriftEvent::is_open)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The drift item is no longer open.",
            )
        })?;
    let item = item_for_event(database.connection(), &event)?.ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The canonical item is no longer available.",
        )
    })?;
    let context = store_context(state, &database, &item, event.scope)?;
    let target = ensure_registered_target(database.connection(), &event)?;
    let canonical = item_store_path(&context.layout, &item)?;
    let roots = authorized_roots(&database, &context.paths)?;
    let backup_root = context.paths.backup_dir().to_path_buf();
    let request = at_engine::drift::DriftAdoptRequest {
        event_id,
        store_root: context.layout.root(),
        backup_root: &backup_root,
        authorized_roots: &roots,
        connection: database.connection(),
        actor: at_db::ops::OperationActor::Ui,
    };
    let outcome = if adopt {
        at_engine::drift::adopt_drift(request)
    } else {
        at_engine::drift::restore_drift(at_engine::drift::DriftRestoreRequest {
            event_id,
            store_root: context.layout.root(),
            backup_root: &backup_root,
            authorized_roots: &roots,
            connection: database.connection(),
            home: context
                .paths
                .home_dir()
                .unwrap_or_else(|| context.paths.user_store()),
            actor: at_db::ops::OperationActor::Ui,
        })
    }
    .map_err(|_| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "The drift item changed or could not be resolved safely.",
        )
    })?;
    for path in outcome.touched_paths {
        if let Ok(bytes) = fs::read(&path) {
            state
                .recent_writes()
                .record(path, at_fs::sha256_hex(&bytes));
        }
    }
    // The canonical copy and the deployment now agree by construction. Recording that as the
    // new baseline is what keeps the next reconcile from reporting our own resolution as a
    // fresh external edit.
    at_engine::drift::sync_written_target(
        database.connection(),
        Some(event.scope),
        &target.path,
        Some(&canonical),
    )
    .map_err(|error| {
        CommandError::internal(format!(
            "the resolved drift baseline could not be recorded: {error}"
        ))
    })?;
    state.invalidate_scope_cache();
    Ok(())
}

/// Scope a restored file belongs to, inferred from the store roots it sits under.
///
/// The safe-write pipeline needs a scope for the backup it takes before replacing the
/// destination. A path inside the Global store belongs to Global, one inside the project tree to
/// Project, and every other native agent file is a User-level configuration file.
fn restore_scope(paths: &at_store::paths::AppPaths, path: &Path) -> Scope {
    let under = |root: &Path| path.starts_with(root);
    if under(paths.global_store()) {
        Scope::Global
    } else if paths
        .project_store()
        .and_then(|store| store.parent())
        .is_some_and(under)
    {
        Scope::Project
    } else {
        Scope::User
    }
}

fn backup_by_id(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<at_db::backups::Backup, CommandError> {
    at_db::backups::backup_by_id(connection, id)
        .map_err(|_| CommandError::internal("could not read the backup catalogue"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The backup is no longer available.",
            )
        })
}

/// Content-bound identity of one backup-restore preview.
///
/// Covers the backup, its verified payload and the destination bytes the user reviewed, so a
/// restore issued against a different destination state is refused as stale.
fn preview_identity(backup_id: &str, payload_hash: &str, destination_hash: Option<&str>) -> String {
    let destination = destination_hash.unwrap_or("absent");
    format!(
        "bkpv1_{}",
        at_fs::sha256_hex(format!("{backup_id}\n{payload_hash}\n{destination}\n").as_bytes())
    )
}

fn backup_payload(
    paths: &at_store::paths::AppPaths,
    backup: &at_db::backups::Backup,
) -> Result<Vec<u8>, CommandError> {
    if backup.content_hash.is_none() {
        return Err(CommandError::new(
            CommandErrorCode::NotFound,
            "Backup payload has no integrity hash and cannot be restored safely.",
        ));
    }
    let path = backup
        .backup_path
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            CommandError::new(CommandErrorCode::NotFound, "Backup payload is unavailable.")
        })?;
    let root = fs::canonicalize(paths.backup_dir())
        .map_err(|_| CommandError::unavailable("the backup directory is unavailable"))?;
    let canonical_path = fs::canonicalize(&path).map_err(|_| {
        CommandError::new(CommandErrorCode::NotFound, "Backup payload is unavailable.")
    })?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| {
        CommandError::new(CommandErrorCode::NotFound, "Backup payload is unavailable.")
    })?;
    if !canonical_path.starts_with(&root)
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
    {
        return Err(CommandError::invalid_argument(
            "backup_id",
            "backup payload is not a safe regular file",
        ));
    }
    let bytes = fs::read(&canonical_path)
        .map_err(|_| CommandError::unavailable("the backup payload cannot be read"))?;
    if backup
        .content_hash
        .as_deref()
        .is_some_and(|hash| at_fs::sha256_hex(&bytes) != hash)
        || backup
            .size
            .is_some_and(|size| i64::try_from(bytes.len()).ok() != Some(size))
    {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The backup payload failed its integrity check.",
        ));
    }
    Ok(bytes)
}

fn authorized_roots(
    database: &at_db::Database,
    paths: &at_store::paths::AppPaths,
) -> Result<at_fs::ApplyRoots, CommandError> {
    let mut roots = Vec::new();
    push_existing_directory(&mut roots, paths.user_store());
    push_existing_directory(&mut roots, paths.global_store());
    if let Some(project_store) = paths.project_store() {
        push_existing_directory(&mut roots, project_store);
    }
    push_existing_directory(&mut roots, paths.backup_dir());
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not read registered agent paths"))?;
    for agent in agents {
        let paths = at_db::agents::list_agent_paths(database.connection(), &agent.id)
            .map_err(|_| CommandError::internal("could not read registered agent paths"))?;
        for candidate in paths.into_iter().filter_map(|entry| entry.path) {
            push_path_parent(&mut roots, &candidate);
        }
    }
    let projects = at_db::projects::list_projects(database.connection())
        .map_err(|_| CommandError::internal("could not read registered projects"))?;
    for project in projects {
        push_existing_directory(&mut roots, Path::new(&project.path));
    }
    at_fs::ApplyRoots::new(roots)
        .map_err(|_| CommandError::unavailable("authorized filesystem roots are unavailable"))
}

fn push_path_parent(roots: &mut Vec<PathBuf>, path: &Path) {
    let candidate = if path.is_dir() {
        path
    } else if let Some(parent) = path.parent() {
        parent
    } else {
        return;
    };
    push_existing_directory(roots, candidate);
}

fn push_existing_directory(roots: &mut Vec<PathBuf>, path: &Path) {
    if path != Path::new("/") && path.is_dir() && !roots.iter().any(|root| root == path) {
        roots.push(path.to_path_buf());
    }
}

fn project_for_path(
    connection: &rusqlite::Connection,
    path: &Path,
) -> Result<Option<String>, CommandError> {
    let projects = at_db::projects::list_projects(connection)
        .map_err(|_| CommandError::internal("could not read registered projects"))?;
    Ok(projects
        .into_iter()
        .filter(|project| path.starts_with(Path::new(&project.path)))
        .max_by_key(|project| project.path.len())
        .map(|project| project.id))
}

fn drift_preview_path(
    target: &DriftTarget,
    deployment_path: &Path,
) -> Result<PathBuf, CommandError> {
    let metadata = match fs::symlink_metadata(deployment_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(deployment_path.to_path_buf());
        }
        Err(_) => {
            return Err(CommandError::unavailable(
                "the on-disk path cannot be inspected safely",
            ));
        }
    };
    if !metadata.file_type().is_symlink() {
        return Ok(deployment_path.to_path_buf());
    }
    let actual_link = fs::read_link(deployment_path)
        .map_err(|_| CommandError::unavailable("the on-disk link cannot be inspected safely"))?;
    let actual_target = absolute_link_target(deployment_path, &actual_link);
    let expected = target.link_target.as_deref().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "The on-disk link is not registered.",
        )
    })?;
    let expected_target = absolute_link_target(deployment_path, expected);
    if !same_link_target(&actual_target, &expected_target) {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The on-disk link points somewhere unexpected and was not followed.",
        ));
    }
    Ok(actual_target)
}

/// Resolves one link target against the link's parent and normalizes it lexically.
///
/// `read_link` returns the target exactly as stored, which may be relative, such as
/// `../../.agentuna/skills/x`. Comparing that raw path with the registered absolute target
/// reported a managed relative link as a conflict, so `.` and `..` components are folded before
/// the comparison. Normalization is lexical: it never touches the filesystem and never lets a
/// component climb above the path root.
fn absolute_link_target(link_path: &Path, target: &Path) -> PathBuf {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .map_or_else(|| target.to_path_buf(), |parent| parent.join(target))
    };
    normalize_lexically(&resolved)
}

/// Folds `.` and `..` components without resolving symlinks or leaving the path root.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

/// Whether two absolute link targets name the same filesystem entry.
///
/// A managed link may store its target relatively while the deployment row stores it absolutely;
/// after lexical normalization they are equal. Canonicalizing both sides also accepts a target
/// that differs only through a symlinked parent directory. A target that resolves elsewhere
/// stays a conflict.
fn same_link_target(actual: &Path, expected: &Path) -> bool {
    if actual == expected {
        return true;
    }
    match (fs::canonicalize(actual), fs::canonicalize(expected)) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => false,
    }
}

fn read_preview_node(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("preview does not follow symbolic links"));
    }
    if metadata.is_file() {
        if usize::try_from(metadata.len()).unwrap_or(usize::MAX) > PREVIEW_MAX_BYTES {
            return Err(io::Error::other("preview is too large"));
        }
        return fs::read(path).map(Some);
    }
    if metadata.is_dir() {
        let mut bytes = Vec::new();
        read_preview_tree(path, path, &mut bytes)?;
        return Ok(Some(bytes));
    }
    Err(io::Error::other(
        "preview target is not a regular file or directory",
    ))
}

fn read_preview_tree(root: &Path, current: &Path, output: &mut Vec<u8>) -> io::Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| io::Error::other("preview entry escaped its root"))?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            append_preview_bytes(
                output,
                format!("--- {} ---\n[symbolic link omitted]\n", relative.display()).as_bytes(),
            )?;
        } else if metadata.is_dir() {
            read_preview_tree(root, &path, output)?;
        } else if metadata.is_file() {
            let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if output.len().saturating_add(size) > PREVIEW_MAX_BYTES {
                return Err(io::Error::other("preview is too large"));
            }
            append_preview_bytes(
                output,
                format!("--- {} ---\n", relative.display()).as_bytes(),
            )?;
            append_preview_bytes(output, &fs::read(path)?)?;
            if !output.ends_with(b"\n") {
                append_preview_bytes(output, b"\n")?;
            }
        }
    }
    Ok(())
}

fn append_preview_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    if output.len().saturating_add(bytes.len()) > PREVIEW_MAX_BYTES {
        return Err(io::Error::other("preview is too large"));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn redact(state: &AppState, diff: String) -> (String, bool) {
    let Some(logging) = state.logging() else {
        return (diff, false);
    };
    (logging.redactor().redact(&diff), true)
}

fn record_recent_write(state: &AppState, path: &Path, bytes: &[u8]) {
    state
        .recent_writes()
        .record(path.to_path_buf(), at_fs::sha256_hex(bytes));
}

/// Opens the selected registered drift target in the system text editor.
#[tauri::command]
pub async fn drift_open_in_editor(
    app: tauri::AppHandle,
    event_id: i64,
) -> Result<(), CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        drift_open_in_editor_inner(&state, event_id)
    })
    .await
}

/// Bridge-facing body of [`drift_open_in_editor`], callable without a Tauri app.
///
/// # Errors
/// See [`drift_open_in_editor`].
pub fn drift_open_in_editor_inner(state: &AppState, event_id: i64) -> Result<(), CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let event = at_db::drift::drift_event(database.connection(), event_id)
        .map_err(|_| CommandError::internal("could not load the drift event"))?
        .filter(DriftEvent::is_open)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The drift item is no longer open.",
            )
        })?;
    let target = ensure_registered_target(database.connection(), &event)?;
    let mut path = target.path.clone();
    if target.content == DriftContent::SkillFolder {
        path.push(at_model::skill::SKILL_FILE_NAME);
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The drift file is not available to open.",
        )
    })?;
    if !metadata.is_file() {
        return Err(CommandError::invalid_argument(
            "event_id",
            "the drift target is not a regular file",
        ));
    }
    if metadata.file_type().is_symlink() {
        let link = fs::read_link(&path).map_err(|_| {
            CommandError::unavailable("the drift file link cannot be inspected safely")
        })?;
        let resolved = if link.is_absolute() {
            link
        } else {
            path.parent()
                .map_or(link.clone(), |parent| parent.join(link))
        };
        if target.link_target.as_deref() != Some(resolved.as_path()) {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "The drift file link points somewhere unexpected and was not opened.",
            ));
        }
    }
    open_in_text_editor(&path)
}

fn open_in_text_editor(path: &Path) -> Result<(), CommandError> {
    #[cfg(target_os = "macos")]
    let (tool, args) = {
        let tool = if Path::new("/usr/bin/open").exists() {
            "/usr/bin/open"
        } else {
            "open"
        };
        (
            tool,
            vec![
                std::ffi::OsString::from("-t"),
                path.as_os_str().to_os_string(),
            ],
        )
    };
    #[cfg(target_os = "windows")]
    let (tool, args) = (
        "C:\\Windows\\notepad.exe",
        vec![path.as_os_str().to_os_string()],
    );
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let (tool, args) = {
        let tool = if Path::new("/usr/bin/xdg-open").exists() {
            "/usr/bin/xdg-open"
        } else {
            "xdg-open"
        };
        (tool, vec![path.as_os_str().to_os_string()])
    };

    let mut child = Command::new(tool)
        .args(&args)
        .spawn()
        .map_err(|_| CommandError::unavailable("the system text editor could not be opened"))?;

    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    });

    Ok(())
}
fn recover_drift_baseline(
    connection: &rusqlite::Connection,
    paths: &at_store::paths::AppPaths,
    event: &DriftEvent,
    content: DriftContent,
    canonical_bytes: Option<&[u8]>,
    canonical_hash: Option<&str>,
) -> Result<(Option<Vec<u8>>, Option<String>), CommandError> {
    if baseline_matches(event.expected_hash.as_deref(), canonical_hash)
        && let Some(bytes) = canonical_bytes
    {
        return Ok((Some(bytes.to_vec()), None));
    }
    let Some(expected_hash) = event.expected_hash.as_deref() else {
        return Ok((
            None,
            Some(String::from(
                "No baseline hash was recorded for this drift event.",
            )),
        ));
    };
    if !matches!(content, DriftContent::File | DriftContent::RuleFile) {
        return Ok((
            None,
            Some(String::from(
                "The retained baseline is not recoverable as one file.",
            )),
        ));
    }
    let backups =
        at_db::backups::backups_for_path(connection, &event.path, Some(BACKUP_LIST_LIMIT))
            .map_err(|_| CommandError::internal("could not query drift baseline backups"))?;
    let mut matching_backup_found = false;
    for backup in backups {
        if backup.content_hash.as_deref() != Some(expected_hash) {
            continue;
        }
        matching_backup_found = true;
        let Ok(bytes) = backup_payload(paths, &backup) else {
            continue;
        };
        let recovered_hash = match content {
            DriftContent::File => at_store::hash_bytes(&bytes).to_string(),
            DriftContent::RuleFile => at_store::hash_rule_bytes(&bytes).to_string(),
            _ => continue,
        };
        if recovered_hash == expected_hash {
            return Ok((Some(bytes), None));
        }
    }
    let reason = if matching_backup_found {
        "A matching backup is unavailable or failed its integrity check."
    } else {
        "No retained backup matches the recorded baseline hash."
    };
    Ok((None, Some(String::from(reason))))
}

fn baseline_matches(expected_hash: Option<&str>, current_hash: Option<&str>) -> bool {
    expected_hash.is_some_and(|expected| current_hash == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::{baseline_matches, recover_drift_baseline};
    use at_model::{Agent, AgentId, Scope};

    #[test]
    fn baseline_content_recovers_from_a_verified_backup_when_the_canonical_copy_has_changed() {
        let temporary = tempfile::tempdir().expect("temporary sandbox");
        let paths = at_store::paths::AppPaths::resolve(&at_store::paths::PathEnvironment {
            home: at_store::paths::HomeSource::Injected(temporary.path().to_path_buf()),
            overrides: at_store::paths::PathOverrides {
                home: Some(temporary.path().join("support")),
                ..Default::default()
            },
            project_root: None,
        })
        .expect("sandbox paths");
        paths.create_dirs().expect("sandbox directories");
        let database = at_db::Database::open_in_memory().expect("database");
        let agent = AgentId::ClaudeCode;
        at_db::agents::upsert_agent(
            database.connection(),
            &Agent {
                id: agent.clone(),
                display_name: String::from("Claude Code"),
                detected: true,
                version: None,
            },
        )
        .expect("registered agent");

        let original_path = temporary.path().join("native.json");
        let original_path_text = original_path.to_string_lossy().into_owned();
        let baseline = br#"{"mcpServers":{"github":{}}"#;
        let baseline_hash = at_store::hash_bytes(baseline).to_string();
        let backup_path = paths.backup_dir().join("baseline.json");
        let backup_path_text = backup_path.to_string_lossy().into_owned();
        std::fs::write(&backup_path, baseline).expect("baseline backup");
        at_db::backups::record_backup(
            database.connection(),
            &at_db::backups::NewBackup::new(&original_path_text, &backup_path_text)
                .with_content(Some(&baseline_hash), Some(baseline.len() as i64)),
        )
        .expect("catalogued backup");
        let event_id = at_db::drift::record_drift(
            database.connection(),
            &at_db::drift::NewDriftEvent::new(&agent, Scope::User, &original_path_text)
                .with_hashes(Some(&baseline_hash), Some("external")),
        )
        .expect("drift event");
        let event = at_db::drift::drift_event(database.connection(), event_id)
            .expect("drift query")
            .expect("drift event exists");
        let changed_canonical = br#"{"mcpServers":{"github":{"new":true}}}"#;

        let (recovered, reason) = recover_drift_baseline(
            database.connection(),
            &paths,
            &event,
            at_engine::drift::DriftContent::File,
            Some(changed_canonical),
            Some(&at_store::hash_bytes(changed_canonical).to_string()),
        )
        .expect("baseline recovery");

        assert_eq!(recovered.as_deref(), Some(baseline.as_slice()));
        assert_eq!(reason, None);
        database
            .connection()
            .execute("DELETE FROM backups", [])
            .expect("remove retained baseline");
        let (unavailable, reason) = recover_drift_baseline(
            database.connection(),
            &paths,
            &event,
            at_engine::drift::DriftContent::File,
            Some(changed_canonical),
            Some(&at_store::hash_bytes(changed_canonical).to_string()),
        )
        .expect("baseline availability");
        assert_eq!(unavailable, None);
        assert_eq!(
            reason.as_deref(),
            Some("No retained backup matches the recorded baseline hash.")
        );
    }

    #[test]
    fn baseline_hash_matches_only_exact_recorded_content() {
        assert!(baseline_matches(Some("baseline"), Some("baseline")));
        assert!(!baseline_matches(Some("baseline"), Some("newer")));
        assert!(!baseline_matches(Some("baseline"), None));
        assert!(!baseline_matches(None, None));
    }
}
