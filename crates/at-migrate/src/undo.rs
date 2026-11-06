//! Generic, history-backed operation undo.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_db::{backups, ops};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder, OperationRecorder};
use at_model::{ChangeSet, ExpectedState, FileOp};
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
        let operation = ops::NewOperation::new(ops::OperationKind::Undo, self.actor)
            .with_summary(&set.summary)
            .undoing(&self.original);
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
    let now_seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UndoError::Clock)?
        .as_secs();
    let started = time::OffsetDateTime::parse(
        &detail.operation.started_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|error| UndoError::OperationId(error.to_string()))?;
    if now_seconds.saturating_sub(started.unix_timestamp().max(0) as u64)
        > u64::from(at_db::retention::UNDO_WINDOW_DAYS) * 86_400
    {
        return Ok(UndoOutcome::Expired);
    }
    let list = ops::undo_file_list_with(request.connection, operation_id, &TreeFileProbe)?
        .ok_or_else(|| UndoError::OperationId(operation_id.to_owned()))?;
    let effective_files = collapse_file_rows(&list.files);
    let conflicts = effective_files
        .iter()
        .filter(|file| file.conflicted)
        .filter_map(|file| file.path.as_ref().map(PathBuf::from))
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
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
                    let backup_hash = at_fs::changeset::hash_node(Path::new(blob))
                        .map_err(|source| UndoError::BackupRead {
                            path: PathBuf::from(blob),
                            source,
                        })?
                        .ok_or_else(|| UndoError::MissingBackup(path.clone()))?;
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
                    inverse.push(FileOp::Move {
                        from: PathBuf::from(blob),
                        to: path,
                        expected: ExpectedState::hash(&backup_hash)
                            .map_err(|_| UndoError::MissingBackup(PathBuf::from(blob)))?,
                    });
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
            ops::UndoAction::Move => incomplete.push(path),
            _ => incomplete.push(path),
        }
    }
    if !incomplete.is_empty() {
        return Ok(UndoOutcome::Partial { paths: incomplete });
    }
    if inverse.is_empty() {
        return Ok(UndoOutcome::Partial { paths: Vec::new() });
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
    let outcome = at_fs::apply_with_roots(&set, &mut { context }, request.authorized_roots)?;
    Ok(UndoOutcome::Restored {
        undo_operation_id: outcome.operation_id,
    })
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
