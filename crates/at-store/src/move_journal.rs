//! TF-042: the durable journal behind the move-aside refusal remedy.
//!
//! `at_fs::remedies::move_aside` moves an unmanaged file or directory into
//! `<support>/.moved-aside/<timestamp>/<encoded path>` and asks a [`MoveJournal`] to record the
//! move before any byte changes. This module is that journal:
//!
//! * one `operations` row (`kind = move`) is the history entry. It is `running` before the
//!   rename, then `succeeded`, `failed` or `undone`;
//! * one JSON record in `<support>/moves/<operation id>.json` keeps the inverse mapping
//!   (`from` → `to`) and the strategy the move needed.
//!
//! Both are required: `operation_files` stores one path per row and no strategy, so it cannot
//! express "this entry came from that path by a verified copy". The JSON record is the same
//! shape of machine-local journal the apply pipeline already writes, and it is never inside a
//! canonical store, so a move-aside entry can never be committed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use at_db::Database;
use at_fs::{
    ExpectedState, FsError, InverseMove, MoveAction, MoveDraft, MoveJournal, MoveOperationId,
    MoveRecord, MoveStrategy, WriteRequest, apply, sha256_hex,
};
use serde::{Deserialize, Serialize};

/// Directory under the support root that holds move-aside journal records.
pub const MOVE_JOURNAL_DIR: &str = "moves";

/// The stored form of one move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredMove {
    /// `operations.id` of the row this record belongs to.
    operation_id: String,
    /// [`MoveAction::as_db`] value.
    action: String,
    /// Path that was moved.
    original: PathBuf,
    /// Where it went.
    moved_to: PathBuf,
    /// [`MoveStrategy::as_db`] value; absent until the move finished.
    strategy: Option<String>,
}

impl StoredMove {
    fn record(&self) -> Result<MoveRecord, FsError> {
        let action = match self.action.as_str() {
            "moved" => MoveAction::Moved,
            other => {
                return Err(FsError::InvalidManifest {
                    path: self.moved_to.clone(),
                    reason: format!("unknown move action `{other}`"),
                });
            }
        };
        let Some(strategy) = self.strategy.as_deref() else {
            return Err(FsError::NotFound {
                path: self.moved_to.clone(),
            });
        };
        let strategy = match strategy {
            "rename" => MoveStrategy::Rename,
            "copy_verified" => MoveStrategy::CopyVerified,
            other => {
                return Err(FsError::InvalidManifest {
                    path: self.moved_to.clone(),
                    reason: format!("unknown move strategy `{other}`"),
                });
            }
        };
        Ok(MoveRecord {
            operation_id: MoveOperationId::new(self.operation_id.clone()),
            action,
            original: self.original.clone(),
            moved_to: self.moved_to.clone(),
            strategy,
            inverse: InverseMove {
                from: self.moved_to.clone(),
                to: self.original.clone(),
            },
        })
    }
}

/// A [`MoveJournal`] backed by the shared SQLite index plus one JSON record per move.
#[derive(Debug, Clone)]
pub struct MoveJournalStore {
    database: Arc<Mutex<Database>>,
    directory: PathBuf,
    actor: at_db::ops::OperationActor,
}

impl MoveJournalStore {
    /// A journal that writes its records into `directory` and rows through `database`.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>, directory: impl Into<PathBuf>) -> Self {
        Self {
            database,
            directory: directory.into(),
            actor: at_db::ops::OperationActor::Ui,
        }
    }

    /// A journal whose records live in `<support>/moves`.
    #[must_use]
    pub fn in_support(database: Arc<Mutex<Database>>, support_dir: impl AsRef<Path>) -> Self {
        Self::new(database, support_dir.as_ref().join(MOVE_JOURNAL_DIR))
    }

    /// Records rows as `actor` instead of the desktop app.
    #[must_use]
    pub fn with_actor(mut self, actor: at_db::ops::OperationActor) -> Self {
        self.actor = actor;
        self
    }

    /// Directory holding the JSON records.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn record_path(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    fn write_record(&self, record: &StoredMove) -> Result<(), FsError> {
        let path = self.record_path(&record.operation_id);
        if let Some(parent) = path.parent() {
            at_fs::ensure_dir(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| FsError::Io {
            path: path.clone(),
            source: io::Error::other(error),
        })?;
        // The first write creates the record; `complete` replaces it under the observed hash, so a
        // concurrent writer is reported instead of silently losing its update.
        let request = match fs::read(&path) {
            Ok(existing) => {
                WriteRequest::replace(path, bytes, ExpectedState::Hash(sha256_hex(&existing)))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                WriteRequest::create(path, bytes)
            }
            Err(error) => {
                return Err(FsError::Io {
                    path,
                    source: error,
                });
            }
        };
        apply(&request).map(|_| ())
    }

    fn read_record(&self, operation_id: &str) -> Result<Option<StoredMove>, FsError> {
        let path = self.record_path(operation_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(FsError::Io {
                    path,
                    source: error,
                });
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| FsError::InvalidManifest {
                path,
                reason: error.to_string(),
            })
    }

    fn database(&self) -> Result<std::sync::MutexGuard<'_, Database>, FsError> {
        self.database.lock().map_err(|_| FsError::Io {
            path: self.directory.clone(),
            source: io::Error::other("the index lock is poisoned"),
        })
    }
}

