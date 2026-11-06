//! Generic, history-backed operation undo.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_db::retention::{RetentionError, ensure_undoable};
use at_db::{backups, ops};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder, OperationRecorder};
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId};
use rusqlite::Connection;

/// Inputs for undoing one recorded operation.
pub struct UndoOperationRequest<'a> {
    /// Database containing the operation, file rows and backup catalogue.
    pub connection: &'a Connection,
    /// Store root used for journals and operation backups.
    pub store_root: &'a Path,
    /// Backup root used by the operation apply pipeline.
    pub backup_root: &'a Path,
    /// Roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Actor recorded on the new Undo operation.
    pub actor: ops::OperationActor,
    /// Proceed despite files that changed after the operation (`false` by default).
    ///
    /// With `false`, any externally edited path refuses the undo with
    /// [`UndoOutcome::Conflict`] before anything is written. With `true`, those paths are
    /// restored from the operation backups anyway, deliberately replacing the newer edits;
    /// the inverse operations still carry the observed current state, so the at-fs pipeline
    /// refuses a path that changes again between the check and the apply.
    pub force: bool,
}

/// Outcome of undoing a recorded operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UndoOutcome {
    /// Every recorded file effect was reversed.
    Restored {
        /// New operation row id.
        undo_operation_id: String,
    },
    /// The operation is newer on disk than the recorded state.
    Conflict {
        /// Paths left unchanged because their current hash differs.
        paths: Vec<PathBuf>,
    },
    /// The operation is outside the fixed undo window.
    Expired,
    /// The operation already has an Undo child operation.
    AlreadyUndone,
    /// Some operation rows cannot be safely represented by the file undo pipeline.
    Partial {
        /// Paths that could not be restored.
        paths: Vec<PathBuf>,
    },
}

/// Failures querying history or applying the reviewed inverse ChangeSet.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UndoError {
    /// Database access or decoding failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Filesystem apply rejected or rolled back the inverse ChangeSet.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// The system clock predates the Unix epoch.
    #[error("system clock is before the Unix epoch")]
    Clock,
    /// An operation row has no valid ULID.
    #[error("invalid operation id: {0}")]
    OperationId(String),
    /// A backup row needed to restore a file is missing or incomplete.
    #[error("operation backup is missing or incomplete for {0}")]
    MissingBackup(PathBuf),
    /// Retention policy refused or failed the window check.
    #[error("undo window check failed: {0}")]
    Retention(String),
    /// A recorded item id is not a valid AgenTuna item id.
    #[error("invalid item id: {0}")]
    ItemId(String),
    /// A backup could not be read.
    #[error("cannot read operation backup `{path}`: {source}")]
    BackupRead {
        /// Backup path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
}

struct UndoRecorder<'a> {
    connection: &'a Connection,
    actor: ops::OperationActor,
    original: ops::OperationId,
    inner: Option<DbOperationRecorder<'a>>,
}

impl OperationRecorder for UndoRecorder<'_> {
    fn begin(&mut self, set: &ChangeSet) -> Result<String, String> {
        let mut operation = ops::NewOperation::new(ops::OperationKind::Undo, self.actor)
            .with_summary(&set.summary)
            .undoing(&self.original);
        operation.scope = set.scope;
        let id = ops::begin(self.connection, &operation).map_err(|error| error.to_string())?;
        self.inner = Some(DbOperationRecorder::new(self.connection, 30));
        Ok(id.to_string())
    }

    fn file_completed(
        &mut self,
        operation_id: &str,
        path: &Path,
        before_hash: Option<&str>,
        after_hash: Option<&str>,
        backup_path: Option<&Path>,
    ) -> Result<(), String> {
        let recorder = self
            .inner
            .as_mut()
            .ok_or_else(|| "undo operation was not started".to_owned())?;
        recorder
            .file_completed(operation_id, path, before_hash, after_hash, backup_path)
            .map_err(|error| format!("could not record undo file `{}`: {error}", path.display()))
    }

    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String> {
        let recorder = self
            .inner
            .as_mut()
            .ok_or_else(|| "undo operation was not started".to_owned())?;
        recorder.finish(operation_id, succeeded, manual_review)
    }
}

