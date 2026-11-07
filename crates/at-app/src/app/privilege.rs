//! Global scope for the Tauri shell: resolved path, availability, elevation and locks.
//!
//! [`GlobalStoreService`] is the single seam between `AppPaths`, the filesystem gate in
//! [`at_fs::privilege`] and the lock index. It holds no policy of its own:
//!
//! * the path and its source come from `at_store::AppPaths` (never hard-coded here);
//! * writes are refused with a typed read-only error before anything is staged;
//! * elevation runs through the injected [`Elevator`] exactly once, on the explicit
//!   "Enable global writes" action;
//! * locks are always read through [`GlobalLockReader`], i.e. from the `locks` table, and are
//!   reported even when the store is read-only or absent. The Global store is never a lock source.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use at_db::Database;
use at_fs::privilege::{
    Elevator, EnableError, EnableOutcome, GlobalStore, GlobalStoreStatus, GlobalWriteError,
    StoreOwner, enable_global_writes, platform_elevator,
};
use at_fs::{WriteOutcome, WriteRequest};
use at_model::{Lock, Scope};
use at_store::paths::{AppPaths, GlobalStoreSource};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ts_rs::TS;

use super::bridge::OperationsLogError;
use super::commands::CommandError;
use super::state::AppState;

/// Reads Global locks from the `locks` table.
///
/// Implementations must query SQLite and must never read a lock file from the Global store.
/// The production implementation lands with TA-002 (`at-engine::locks`); the shell depends on
/// this trait so the UI path cannot grow a second lock source.
pub trait GlobalLockReader: Send + Sync {
    /// Every lock whose scope is Global, in a stable order.
    ///
    /// # Errors
    /// [`LockReadError`] when the lock index cannot be queried.
    fn global_locks(&self) -> Result<Vec<Lock>, LockReadError>;
}

impl<F> GlobalLockReader for F
where
    F: Fn() -> Result<Vec<Lock>, LockReadError> + Send + Sync,
{
    fn global_locks(&self) -> Result<Vec<Lock>, LockReadError> {
        self()
    }
}

/// Why Global locks could not be read. The message never contains lock contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("could not read Global locks from the locks table: {message}")]
pub struct LockReadError {
    /// Redacted, human-readable explanation.
    pub message: String,
}

impl LockReadError {
    /// Builds an error from a database explanation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Everything the Global Overview card needs, in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalOverview {
    /// Resolved Global store directory.
    pub path: PathBuf,
    /// Where [`Self::path`] came from (`environment`, `settings` or `default`).
    pub source: GlobalStoreSource,
    /// Current availability with its reason.
    pub status: GlobalStoreStatus,
    /// Locks from the `locks` table, shown regardless of [`Self::status`].
    pub locks: Vec<Lock>,
}

impl GlobalOverview {
    /// Whether the read-only banner and "Enable global writes" action are required.
    #[must_use]
    pub fn needs_elevation(&self) -> bool {
        !self.status.is_available()
    }
}

/// One `operations` row for a write that landed inside the Global store.
///
/// The row is the audit trail of an elevated scope: it proves which files changed after the
/// authorization prompt, and it is the only thing a read-only store must never produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalWriteOperation {
    /// Global store root the write targeted.
    pub root: PathBuf,
    /// Files the operation wrote, in write order.
    pub files: Vec<PathBuf>,
}

impl GlobalWriteOperation {
    /// `operations.kind` recorded for this row.
    pub const KIND: &'static str = "global_write";

    /// `operations.status` recorded for this row.
    pub const STATUS: &'static str = "succeeded";
}

/// Records the `operations` row of a Global write.
///
/// TF-009 (`at-db`) supplies the SQLite-backed implementation; [`TracingGlobalOperationLog`] is
/// the default and keeps the row visible in the log until that wiring lands. A failed record never
/// fails the write: the bytes are already in place, so the error is logged instead.
pub trait GlobalOperationLog: Send + Sync {
    /// Records one row.
    ///
    /// # Errors
    /// [`OperationsLogError`] when the row cannot be written.
    fn record(&self, operation: &GlobalWriteOperation) -> Result<(), OperationsLogError>;
}

