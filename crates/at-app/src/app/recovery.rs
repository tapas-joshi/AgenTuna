//! Crash recovery: what an unclean shutdown left behind, and how it is repaired.
//!
//! [`SessionFile`] records `session.json` (`{ pid, started_at, clean_exit }`) in the support
//! directory. [`RecoveryService::run`] is called once, after [`super::instance::begin`] returned
//! [`super::instance::Startup::Primary`] and before the main window becomes interactive:
//!
//! 1. read the previous session marker and decide whether the last exit was unclean;
//! 2. write this process's marker with `clean_exit: false`, so a crash during recovery is still
//!    detected next time;
//! 3. remove temp files and journals older than [`at_fs::recovery::RETENTION`] unconditionally;
//! 4. on an unclean exit, check the index, re-read every pending journal, mark its operation
//!    interrupted and build the [`RecoverySheet`];
//! 5. log the duration.
//!
//! The sheet offers Restore per file ([`RecoveryService::restore`]), Discard
//! ([`RecoveryService::discard`]) and Continue (close the sheet; nothing is removed and nothing is
//! written). Restore is an ordinary [`at_fs::apply`] write that backs up the current bytes first,
//! so it is undoable like any other operation.
//!
//! ## Why two trait seams
//!
//! `at-app` owns no SQLite handle. The `operations` rows and the `PRAGMA integrity_check` that
//! TF-009 and TF-020 expose in `at-db` arrive through [`RecoveryOperations`] and
//! [`IndexIntegrity`]. Both are `Send + Sync` and injected, so the shell wires the real
//! implementations once and tests inject recording fakes.

use std::sync::Mutex;

use at_db::Database;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use at_fs::WriteOutcome;
use at_fs::backup::BackupMeta;
use at_fs::recovery::{
    self as fs_recovery, DiscardOutcome, EntryState, JournalAssessment, RETENTION, TempFile,
};
use at_store::instance::{ProcessProbe, ProcessState, SystemProbe};
use at_store::layout::{JOURNALS_DIR, STATE_DIR};
use at_store::paths::AppPaths;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// File name of the session marker in the support directory.
pub const SESSION_FILE: &str = "session.json";

/// Suffix of the file the marker is written through before it is renamed into place.
const SESSION_STAGING_FILE: &str = "session.json.next";

/// Directory under the cache root that holds journals of in-flight writes.
pub const PENDING_DIR: &str = "pending";

/// A previous session's record of itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMarker {
    /// Process id of the instance that wrote the marker.
    pub pid: u32,
    /// RFC 3339 time the instance started.
    pub started_at: String,
    /// Whether that instance reached its exit hook.
    pub clean_exit: bool,
}

impl SessionMarker {
    /// A marker for the current process, marked unclean until [`SessionFile::mark_clean`] runs.
    #[must_use]
    pub fn for_current_process() -> Self {
        Self {
            pid: std::process::id(),
            started_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            clean_exit: false,
        }
    }
}

/// What the previous session marker says.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PreviousExit {
    /// No marker, or one that could not be decoded: nothing to recover.
    Unknown,
    /// The previous process reached its exit hook.
    Clean(SessionMarker),
    /// The previous process did not reach its exit hook.
    Unclean(SessionMarker),
    /// The recorded process is still alive, so that instance owns recovery.
    Running(SessionMarker),
}

/// `session.json` in one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    support_dir: PathBuf,
}

impl SessionFile {
    /// A session marker in `support_dir`.
    #[must_use]
    pub fn new(support_dir: impl Into<PathBuf>) -> Self {
        Self {
            support_dir: support_dir.into(),
        }
    }