/// Undo an operation after checking the 30-day window and every recorded after-hash.
///
/// The inverse plan is applied through at-fs so the Undo operation has its own journal,
/// backups and file rows. Conflicts are returned before any write is submitted.
///
/// # Errors
/// Returns [`UndoError`] when history is invalid, a backup cannot be read, or the at-fs
/// ChangeSet pipeline refuses the inverse.
pub fn undo_operation(
    request: UndoOperationRequest<'_>,
    operation_id: &str,
) -> Result<UndoOutcome, UndoError> {
    let operation = ops::OperationId::parse(operation_id)
        .map_err(|_| UndoError::OperationId(operation_id.to_owned()))?;
    let detail = match ops::operation_detail(request.connection, operation_id)? {
        Some(detail) => detail,
        None => return Err(UndoError::OperationId(operation_id.to_owned())),
    };
    let already_undone: bool = request
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE undo_of = ?1)",
            [operation_id],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    if already_undone {
        return Ok(UndoOutcome::AlreadyUndone);
    }
    let now = SystemTime::now();
    match ensure_undoable(request.connection, &operation, now) {
        Ok(()) => {}
        Err(RetentionError::UndoExpired { .. }) => return Ok(UndoOutcome::Expired),
        Err(RetentionError::UnknownOperation { .. }) => {
            return Err(UndoError::OperationId(operation_id.to_owned()));
        }
        Err(RetentionError::Db(error)) => return Err(UndoError::Database(error)),
        Err(error) => return Err(UndoError::Retention(error.to_string())),
    }
    let list = ops::undo_file_list_with(request.connection, operation_id, &TreeFileProbe)?
        .ok_or_else(|| UndoError::OperationId(operation_id.to_owned()))?;
    let effective_files = collapse_file_rows(&list.files);
    let conflicts = effective_files
        .iter()
        .filter(|file| file.conflicted)
        .filter_map(|file| file.path.as_ref().map(PathBuf::from))
        .collect::<Vec<_>>();
    if !conflicts.is_empty() && !request.force {
        return Ok(UndoOutcome::Conflict { paths: conflicts });
    }

    let mut inverse = Vec::new();
    let mut incomplete = Vec::new();
    for file in effective_files.iter().rev() {
        let Some(path_text) = file.path.as_deref() else {
            incomplete.push(PathBuf::new());
            continue;
        };
        let path = PathBuf::from(path_text);
        let Some(action) = file.inverse else {
            incomplete.push(path);
            continue;
        };
        match action {
            ops::UndoAction::Delete => {
                let Some(hash) = file.recorded_hash.as_deref() else {
                    incomplete.push(path);
                    continue;
                };
                let expected = ExpectedState::hash(hash)
                    .map_err(|_| UndoError::MissingBackup(path.clone()))?;
                inverse.push(FileOp::Delete { path, expected });
            }
            ops::UndoAction::Restore => {
                let Some(backup_id) = file.backup_id.as_deref() else {
                    incomplete.push(path);
                    continue;
                };
                let backup = backups::backup_by_id(request.connection, backup_id)?
                    .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                let blob = backup
                    .backup_path
                    .as_deref()
                    .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                if !Path::new(blob).starts_with(request.backup_root) {
                    incomplete.push(path);
                    continue;
                }
                let metadata =
                    fs::symlink_metadata(blob).map_err(|source| UndoError::BackupRead {
                        path: PathBuf::from(blob),
                        source,
                    })?;
                if metadata.is_dir() || metadata.file_type().is_symlink() {
                    let delete_already_planned = inverse.last().is_some_and(
                        |operation: &FileOp| matches!(operation, FileOp::Delete { path: deleted, .. } if deleted == &path),
                    );
                    if !delete_already_planned {
                        match &file.current {
                            ops::CurrentState::Hash(hash) => inverse.push(FileOp::Delete {
                                path: path.clone(),
                                expected: ExpectedState::hash(hash)
                                    .map_err(|_| UndoError::MissingBackup(path.clone()))?,
                            }),
                            ops::CurrentState::Missing => {}
                            ops::CurrentState::Unverifiable => {
                                incomplete.push(path);
                                continue;
                            }
                        }
                    }
                    // The backup is re-created rather than moved out of the operation folder:
                    // a copy keeps the backup intact and leaves only invertible file rows, so
                    // this Undo operation is itself undoable. A symlink backup is re-created as
                    // the link it was; a tree is copied.
                    if metadata.file_type().is_symlink() {
                        let recorded =
                            fs::read_link(blob).map_err(|source| UndoError::BackupRead {
                                path: PathBuf::from(blob),
                                source,
                            })?;
                        // A link's recorded target is relative to where the link lived, not to
                        // the operation backup folder it was moved into.
                        let target = if recorded.is_absolute() {
                            recorded
                        } else {
                            let parent = path
                                .parent()
                                .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
                            normalize_lexically(&parent.join(recorded))
                        };
                        inverse.push(FileOp::Symlink {
                            link_path: path,
                            target,
                            mode: at_model::DeployMode::Link,
                        });
                    } else {
                        // Plain tree copy: no copy manifest, so the restored tree hashes
                        // exactly like the backed-up original.
                        inverse.push(FileOp::CopyTree {
                            source: PathBuf::from(blob),
                            dest: path,
                            expected: ExpectedState::Absent,
                        });
                    }
                    continue;
                }
                let bytes = fs::read(blob).map_err(|source| UndoError::BackupRead {
                    path: PathBuf::from(blob),
                    source,
                })?;
                let expected = match &file.current {
                    ops::CurrentState::Hash(hash) => ExpectedState::hash(hash)
                        .map_err(|_| UndoError::MissingBackup(path.clone()))?,
                    ops::CurrentState::Missing => ExpectedState::Absent,
                    ops::CurrentState::Unverifiable => {
                        incomplete.push(path);
                        continue;
                    }
                };
                inverse.push(FileOp::Write {
                    path,
                    bytes,
                    expected,
                    follow_symlink: false,
                    backup: true,
                });
            }
            // `operation_files` records a move's destination, never its source, so a move
            // cannot be reversed from history alone. Undo never plans one itself: it copies
            // backups and re-creates links, so its own rows stay invertible.
            ops::UndoAction::Move => incomplete.push(path),
            _ => incomplete.push(path),
        }
    }
    if !incomplete.is_empty() {
        return Ok(UndoOutcome::Partial { paths: incomplete });
    }
    if inverse.is_empty() {
        // Kinds without file effects (scan, settings, an empty restore) still get an audit
        // row, so the operation is marked undone and a second undo is refused.
        let recorded = ops::record(
            request.connection,
            &ops::NewOperation::new(ops::OperationKind::Undo, request.actor)
                .with_scope(detail.operation.scope.unwrap_or(at_model::Scope::User))
                .with_summary(&format!("Undo operation {operation_id} (no file effects)"))
                .undoing(&operation),
            &[],
            ops::OperationStatus::Succeeded,
            None,
        )?;
        return Ok(UndoOutcome::Restored {
            undo_operation_id: recorded.to_string(),
        });
    }
    let created_at_unix_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UndoError::Clock)?
        .as_secs();
    let set = ChangeSet {
        id: at_model::ItemId::new().to_string(),
        preview_id: format!("undo:{operation_id}"),
        operation_kind: at_model::OperationKind::parse("undo")
            .map_err(|error| UndoError::OperationId(error.to_string()))?,
        scope: detail.operation.scope,
        project_id: None,
        summary: format!("Undo operation {operation_id}"),
        ops: inverse,
        created_at_unix_seconds,
    };
    let mut recorder = UndoRecorder {
        connection: request.connection,
        actor: request.actor,
        original: operation,
        inner: None,
    };
    let context = ApplyContext {
        store_root: request.store_root,
        backup_folder: request.backup_root,
        recorder: &mut recorder,
    };
    let removed_store_paths = set
        .ops
        .iter()
        .filter_map(|op| match op {
            FileOp::Delete { path, .. } if path.starts_with(request.store_root) => {
                Some(path.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let outcome = at_fs::apply_with_roots(&set, &mut { context }, request.authorized_roots)?;
    prune_empty_store_dirs(request.store_root, &removed_store_paths);
    if let Some(kind) = detail.operation.kind {
        revert_migration_metadata(request.connection, operation_id, kind)?;
    }
    Ok(UndoOutcome::Restored {
        undo_operation_id: outcome.operation_id,
    })
}

/// Resolves `.` and `..` components without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What a caller needs to decide whether undo is offered and how it will behave.
///
/// Produced by [`undo_readiness`] without writing anything; [`undo_operation`] re-checks
/// everything at undo time, so this is advisory, not an authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UndoReadiness {
    /// The operation row exists.
    pub known: bool,
    /// The operation already has an Undo child operation.
    pub already_undone: bool,
    /// The operation started before the fixed undo window.
    pub expired: bool,
    /// The fixed undo window in days.
    pub window_days: u32,
    /// Unix seconds when the window closes, when the operation exists.
    pub expires_at_unix_seconds: Option<i64>,
    /// Paths that changed after the operation and refuse undo unless forced.
    pub conflicted_paths: Vec<PathBuf>,
}

/// Reads undo readiness for one operation without writing anything.
///
/// # Errors
/// [`UndoError`] when history cannot be queried.
pub fn undo_readiness(
    connection: &Connection,
    operation_id: &str,
) -> Result<UndoReadiness, UndoError> {
    let known = ops::operation_detail(connection, operation_id)?.is_some();
    if !known {
        return Ok(UndoReadiness {
            known: false,
            already_undone: false,
            expired: false,
            window_days: at_db::retention::UNDO_WINDOW_DAYS,
            expires_at_unix_seconds: None,
            conflicted_paths: Vec::new(),
        });
    }
    let already_undone: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE undo_of = ?1)",
            [operation_id],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    let expires_at: Option<String> = connection
        .query_row(
            "SELECT strftime('%s', started_at, ?1) FROM operations WHERE id = ?2",
            rusqlite::params![
                format!("+{} days", at_db::retention::UNDO_WINDOW_DAYS),
                operation_id
            ],
            |row| row.get(0),
        )
        .map_err(|error| UndoError::Database(error.into()))?;
    let expires_at_unix_seconds = expires_at.and_then(|text| text.parse::<i64>().ok());
    let now_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UndoError::Clock)?
        .as_secs();
    let expired = expires_at_unix_seconds
        .is_some_and(|expires| expires <= i64::try_from(now_unix_seconds).unwrap_or(i64::MAX));
    let conflicted_paths = match ops::undo_file_list_with(connection, operation_id, &TreeFileProbe)?
    {
        Some(list) => collapse_file_rows(&list.files)
            .iter()
            .filter(|file| file.conflicted)
            .filter_map(|file| file.path.as_ref().map(PathBuf::from))
            .collect(),
        None => Vec::new(),
    };
    Ok(UndoReadiness {
        known: true,
        already_undone,
        expired,
        window_days: at_db::retention::UNDO_WINDOW_DAYS,
        expires_at_unix_seconds,
        conflicted_paths,
    })
}