/// Default [`GlobalOperationLog`]: one structured log record per write.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingGlobalOperationLog;

impl GlobalOperationLog for TracingGlobalOperationLog {
    fn record(&self, operation: &GlobalWriteOperation) -> Result<(), OperationsLogError> {
        tracing::info!(
            kind = GlobalWriteOperation::KIND,
            status = GlobalWriteOperation::STATUS,
            root = %operation.root.display(),
            files = operation.files.len(),
            "Global write recorded"
        );
        Ok(())
    }
}

/// Shell-facing Global scope: path, status, guarded writes, one-shot elevation and locks.
pub struct GlobalStoreService {
    paths: AppPaths,
    elevator: Arc<dyn Elevator>,
    operation_log: Arc<dyn GlobalOperationLog>,
}

impl GlobalStoreService {
    /// Builds the service from already-resolved paths and an authorization runner. Tests inject a
    /// recording [`Elevator`]; production uses [`Self::with_platform_elevator`].
    #[must_use]
    pub fn new(paths: AppPaths, elevator: Arc<dyn Elevator>) -> Self {
        Self {
            paths,
            elevator,
            operation_log: Arc::new(TracingGlobalOperationLog),
        }
    }

    /// Replaces the operation log, e.g. with the `at-db` implementation once TF-009 lands.
    #[must_use]
    pub fn with_operation_log(mut self, operation_log: Arc<dyn GlobalOperationLog>) -> Self {
        self.operation_log = operation_log;
        self
    }

    /// Builds the service with the operating system's consent-based elevator.
    #[must_use]
    pub fn with_platform_elevator(paths: AppPaths) -> Self {
        Self::new(paths, platform_elevator())
    }

    /// Every resolved root, including the settings override applied to the Global store.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// The resolved Global store directory. Displayed by the Global card.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.paths.global_store()
    }

    /// Where [`Self::path`] came from.
    #[must_use]
    pub fn source(&self) -> GlobalStoreSource {
        self.paths.global_store_source()
    }

    /// Applies the `global_store_path` settings value once the database has opened.
    ///
    /// `AGENTUNA_GLOBAL_STORE` still wins and a relative settings value resolves against the
    /// support directory. Returns whether the resolved path changed, so the caller can refresh the
    /// overview; this method never probes or writes the filesystem itself.
    pub fn apply_settings_global_store(&mut self, setting: Option<&Path>) -> bool {
        self.paths.apply_settings_global_store(setting)
    }

    /// Re-probes availability. Cheap enough to call on every overview refresh.
    #[must_use]
    pub fn status(&self) -> GlobalStoreStatus {
        GlobalStoreStatus::probe(self.path())
    }

    /// Reads availability and the Global locks in one call.
    ///
    /// The lock query is independent of the filesystem status: a read-only or absent store still
    /// lists its locks, because locks live in SQLite.
    ///
    /// # Errors
    /// [`LockReadError`] when the lock index cannot be queried.
    pub fn overview(&self, locks: &dyn GlobalLockReader) -> Result<GlobalOverview, LockReadError> {
        Ok(GlobalOverview {
            path: self.path().to_path_buf(),
            source: self.source(),
            status: self.status(),
            locks: locks.global_locks()?,
        })
    }

    /// Runs the explicit "Enable global writes" action: one privileged directory preparation
    /// behind the operating-system authorization prompt.
    ///
    /// No write intent calls this automatically; a read-only store returns a typed error instead.
    ///
    /// # Errors
    /// [`EnableError`] when the path is invalid, the prompt is cancelled or denied, elevation is
    /// unsupported, the privileged command fails, or success is not observable afterwards.
    pub fn enable_global_writes(&self) -> Result<EnableOutcome, EnableError> {
        let outcome =
            enable_global_writes(self.path(), StoreOwner::current(), self.elevator.as_ref())?;
        // The authorization prompt is the audit-worthy event: it changes no item file, so the
        // row carries no file entries and its absence of files is what the summary reports.
        if let Err(error) = self.operation_log.record(&GlobalWriteOperation {
            root: self.path().to_path_buf(),
            files: Vec::new(),
        }) {
            tracing::warn!(%error, "could not record the Global store authorization");
        }
        Ok(outcome)
    }

    /// Writes inside the Global store after the read-only gate.
    ///
    /// # Errors
    /// [`GlobalWriteError::ReadOnly`] / [`GlobalWriteError::Unavailable`] before any temp file is
    /// staged, [`GlobalWriteError::OutsideStore`] for a foreign path, or the underlying
    /// [`at_fs::FsError`].
    pub fn write(&self, request: WriteRequest) -> Result<WriteOutcome, GlobalWriteError> {
        let path = request.path.clone();
        let outcome = GlobalStore::new(self.path()).write(request)?;
        // The bytes are in place; a failed audit row must not turn a successful write into an
        // error, and the read-only gate above has already refused everything that did not land.
        if let Err(error) = self.operation_log.record(&GlobalWriteOperation {
            root: self.path().to_path_buf(),
            files: vec![path],
        }) {
            tracing::warn!(%error, "could not record the Global write operation row");
        }
        Ok(outcome)
    }
}