    /// `<support>/session.json`.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.support_dir.join(SESSION_FILE)
    }

    /// The stored marker, or `None` when it is absent or undecodable.
    ///
    /// An undecodable marker is treated as absent rather than as an unclean exit: a marker that
    /// cannot be read carries no evidence of what happened, and guessing would show a recovery
    /// sheet for a healthy start.
    #[must_use]
    pub fn read(&self) -> Option<SessionMarker> {
        let bytes = fs::read(self.path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Classifies the marker without modifying it.
    #[must_use]
    pub fn classify(&self, probe: &dyn ProcessProbe) -> PreviousExit {
        let Some(marker) = self.read() else {
            return PreviousExit::Unknown;
        };
        if !marker.clean_exit
            && marker.pid != std::process::id()
            && matches!(probe.inspect(marker.pid), ProcessState::Alive { .. })
        {
            return PreviousExit::Running(marker);
        }
        if marker.clean_exit {
            PreviousExit::Clean(marker)
        } else {
            PreviousExit::Unclean(marker)
        }
    }

    /// Classifies the previous session, then records this one as running.
    ///
    /// A live other instance is left untouched: [`PreviousExit::Running`] is returned without
    /// writing, so two instances can never overwrite each other's marker.
    ///
    /// # Errors
    /// [`RecoveryError::Io`] when the marker cannot be written.
    pub fn start(&self) -> Result<PreviousExit, RecoveryError> {
        self.start_with(&SystemProbe)
    }

    /// [`Self::start`] with an injected process probe, so tests never depend on the real process
    /// table.
    ///
    /// # Errors
    /// [`RecoveryError::Io`] when the marker cannot be written.
    pub fn start_with(&self, probe: &dyn ProcessProbe) -> Result<PreviousExit, RecoveryError> {
        let previous = self.classify(probe);
        if matches!(previous, PreviousExit::Running(_)) {
            return Ok(previous);
        }
        self.write(&SessionMarker::for_current_process())?;
        Ok(previous)
    }

    /// Marks the current process's session as a clean exit.
    ///
    /// Only the marker of this process is rewritten; a marker that belongs to another instance, or
    /// no marker at all, is left alone and `false` is returned.
    ///
    /// # Errors
    /// [`RecoveryError::Io`] when the marker exists but cannot be rewritten.
    pub fn mark_clean(&self) -> Result<bool, RecoveryError> {
        let Some(mut marker) = self.read() else {
            return Ok(false);
        };
        if marker.pid != std::process::id() {
            return Ok(false);
        }
        if marker.clean_exit {
            return Ok(true);
        }
        marker.clean_exit = true;
        self.write(&marker)?;
        Ok(true)
    }

    fn write(&self, marker: &SessionMarker) -> Result<(), RecoveryError> {
        let staging = self.support_dir.join(SESSION_STAGING_FILE);
        let path = self.path();
        let bytes = serde_json::to_vec_pretty(marker).map_err(|error| RecoveryError::Io {
            path: path.clone(),
            message: error.to_string(),
        })?;
        fs::write(&staging, &bytes).map_err(|error| io_error(&staging, error))?;
        fs::rename(&staging, &path).map_err(|error| io_error(&path, error))
    }
}

/// Where recovery looks. Built from [`AppPaths`], never hard-coded by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLocations {
    /// Support directory: `session.json`, and the root of the cache and backup trees.
    pub support_dir: PathBuf,
    /// Operation backup root that receives the current bytes of a restored file.
    pub backup_dir: PathBuf,
    /// Directories scanned for `*.journal`.
    pub journal_dirs: Vec<PathBuf>,
    /// Directories scanned for `.<name>.agentuna-tmp-*` debris.
    pub temp_roots: Vec<PathBuf>,
}

impl RecoveryLocations {
    /// Every location recovery needs, derived from the resolved application paths.
    #[must_use]
    pub fn from_paths(paths: &AppPaths) -> Self {
        let mut stores = vec![
            paths.user_store().to_path_buf(),
            paths.global_store().to_path_buf(),
        ];
        if let Some(project) = paths.project_store() {
            stores.push(project.to_path_buf());
        }
        let mut journal_dirs = vec![paths.cache_dir().join(PENDING_DIR)];
        journal_dirs.extend(
            stores
                .iter()
                .map(|store| store.join(STATE_DIR).join(JOURNALS_DIR)),
        );
        let mut temp_roots = vec![
            paths.support_dir().to_path_buf(),
            paths.cache_dir().to_path_buf(),
            paths.backup_dir().to_path_buf(),
        ];
        temp_roots.extend(stores);
        Self {
            support_dir: paths.support_dir().to_path_buf(),
            backup_dir: paths.backup_dir().to_path_buf(),
            journal_dirs,
            temp_roots,
        }
    }
}

/// The `operations` row of one interrupted operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedOperation {
    /// `operations.id` of the row the journal belongs to.
    pub id: String,
    /// `operations.kind`; `apply` for a journaled multi-file change set.
    pub kind: String,
    /// Destinations the operation intended to write.
    pub paths: Vec<PathBuf>,
    /// When the operation started, as recorded in its journal.
    pub started_at: Option<OffsetDateTime>,
}