/// Removes directories the undo emptied inside the store.
///
/// Deleting the files of a migrated item leaves its folder behind; an empty folder is debris
/// the user did not ask for. Only empty directories strictly below `store_root` are removed.
fn prune_empty_store_dirs(store_root: &Path, removed: &[PathBuf]) {
    let mut candidates = Vec::new();
    for path in removed {
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory == store_root || !directory.starts_with(store_root) {
                break;
            }
            candidates.push(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    candidates.dedup();
    for directory in candidates {
        let _ = fs::remove_dir(&directory);
    }
}

/// Removes the deployment and native adoption rows a Migrate or Adopt operation wrote.
///
/// The inverse ChangeSet has already deleted the store files and restored the native paths, so
/// the metadata rows it wrote must disappear with them: an item that no longer exists on disk
/// must not keep a deployment, and the native rows it adopted become unmanaged findings again.
fn revert_migration_metadata(
    connection: &Connection,
    operation_id: &str,
    kind: ops::OperationKind,
) -> Result<(), UndoError> {
    if !matches!(
        kind,
        ops::OperationKind::Migrate | ops::OperationKind::Adopt
    ) {
        return Ok(());
    }
    let item_ids = at_db::deployments::operation_item_ids(connection, operation_id)?;
    at_db::deployments::delete_for_operation(connection, operation_id)?;
    for text in item_ids {
        let item_id: ItemId = text.parse().map_err(|_| UndoError::ItemId(text.clone()))?;
        if at_db::deployments::count_for_item(connection, &item_id)? == 0 {
            at_db::native::release_adoptions(connection, &item_id)?;
            at_db::items::delete_item(connection, &item_id)?;
        }
    }
    Ok(())
}

struct TreeFileProbe;

impl ops::FileProbe for TreeFileProbe {
    fn current_state(&self, path: &Path) -> ops::CurrentState {
        match at_fs::changeset::hash_node(path) {
            Ok(Some(hash)) => ops::CurrentState::Hash(hash),
            Ok(None) => ops::CurrentState::Missing,
            Err(_) => ops::CurrentState::Unverifiable,
        }
    }
}

fn collapse_file_rows(files: &[ops::UndoFile]) -> Vec<ops::UndoFile> {
    let mut indexes: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut effective: Vec<ops::UndoFile> = Vec::new();
    for file in files {
        let Some(path) = file.path.as_deref() else {
            effective.push(file.clone());
            continue;
        };
        if let Some(index) = indexes.get(path).copied() {
            if let Some(existing) = effective.get_mut(index) {
                existing.recorded_hash.clone_from(&file.recorded_hash);
                existing.current.clone_from(&file.current);
                existing.conflicted = file.conflicted;
            }
        } else {
            indexes.insert(path.to_owned(), effective.len());
            effective.push(file.clone());
        }
    }
    effective
}