/// Reads Global locks from the `locks` table through the shared database handle.
///
/// TA-080: the Overview card shows exactly the rows the resolver enforces. A store that is
/// read-only or absent still lists its locks, because locks live in SQLite.
#[derive(Debug, Clone)]
pub struct DatabaseGlobalLockReader {
    database: Arc<Mutex<Database>>,
}

impl DatabaseGlobalLockReader {
    /// Wraps the shared index handle.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self { database }
    }
}

impl GlobalLockReader for DatabaseGlobalLockReader {
    fn global_locks(&self) -> Result<Vec<Lock>, LockReadError> {
        let database = self
            .database
            .lock()
            .map_err(|_| LockReadError::new("lock index is unavailable"))?;
        let rows = at_db::locks::locks_for_scope(database.connection(), Scope::Global)
            .map_err(|error| LockReadError::new(error.to_string()))?;
        rows.into_iter()
            .map(|row| {
                let locked_at = OffsetDateTime::parse(
                    &row.locked_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|error| {
                    LockReadError::new(format!(
                        "lock {} has an unreadable timestamp: {error}",
                        row.id
                    ))
                })?;
                Ok(Lock {
                    scope: row.scope,
                    item_id: row.item_id,
                    reason: row.reason.unwrap_or_default(),
                    locked_by: row.locked_by.unwrap_or_default(),
                    locked_at,
                })
            })
            .collect()
    }
}

/// Records a Global write (or the one-shot authorization that prepared the store) in `operations`.
///
/// The row is the audit trail of an elevated scope: it proves what happened after the OS prompt.
/// A record failure never fails the action that produced it, because the bytes are already in
/// place by then.
#[derive(Debug, Clone)]
pub struct DatabaseGlobalOperationLog {
    database: Arc<Mutex<Database>>,
}

impl DatabaseGlobalOperationLog {
    /// Wraps the shared index handle.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self { database }
    }
}

impl GlobalOperationLog for DatabaseGlobalOperationLog {
    fn record(&self, operation: &GlobalWriteOperation) -> Result<(), OperationsLogError> {
        let database = self
            .database
            .lock()
            .map_err(|_| OperationsLogError::new("operation log is unavailable"))?;
        let kind = at_db::ops::OperationKind::GlobalWrite;
        let actor = at_db::ops::OperationActor::Ui;
        if operation.files.is_empty() {
            at_db::ops::record_completed(
                database.connection(),
                kind,
                actor,
                Some("Enabled Global writes"),
            )
            .map(|_| ())
            .map_err(|error| OperationsLogError::new(error.to_string()))
        } else {
            let paths = operation
                .files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            let files = paths
                .iter()
                .map(|path| at_db::ops::TouchedFile {
                    path,
                    action: at_db::ops::FileAction::Modified,
                    before_hash: None,
                    after_hash: None,
                    backup_id: None,
                    bytes_before: None,
                    bytes_after: None,
                })
                .collect::<Vec<_>>();
            let summary = format!(
                "Wrote {} file(s) into the Global store",
                operation.files.len()
            );
            let new = at_db::ops::NewOperation::new(kind, actor)
                .with_scope(Scope::Global)
                .with_summary(&summary);
            at_db::ops::record(
                database.connection(),
                &new,
                &files,
                at_db::ops::OperationStatus::Succeeded,
                None,
            )
            .map(|_| ())
            .map_err(|error| OperationsLogError::new(error.to_string()))
        }
    }
}