/// The `operations` rows recovery writes.
///
/// TF-009 (`at-db`) supplies the production implementation; this trait keeps `at-app` free of a
/// direct SQLite dependency and lets tests record the rows instead.
pub trait RecoveryOperations: Send + Sync {
    /// Marks an operation `interrupted`. Must be idempotent: the same journal is reported on every
    /// start until the user decides.
    ///
    /// # Errors
    /// [`RecoveryError::Operations`] when the row cannot be written.
    fn mark_interrupted(&self, operation: &InterruptedOperation) -> Result<(), RecoveryError>;

    /// Records a `restore` operation for `files` and returns its id.
    ///
    /// # Errors
    /// [`RecoveryError::Operations`] when the row cannot be written.
    fn record_restore(&self, files: &[PathBuf]) -> Result<String, RecoveryError>;

    /// Loads the original bytes for a completed operation from its verified backup.
    ///
    /// # Errors
    /// Returns [`RecoveryError::Operations`] when backup history cannot be queried.
    fn before_bytes(
        &self,
        _journal: &fs_recovery::PendingJournal,
        _index: usize,
        _entry: &fs_recovery::JournalEntry,
    ) -> Result<Option<Vec<u8>>, RecoveryError> {
        Ok(None)
    }

    /// Records the terminal state of startup journal recovery.
    fn finish_journal(
        &self,
        _operation_id: &str,
        _succeeded: bool,
        _manual_review: bool,
    ) -> Result<(), RecoveryError> {
        Ok(())
    }
}

/// Production operations-history adapter for startup recovery.
pub struct DatabaseRecoveryOperations {
    database: Arc<Mutex<Database>>,
    backup_root: Option<PathBuf>,
}

impl DatabaseRecoveryOperations {
    /// Creates the adapter over the app's shared SQLite handle.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self {
            database,
            backup_root: None,
        }
    }
    /// Sets the on-disk backup root used when a crash preceded backup catalogue insertion.
    #[must_use]
    pub fn with_backup_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.backup_root = Some(path.into());
        self
    }
}

impl RecoveryOperations for DatabaseRecoveryOperations {
    fn mark_interrupted(&self, operation: &InterruptedOperation) -> Result<(), RecoveryError> {
        let database = self
            .database
            .lock()
            .map_err(|_| RecoveryError::Operations {
                message: "database lock poisoned".to_owned(),
            })?;
        let kind = operation.kind.parse().ok();
        let started_at = operation
            .started_at
            .map(|value| value.unix_timestamp_nanos() / 1_000_000)
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or(0);
        at_db::ops::record_interrupted(
            database.connection(),
            &operation.id,
            kind,
            Some("Recovered interrupted filesystem operation"),
            started_at,
        )
        .map_err(|error| RecoveryError::Operations {
            message: error.to_string(),
        })
    }

    fn record_restore(&self, _files: &[PathBuf]) -> Result<String, RecoveryError> {
        let database = self
            .database
            .lock()
            .map_err(|_| RecoveryError::Operations {
                message: "database lock poisoned".to_owned(),
            })?;
        at_db::ops::record_completed(
            database.connection(),
            at_db::ops::OperationKind::Restore,
            at_db::ops::OperationActor::Ui,
            Some("Restore interrupted file"),
        )
        .map(|id| id.to_string())
        .map_err(|error| RecoveryError::Operations {
            message: error.to_string(),
        })
    }
    fn before_bytes(
        &self,
        journal: &fs_recovery::PendingJournal,
        _index: usize,
        entry: &fs_recovery::JournalEntry,
    ) -> Result<Option<Vec<u8>>, RecoveryError> {
        let database = self
            .database
            .lock()
            .map_err(|_| RecoveryError::Operations {
                message: "database lock poisoned".to_owned(),
            })?;
        let backups =
            at_db::backups::backups_for_operation(database.connection(), &journal.operation_id)
                .map_err(|error| RecoveryError::Operations {
                    message: error.to_string(),
                })?;
        let entry_path = entry.path.to_string_lossy();
        if let Some(backup) = backups.into_iter().find(|backup| {
            backup.original_path.as_deref() == Some(entry_path.as_ref())
                && backup.content_hash.as_deref() == entry.before_hash.as_deref()
        }) {
            let path =
                backup
                    .backup_path
                    .map(PathBuf::from)
                    .ok_or_else(|| RecoveryError::Operations {
                        message: "backup catalogue row has no path".to_owned(),
                    })?;
            return fs::read(&path)
                .map(Some)
                .map_err(|error| io_error(&path, error));
        }
        drop(database);
        let Some(root) = &self.backup_root else {
            return Ok(None);
        };
        find_backup_bytes(
            root,
            &journal.operation_id,
            &entry.path,
            entry.before_hash.as_deref(),
        )
    }