impl MoveJournal for MoveJournalStore {
    fn begin(&self, draft: MoveDraft) -> Result<MoveOperationId, FsError> {
        let summary = format!("Moved {} aside", draft.original.display());
        let operation = {
            let database = self.database()?;
            at_db::ops::begin(
                database.connection(),
                &at_db::ops::NewOperation::new(at_db::ops::OperationKind::Move, self.actor)
                    .with_summary(&summary),
            )
            .map_err(|error| FsError::Io {
                path: self.directory.clone(),
                source: io::Error::other(format!("cannot start the move row: {error}")),
            })?
        };
        let record = StoredMove {
            operation_id: operation.as_str().to_owned(),
            action: draft.action.as_db().to_owned(),
            original: draft.original,
            moved_to: draft.moved_to,
            strategy: None,
        };
        // A journal failure happens before the move, so nothing has changed on disk yet.
        self.write_record(&record)?;
        Ok(MoveOperationId::new(record.operation_id))
    }

    fn complete(
        &self,
        operation_id: &MoveOperationId,
        strategy: MoveStrategy,
    ) -> Result<(), FsError> {
        let mut record =
            self.read_record(operation_id.as_str())?
                .ok_or_else(|| FsError::NotFound {
                    path: PathBuf::from(operation_id.as_str()),
                })?;
        record.strategy = Some(strategy.as_db().to_owned());
        self.write_record(&record)?;
        let database = self.database()?;
        at_db::ops::finish(
            database.connection(),
            &operation_id_from(operation_id)?,
            at_db::ops::OperationStatus::Succeeded,
            None,
        )
        .map(|_| ())
        .map_err(|error| FsError::Io {
            path: self.directory.clone(),
            source: io::Error::other(format!("cannot finish the move row: {error}")),
        })
    }

    fn fail(&self, operation_id: &MoveOperationId, reason: &str) -> Result<(), FsError> {
        let database = self.database()?;
        at_db::ops::finish(
            database.connection(),
            &operation_id_from(operation_id)?,
            at_db::ops::OperationStatus::Failed,
            Some(reason),
        )
        .map(|_| ())
        .map_err(|error| FsError::Io {
            path: self.directory.clone(),
            source: io::Error::other(format!("cannot fail the move row: {error}")),
        })
    }

    fn lookup(&self, operation_id: &MoveOperationId) -> Result<Option<MoveRecord>, FsError> {
        let Some(record) = self.read_record(operation_id.as_str())? else {
            return Ok(None);
        };
        let status = {
            let database = self.database()?;
            at_db::ops::operation_detail(database.connection(), operation_id.as_str())
                .map_err(|error| FsError::Io {
                    path: self.directory.clone(),
                    source: io::Error::other(format!("cannot read the move row: {error}")),
                })?
                .and_then(|detail| detail.operation.status)
        };
        // Only a finished move is undoable; a pending record means the rename never completed.
        if status != Some(at_db::ops::OperationStatus::Succeeded) {
            return Ok(None);
        }
        record.record().map(Some)
    }

    fn mark_undone(&self, operation_id: &MoveOperationId) -> Result<(), FsError> {
        let database = self.database()?;
        at_db::ops::mark_undone(database.connection(), &operation_id_from(operation_id)?)
            .map(|_| ())
            .map_err(|error| FsError::Io {
                path: self.directory.clone(),
                source: io::Error::other(format!("cannot mark the move undone: {error}")),
            })
    }
}

/// Parses a journal id into the `operations` identity it names.
fn operation_id_from(id: &MoveOperationId) -> Result<at_db::ops::OperationId, FsError> {
    at_db::ops::OperationId::parse(id.as_str()).map_err(|error| FsError::InvalidManifest {
        path: PathBuf::from(id.as_str()),
        reason: error.to_string(),
    })
}