/// One lock row rendered on the Global card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct GlobalLockView {
    /// Locked item.
    pub item_id: String,
    /// Scope holding the pinned definition.
    pub scope: String,
    /// Recorded reason.
    pub reason: String,
}

/// The Global Scope card's payload: resolved path, provenance, availability and locks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct GlobalStoreView {
    /// Resolved Global store directory; never a hard-coded literal.
    pub path: String,
    /// Where the path came from (`environment`, `settings` or `default`).
    pub source: String,
    /// `available`, `read_only` or `unavailable`.
    pub status: String,
    /// Why writes are unavailable, when they are.
    pub reason: Option<String>,
    /// Whether the read-only banner and the "Enable global writes" action are required.
    pub needs_elevation: bool,
    /// Locks read from the `locks` table, shown whatever the status is.
    pub locks: Vec<GlobalLockView>,
}

impl From<GlobalOverview> for GlobalStoreView {
    fn from(overview: GlobalOverview) -> Self {
        Self {
            path: overview.path.display().to_string(),
            source: overview.source.as_str().to_owned(),
            status: overview.status.state().to_owned(),
            reason: overview.status.reason().map(str::to_owned),
            needs_elevation: overview.needs_elevation(),
            locks: overview
                .locks
                .into_iter()
                .map(|lock| GlobalLockView {
                    item_id: lock.item_id.to_string(),
                    scope: lock.scope.to_string(),
                    reason: lock.reason,
                })
                .collect(),
        }
    }
}

/// Builds the shell's Global scope service for the current state.
///
/// # Errors
/// [`super::commands::CommandErrorCode::Unavailable`] when the index or the resolved paths are missing.
pub fn service(state: &AppState) -> Result<GlobalStoreService, CommandError> {
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("database unavailable"))?;
    let settings = {
        let guard = database
            .lock()
            .map_err(|_| CommandError::unavailable("database unavailable"))?;
        at_db::settings::load(guard.connection()).map_err(|error| {
            CommandError::internal(format!("cannot read path settings: {error}"))
        })?
    };
    let mut paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?
        .clone();
    paths.apply_settings_global_store(settings.global_store_path());
    Ok(GlobalStoreService::with_platform_elevator(paths)
        .with_operation_log(Arc::new(DatabaseGlobalOperationLog::new(database))))
}

/// Reads the Global Scope card payload for the current application state.
///
/// # Errors
/// [`super::commands::CommandErrorCode::Unavailable`] when the index or the resolved paths are missing,
/// [`super::commands::CommandErrorCode::Internal`] when the lock rows cannot be read.
pub fn overview_for_state(state: &AppState) -> Result<GlobalStoreView, CommandError> {
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("database unavailable"))?;
    let service = service(state)?;
    let reader = DatabaseGlobalLockReader::new(database);
    service
        .overview(&reader)
        .map(GlobalStoreView::from)
        .map_err(|error| CommandError::internal(error.to_string()))
}

/// Returns the resolved Global store path, availability and the locks the resolver enforces.
#[tauri::command]
pub fn global_store_overview(
    state: tauri::State<'_, AppState>,
) -> Result<GlobalStoreView, CommandError> {
    overview_for_state(&state)
}

/// Runs the explicit "Enable global writes" action and records the authorization in history.
#[tauri::command]
pub fn global_enable_writes(
    state: tauri::State<'_, AppState>,
) -> Result<GlobalStoreView, CommandError> {
    let service = service(&state)?;
    service
        .enable_global_writes()
        .map_err(|_| CommandError::unavailable("Global store authorization failed"))?;
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("database unavailable"))?;
    let reader = DatabaseGlobalLockReader::new(database);
    service
        .overview(&reader)
        .map(GlobalStoreView::from)
        .map_err(|error| CommandError::internal(error.to_string()))
}