    fn finish_journal(
        &self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), RecoveryError> {
        let operation_id = operation_id
            .parse::<at_db::ops::OperationId>()
            .map_err(|error| RecoveryError::Operations {
                message: error.to_string(),
            })?;
        let database = self
            .database
            .lock()
            .map_err(|_| RecoveryError::Operations {
                message: "database lock poisoned".to_owned(),
            })?;
        let status = if succeeded {
            at_db::ops::OperationStatus::Succeeded
        } else {
            at_db::ops::OperationStatus::Failed
        };
        at_db::ops::finish(
            database.connection(),
            &operation_id,
            status,
            manual_review.then_some("manual_review_required"),
        )
        .map(|_| ())
        .map_err(|error| RecoveryError::Operations {
            message: error.to_string(),
        })
    }
}

fn find_backup_bytes(
    root: &Path,
    operation_id: &str,
    original_path: &Path,
    expected_hash: Option<&str>,
) -> Result<Option<Vec<u8>>, RecoveryError> {
    let Some(expected_hash) = expected_hash else {
        return Ok(None);
    };
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&directory, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| io_error(&directory, error))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(blob_name) = name.strip_suffix(".json") else {
                continue;
            };
            let sidecar: at_fs::backup::BackupSidecar = match fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                Some(sidecar) => sidecar,
                None => continue,
            };
            if sidecar.operation_id.as_deref() != Some(operation_id)
                || sidecar.original_path != original_path
                || sidecar.content_hash != expected_hash
            {
                continue;
            }
            let Some(parent) = path.parent() else {
                continue;
            };
            let blob = parent.join(blob_name);
            let bytes = fs::read(&blob).map_err(|error| io_error(&blob, error))?;
            if at_fs::sha256_hex(&bytes) == expected_hash {
                return Ok(Some(bytes));
            }
        }
    }
    Ok(None)
}

/// Runs the production database integrity check used after an unclean exit.
pub struct DatabaseIndexIntegrity {
    database: Arc<Mutex<Database>>,
}

impl DatabaseIndexIntegrity {
    /// Creates the check over the app's shared SQLite handle.
    #[must_use]
    pub fn new(database: Arc<Mutex<Database>>) -> Self {
        Self { database }
    }
}

impl IndexIntegrity for DatabaseIndexIntegrity {
    fn check(&self) -> Result<IntegrityReport, RecoveryError> {
        let database = self.database.lock().map_err(|_| RecoveryError::Integrity {
            detail: "database lock poisoned".to_owned(),
        })?;
        let detail = database
            .connection()
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .map_err(|error| RecoveryError::Integrity {
                detail: error.to_string(),
            })?;
        Ok(IntegrityReport {
            ok: detail.eq_ignore_ascii_case("ok"),
            detail,
        })
    }
}

/// What an index integrity check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// Whether the index passed.
    pub ok: bool,
    /// Redacted explanation, shown in the log and on the Recovery sheet.
    pub detail: String,
}

impl IntegrityReport {
    /// A passing report.
    #[must_use]
    pub fn healthy() -> Self {
        Self {
            ok: true,
            detail: "ok".to_owned(),
        }
    }
}

/// Runs the index integrity check that follows an unclean exit (TF-020).
///
/// A closure is the whole trait, so the shell can pass `|| database.integrity_check()`.
pub trait IndexIntegrity: Send + Sync {
    /// Runs the check.
    ///
    /// # Errors
    /// [`RecoveryError::Integrity`] when the check itself cannot run.
    fn check(&self) -> Result<IntegrityReport, RecoveryError>;
}

impl<F> IndexIntegrity for F
where
    F: Fn() -> Result<IntegrityReport, RecoveryError> + Send + Sync,
{
    fn check(&self) -> Result<IntegrityReport, RecoveryError> {
        self()
    }
}

/// One file an interrupted operation left behind, ready for the Recovery sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedFile {
    /// The operation that was writing it.
    pub operation_id: String,
    /// Destination path.
    pub path: PathBuf,
    /// What the destination holds now: [`EntryState::After`] can be restored.
    pub state: EntryState,
    /// SHA-256 of the bytes before the write; `None` when the file did not exist yet.
    pub before_hash: Option<String>,
    /// SHA-256 of the bytes the write was installing.
    pub after_hash: String,
    /// Journal that recorded the write.
    pub journal_path: PathBuf,
}

/// A file recovery refuses to touch on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReview {
    /// The operation that was writing it.
    pub operation_id: String,
    /// Destination, or `None` when the journal itself could not be decoded.
    pub path: Option<PathBuf>,
    /// Why a human has to decide.
    pub reason: String,
    /// SHA-256 of the bytes before the write, when the journal recorded one.
    pub before_hash: Option<String>,
    /// SHA-256 of the bytes the write was installing, when known.
    pub after_hash: Option<String>,
    /// SHA-256 of what is there now, when it could be hashed.
    pub observed: Option<String>,
}

/// Everything the Recovery sheet renders, plus how long recovery took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySheet {
    /// Operations that were mid-flight, one per journal.
    pub interrupted: Vec<InterruptedOperation>,
    /// Files whose destination still holds the after bytes and can be restored.
    pub files: Vec<InterruptedFile>,
    /// Temp files that survived the scan and may be discarded.
    pub temp_files: Vec<TempFile>,
    /// Files recovery will not write: a human decides.
    pub manual_review: Vec<ManualReview>,
    /// Debris removed automatically because it was older than [`at_fs::recovery::RETENTION`].
    pub swept: Vec<PathBuf>,
    /// Time spent detecting and classifying, reported in the log.
    pub duration: Duration,
}

/// What a recovery run decided.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryOutcome {
    /// The previous exit was clean: nothing to review, only expired debris was removed.
    Clean {
        /// Debris removed because it was older than [`at_fs::recovery::RETENTION`].
        swept: Vec<PathBuf>,
        /// Time the run took.
        duration: Duration,
    },
    /// Unclean exit: the shell shows the Recovery sheet before the window is interactive.
    Review(Box<RecoverySheet>),
    /// Another live instance owns the session marker; this process did not run recovery.
    Skipped {
        /// Process id that held the marker.
        pid: u32,
    },
}

impl RecoveryOutcome {
    /// The sheet, when the run produced one.
    #[must_use]
    pub fn sheet(&self) -> Option<&RecoverySheet> {
        match self {
            Self::Review(sheet) => Some(sheet.as_ref()),
            Self::Clean { .. } | Self::Skipped { .. } => None,
        }
    }
}

/// A completed restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// `operations.id` of the restore row.
    pub operation_id: String,
    /// Destination that was restored.
    pub path: PathBuf,
    /// The guarded write that put the bytes back.
    pub outcome: WriteOutcome,
}

/// Why recovery could not run or repair a file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// Filesystem recovery failed.
    #[error(transparent)]
    Fs(#[from] fs_recovery::RecoveryError),
    /// A session marker or backup could not be read or written.
    #[error("{}: {message}", path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Operating-system error text.
        message: String,
    },
    /// The index did not pass its integrity check.
    #[error("index integrity check failed: {detail}")]
    Integrity {
        /// Redacted explanation.
        detail: String,
    },
    /// An `operations` row could not be written.
    #[error("operations log write failed: {message}")]
    Operations {
        /// Redacted explanation.
        message: String,
    },
    /// No journal exists for the requested operation.
    #[error("no pending journal for operation {operation_id}")]
    UnknownOperation {
        /// Operation that was asked for.
        operation_id: String,
    },
    /// The file cannot be restored automatically.
    #[error("{}: cannot restore automatically: {reason}", path.display())]
    NotRestorable {
        /// Destination that was asked for.
        path: PathBuf,
        /// Why not.
        reason: String,
    },
}

fn io_error(path: &Path, error: io::Error) -> RecoveryError {
    RecoveryError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

struct AppJournalCallbacks<'a>(&'a dyn RecoveryOperations);

impl fs_recovery::RecoveryCallbacks for AppJournalCallbacks<'_> {
    fn before_bytes(
        &mut self,
        journal: &fs_recovery::PendingJournal,
        index: usize,
        entry: &fs_recovery::JournalEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        self.0
            .before_bytes(journal, index, entry)
            .map_err(|error| error.to_string())
    }

    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String> {
        self.0
            .finish_journal(operation_id, succeeded, manual_review)
            .map_err(|error| error.to_string())
    }
}

/// Detects and repairs what an unclean shutdown left behind.
pub struct RecoveryService {
    locations: RecoveryLocations,
    operations: Arc<dyn RecoveryOperations>,
    integrity: Arc<dyn IndexIntegrity>,
    probe: Arc<dyn ProcessProbe>,
}

impl RecoveryService {
    /// Builds the service from resolved locations and the two injected database seams.
    #[must_use]
    pub fn new(
        locations: RecoveryLocations,
        operations: Arc<dyn RecoveryOperations>,
        integrity: Arc<dyn IndexIntegrity>,
    ) -> Self {
        Self {
            locations,
            operations,
            integrity,
            probe: Arc::new(SystemProbe),
        }
    }

    /// Replaces the process probe used to decide whether the previous session is still running.
    #[must_use]
    pub fn with_probe(mut self, probe: Arc<dyn ProcessProbe>) -> Self {
        self.probe = probe;
        self
    }

    /// The locations this service scans.
    #[must_use]
    pub fn locations(&self) -> &RecoveryLocations {
        &self.locations
    }

    /// Runs startup recovery once.
    ///
    /// Must be called after the single-instance gate accepted this process and before the main
    /// window is shown, so the Recovery sheet is the first thing the user sees.
    ///
    /// # Errors
    /// [`RecoveryError::Io`] when the session marker cannot be written,
    /// [`RecoveryError::Integrity`] when the index fails its check, any journal read or
    /// `operations` row failure, or an [`at_fs::FsError`] from the temp sweep.
    pub fn run(&self) -> Result<RecoveryOutcome, RecoveryError> {
        let started = Instant::now();
        let session = SessionFile::new(&self.locations.support_dir);
        let previous = session.start_with(self.probe.as_ref())?;
        if let PreviousExit::Running(marker) = previous {
            // Another live instance owns recovery; touching anything here would race it.
            return Ok(RecoveryOutcome::Skipped { pid: marker.pid });
        }

        let now = OffsetDateTime::now_utc();
        let swept = self.sweep(now)?;
        let outcome = match previous {
            PreviousExit::Unclean(_) => {
                let mut sheet = self.review(now, swept)?;
                sheet.duration = started.elapsed();
                RecoveryOutcome::Review(Box::new(sheet))
            }
            PreviousExit::Clean(_) | PreviousExit::Unknown | PreviousExit::Running(_) => {
                RecoveryOutcome::Clean {
                    swept,
                    duration: started.elapsed(),
                }
            }
        };
        tracing::info!(
            duration_ms = started.elapsed().as_millis(),
            unclean = outcome.sheet().is_some(),
            clean = matches!(outcome, RecoveryOutcome::Clean { .. }),
            "startup recovery finished"
        );
        Ok(outcome)
    }

    /// Removes debris older than [`at_fs::recovery::RETENTION`], in both the temp trees and the
    /// journal directories.
    fn sweep(&self, now: OffsetDateTime) -> Result<Vec<PathBuf>, RecoveryError> {
        let mut removed =
            fs_recovery::sweep_stale_temp_files(&self.locations.temp_roots, now, RETENTION).removed;
        for dir in &self.locations.journal_dirs {
            removed.extend(fs_recovery::purge_expired_journals(dir, now, RETENTION)?);
        }
        removed.sort();
        Ok(removed)
    }

    /// Integrity check, then one interrupted operation per readable journal.
    fn review(
        &self,
        now: OffsetDateTime,
        swept: Vec<PathBuf>,
    ) -> Result<RecoverySheet, RecoveryError> {
        let integrity = self.integrity.check()?;
        if !integrity.ok {
            return Err(RecoveryError::Integrity {
                detail: integrity.detail,
            });
        }
        for journal_dir in &self.locations.journal_dirs {
            if journal_dir
                .file_name()
                .is_none_or(|name| name != JOURNALS_DIR)
                || journal_dir
                    .parent()
                    .and_then(Path::file_name)
                    .is_none_or(|name| name != STATE_DIR)
            {
                continue;
            }
            let Some(store_root) = journal_dir.parent().and_then(Path::parent) else {
                continue;
            };
            let mut callbacks = AppJournalCallbacks(self.operations.as_ref());
            fs_recovery::recover_journals(journal_dir, store_root, &mut callbacks)?;
        }
        let mut sheet = RecoverySheet {
            interrupted: Vec::new(),
            files: Vec::new(),
            temp_files: Vec::new(),
            manual_review: Vec::new(),
            swept,
            duration: Duration::ZERO,
        };
        for dir in &self.locations.journal_dirs {
            for journal_path in fs_recovery::scan_journals(dir)? {
                self.absorb_journal(&journal_path, &mut sheet)?;
            }
        }
        let max_age = RETENTION;
        sheet.temp_files = fs_recovery::scan_temp_files(&self.locations.temp_roots, now, max_age)
            .into_iter()
            .filter(|file| !file.stale)
            .collect();
        Ok(sheet)
    }

    /// Adds one journal to the sheet and marks its operation interrupted.
    fn absorb_journal(
        &self,
        journal_path: &Path,
        sheet: &mut RecoverySheet,
    ) -> Result<(), RecoveryError> {
        let fallback_id = fs_recovery::journal_operation_id(journal_path).unwrap_or_else(|| {
            journal_path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        });
        let assessment = match fs_recovery::assess_journal(journal_path) {
            Ok(assessment) => assessment,
            Err(fs_recovery::RecoveryError::MalformedJournal { reason, .. }) => {
                let operation = InterruptedOperation {
                    id: fallback_id,
                    kind: "apply".to_owned(),
                    paths: Vec::new(),
                    started_at: None,
                };
                self.operations.mark_interrupted(&operation)?;
                sheet.manual_review.push(ManualReview {
                    operation_id: operation.id.clone(),
                    path: None,
                    reason: format!("journal could not be read: {reason}"),
                    before_hash: None,
                    after_hash: None,
                    observed: None,
                });
                sheet.interrupted.push(operation);
                return Ok(());
            }
            Err(other) => return Err(other.into()),
        };
        let operation = InterruptedOperation {
            id: assessment.journal.operation_id.clone(),
            kind: "apply".to_owned(),
            paths: assessment
                .journal
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
            started_at: Some(assessment.journal.started_at),
        };
        self.operations.mark_interrupted(&operation)?;
        sheet.interrupted.push(operation);
        self.absorb_entries(&assessment, sheet);
        Ok(())
    }

    fn absorb_entries(&self, assessment: &JournalAssessment, sheet: &mut RecoverySheet) {
        for assessed in &assessment.entries {
            let entry = &assessed.entry;
            match &assessed.state {
                EntryState::Diverged { observed } => sheet.manual_review.push(ManualReview {
                    operation_id: assessment.journal.operation_id.clone(),
                    path: Some(entry.path.clone()),
                    reason: "the file matches neither the before nor the after hash".to_owned(),
                    before_hash: entry.before_hash.clone(),
                    after_hash: Some(entry.after_hash.clone()),
                    observed: observed.clone(),
                }),
                EntryState::Before | EntryState::After => {
                    sheet.files.push(InterruptedFile {
                        operation_id: assessment.journal.operation_id.clone(),
                        path: entry.path.clone(),
                        state: assessed.state.clone(),
                        before_hash: entry.before_hash.clone(),
                        after_hash: entry.after_hash.clone(),
                        journal_path: assessment.path.clone(),
                    });
                }
                other => sheet.manual_review.push(ManualReview {
                    operation_id: assessment.journal.operation_id.clone(),
                    path: Some(entry.path.clone()),
                    reason: format!(
                        "recovery does not understand the recorded state ({})",
                        other.label()
                    ),
                    before_hash: entry.before_hash.clone(),
                    after_hash: Some(entry.after_hash.clone()),
                    observed: None,
                }),
            }
        }
    }

    /// Restores one file from a verified backup blob.
    ///
    /// Records the `restore` operation first, then writes through [`at_fs::apply`] with the
    /// operation folder as backup destination, so the bytes that are being replaced are kept.
    ///
    /// # Errors
    /// [`RecoveryError::NotRestorable`] when the destination still holds the before bytes, did not
    /// exist before the write, or diverged; [`fs_recovery::RecoveryError::BackupMismatch`] when
    /// the blob is not the recorded version; [`RecoveryError::Operations`] when the row cannot be
    /// written; [`RecoveryError::Io`] when the blob cannot be read; or any [`at_fs::FsError`].
    pub fn restore(
        &self,
        file: &InterruptedFile,
        backup_blob: &Path,
    ) -> Result<RestoreReport, RecoveryError> {
        match &file.state {
            EntryState::After => {}
            EntryState::Before => {
                return Err(RecoveryError::NotRestorable {
                    path: file.path.clone(),
                    reason: "the destination already holds the before bytes".to_owned(),
                });
            }
            EntryState::Diverged { observed } => {
                return Err(RecoveryError::NotRestorable {
                    path: file.path.clone(),
                    reason: format!(
                        "the destination was changed outside AgenTuna (now {})",
                        observed.as_deref().unwrap_or("absent")
                    ),
                });
            }
            other => {
                return Err(RecoveryError::NotRestorable {
                    path: file.path.clone(),
                    reason: format!("unknown recorded state ({})", other.label()),
                });
            }
        }
        let Some(before_hash) = file.before_hash.clone() else {
            return Err(RecoveryError::NotRestorable {
                path: file.path.clone(),
                reason: "the file did not exist before the interrupted operation".to_owned(),
            });
        };
        let before_bytes = fs::read(backup_blob).map_err(|error| io_error(backup_blob, error))?;
        let operation_id = self
            .operations
            .record_restore(std::slice::from_ref(&file.path))?;
        let operation_folder = self.locations.backup_dir.join(&operation_id);
        let request = fs_recovery::RestoreRequest {
            path: file.path.clone(),
            before_bytes,
            before_hash,
            expected_current_hash: file.after_hash.clone(),
            operation_folder,
            backup_meta: BackupMeta::default(),
        };
        let outcome = fs_recovery::restore_file(&request)?;
        tracing::info!(
            operation_id,
            path = %file.path.display(),
            "restored a file after an interrupted operation"
        );
        Ok(RestoreReport {
            operation_id,
            path: file.path.clone(),
            outcome,
        })
    }

    /// Discards one pending operation: its journal and the temp files it names.
    ///
    /// The destination is never touched, and the `interrupted` operations row stays for the audit
    /// trail.
    ///
    /// # Errors
    /// [`RecoveryError::UnknownOperation`] when no journal holds `operation_id`, or any
    /// [`RecoveryError::Io`] from deleting a path.
    pub fn discard(&self, operation_id: &str) -> Result<DiscardOutcome, RecoveryError> {
        let name = fs_recovery::journal_file_name(operation_id);
        for dir in &self.locations.journal_dirs {
            let candidate = dir.join(&name);
            if !candidate.is_file() {
                continue;
            }
            let journal = fs_recovery::read_journal(&candidate)?;
            let outcome = fs_recovery::discard_pending(&candidate, &journal)?;
            tracing::info!(
                operation_id,
                removed = outcome.removed.len(),
                "discarded a pending operation"
            );
            return Ok(outcome);
        }
        Err(RecoveryError::UnknownOperation {
            operation_id: operation_id.to_owned(),
        })
    }
}

#[cfg(test)]
mod database_adapter_tests {
    use std::sync::{Arc, Mutex};

    use time::OffsetDateTime;

    use super::{
        DatabaseIndexIntegrity, DatabaseRecoveryOperations, IndexIntegrity, IntegrityReport,
        InterruptedOperation, RecoveryOperations,
    };

    #[test]
    fn database_adapters_record_interruption_restore_and_check_integrity() {
        let database = Arc::new(Mutex::new(
            at_db::Database::open_in_memory().expect("database"),
        ));
        let operations = DatabaseRecoveryOperations::new(Arc::clone(&database));
        let interrupted = InterruptedOperation {
            id: "job-recovery-test".to_owned(),
            kind: "deploy".to_owned(),
            paths: vec![std::path::PathBuf::from("sandbox/skill")],
            started_at: Some(OffsetDateTime::now_utc()),
        };

        operations
            .mark_interrupted(&interrupted)
            .expect("record interrupted operation");
        let restore_id = operations
            .record_restore(&interrupted.paths)
            .expect("record restore operation");
        let integrity = DatabaseIndexIntegrity::new(Arc::clone(&database))
            .check()
            .expect("integrity check");

        assert_eq!(restore_id.len(), 26);
        assert_eq!(
            integrity,
            IntegrityReport {
                ok: true,
                detail: "ok".to_owned(),
            }
        );
        let database = database.lock().expect("database lock");
        let status = database
            .connection()
            .query_row(
                "SELECT status FROM operations WHERE id = ?1",
                ["job-recovery-test"],
                |row| row.get::<_, String>(0),
            )
            .expect("interrupted status");
        assert_eq!(status, "interrupted");
    }
}
