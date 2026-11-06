//! Retention and pruning of durable history (TF-050).
//!
//! AgenTuna keeps history for as long as it is useful and no longer. This
//! module is the only code that removes it, and it removes it under one policy:
//!
//! | Data | Kept |
//! |---|---|
//! | resolved drift events | [`DRIFT_RETENTION_DAYS`] (90) |
//! | backups | older than `max(30, backup_retention_days)` **and** outside the newest [`KEEP_NEWEST_BACKUPS`] (3) for the same original path **and** not pinned |
//! | operations and their file rows | [`OPERATION_RETENTION_DAYS`] (365), except rows referenced by a pinned backup or still inside the [`UNDO_WINDOW_DAYS`] (30) undo window |
//! | native items not seen | marked `missing` after [`NATIVE_MISSING_DAYS`] (90) |
//! | log files | [`LOG_RETENTION_DAYS`] (30) |
//!
//! Health-check retention is **not** here: TM-009 owns it (the last 200 rows per
//! server, pruned hourly). A pruning run never touches `health_checks`.
//!
//! Rows are deleted in batches of [`PRUNE_BATCH_SIZE`] (500) inside their own
//! transactions, so a long prune never holds a write lock for long and a
//! failure part-way leaves a consistent database. The report carries the count
//! per table, the batch count and the longest batch so a caller can log how much
//! work pruning did. `VACUUM` runs only after pruning, and only when the free
//! pages exceed [`VACUUM_FREE_PAGE_RATIO`] (20%) of the database.
//!
//! Pruning is deferred while an operation is active: a row still `running` and
//! started within [`ACTIVE_OPERATION_GRACE`] blocks the whole run and is
//! reported as such. A `running` row older than that grace period is a process
//! that died (`record_interrupted` may not have run) and does not block.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, ToSql};

use crate::DbError;
use crate::ops::OperationId;

/// Days an operation stays undoable (CONTRACT decision 7).
pub const UNDO_WINDOW_DAYS: u32 = 30;

/// Days before a resolved drift event is pruned.
pub const DRIFT_RETENTION_DAYS: u32 = 90;

/// Days before an operation and its file rows are pruned.
pub const OPERATION_RETENTION_DAYS: u32 = 365;

/// Days without a sighting before a native item is marked `missing`.
pub const NATIVE_MISSING_DAYS: u32 = 90;

/// Days before a log file is deleted.
pub const LOG_RETENTION_DAYS: u32 = 30;

/// Minimum backup age, whatever `backup_retention_days` says (settings clamp to
/// the same floor, so both agree).
pub const MIN_BACKUP_RETENTION_DAYS: u32 = 30;

/// How many newest backups of one original path are never pruned.
pub const KEEP_NEWEST_BACKUPS: i64 = 3;

/// Rows deleted (or marked) per transaction.
pub const PRUNE_BATCH_SIZE: usize = 500;

/// Free-page share above which pruning ends with `VACUUM`.
pub const VACUUM_FREE_PAGE_RATIO: f64 = 0.2;

/// How long a `running` operation is considered active.
pub const ACTIVE_OPERATION_GRACE: Duration = Duration::from_secs(60 * 60);

/// Seconds in one day, for the log-file cutoff.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// What one pruning run is allowed to do.
#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    /// Days a backup is kept before it becomes prunable; clamped to
    /// [`MIN_BACKUP_RETENTION_DAYS`].
    pub backup_retention_days: u32,
    /// The instant the run happens, injected so tests are deterministic.
    pub now: SystemTime,
    /// A `running` operation younger than this defers the whole run.
    pub active_grace: Duration,
}

impl RetentionPolicy {
    /// A policy for `backup_retention_days` evaluated at `now`.
    #[must_use]
    pub fn new(backup_retention_days: u32, now: SystemTime) -> Self {
        Self {
            backup_retention_days: backup_retention_days.max(MIN_BACKUP_RETENTION_DAYS),
            now,
            active_grace: ACTIVE_OPERATION_GRACE,
        }
    }
}

/// What a run did to one table.
#[derive(Clone, Debug)]
pub struct TablePrune {
    /// Table name, or `log_files` for the filesystem pass.
    pub table: &'static str,
    /// Rows deleted.
    pub deleted: u64,
    /// Rows marked instead of deleted (native items).
    pub marked: u64,
    /// Transactions the table needed.
    pub batches: u32,
    /// Longest single transaction for this table.
    pub longest_batch: Duration,
}

impl TablePrune {
    /// An empty entry for `table`.
    const fn new(table: &'static str) -> Self {
        Self {
            table,
            deleted: 0,
            marked: 0,
            batches: 0,
            longest_batch: Duration::ZERO,
        }
    }
}

/// Everything one pruning run did.
#[derive(Clone, Debug, Default)]
pub struct PruneReport {
    /// One entry per table, in the order the run pruned them.
    pub tables: Vec<TablePrune>,
    /// Whether the run ended with `VACUUM`.
    pub vacuumed: bool,
    /// Whether the run deferred because an operation is active.
    pub deferred: bool,
    /// Active operations that deferred the run.
    pub active_operations: u64,
    /// Longest transaction across every table.
    pub longest_batch: Duration,
}

impl PruneReport {
    /// Total rows deleted across every table.
    #[must_use]
    pub fn total_deleted(&self) -> u64 {
        self.tables.iter().map(|table| table.deleted).sum()
    }

    /// Total rows marked across every table.
    #[must_use]
    pub fn total_marked(&self) -> u64 {
        self.tables.iter().map(|table| table.marked).sum()
    }

    /// The entry for `table`, if the run visited it.
    #[must_use]
    pub fn table(&self, table: &str) -> Option<&TablePrune> {
        self.tables.iter().find(|entry| entry.table == table)
    }
}

/// Why a pruning run failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RetentionError {
    /// The database rejected a statement.
    #[error("database: {0}")]
    Db(#[from] DbError),
    /// The file system rejected a log-file operation.
    #[error("{path}: {source}", path = .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// The operation is older than the undo window.
    #[error("operation {operation} is outside the {days}-day undo window")]
    UndoExpired {
        /// Operation asked about.
        operation: String,
        /// The window that refused it.
        days: u32,
    },
    /// The operation does not exist.
    #[error("operation {operation} does not exist")]
    UnknownOperation {
        /// Operation asked about.
        operation: String,
    },
}

impl From<rusqlite::Error> for RetentionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// Prunes history according to `policy`.
///
/// `log_dir` is the directory of rolled log files to prune; pass `None` when the
/// caller does not own a log directory. Pruning is deferred (and reported) while
/// an operation is active.
///
/// # Errors
///
/// [`RetentionError`] when a statement, the log-file pass or `VACUUM` fails.
pub fn run(
    connection: &Connection,
    policy: &RetentionPolicy,
    log_dir: Option<&Path>,
) -> Result<PruneReport, RetentionError> {
    let now_seconds = unix_seconds(policy.now);
    let grace_seconds = i64::try_from(policy.active_grace.as_secs()).unwrap_or(i64::MAX);
    let active = active_operations(connection, now_seconds.saturating_sub(grace_seconds))?;
    if active > 0 {
        return Ok(PruneReport {
            deferred: true,
            active_operations: active,
            ..PruneReport::default()
        });
    }

    let mut report = PruneReport::default();

    let drift_cutoff = format!("-{DRIFT_RETENTION_DAYS} days");
    report.tables.push(table_prune(
        "drift_events",
        execute_batches(connection, DRIFT_EVENTS_SQL, &[&now_seconds, &drift_cutoff])?,
    ));

    let backup_cutoff = format!("-{} days", policy.backup_retention_days);
    report.tables.push(table_prune(
        "backups",
        execute_batches(connection, BACKUPS_SQL, &[&now_seconds, &backup_cutoff])?,
    ));

    let operation_cutoff = format!("-{OPERATION_RETENTION_DAYS} days");
    let undo_cutoff = format!("-{UNDO_WINDOW_DAYS} days");
    report.tables.push(table_prune(
        "operations",
        execute_batches(
            connection,
            OPERATIONS_SQL,
            &[&now_seconds, &operation_cutoff, &undo_cutoff],
        )?,
    ));

    report.tables.push(table_prune(
        "operation_files",
        execute_batches(connection, ORPHAN_FILES_SQL, &[])?,
    ));

    let native_cutoff = format!("-{NATIVE_MISSING_DAYS} days");
    let mut native = TablePrune::new("native_items");
    let batch = execute_batches(
        connection,
        NATIVE_MISSING_SQL,
        &[&now_seconds, &native_cutoff],
    )?;
    native.marked = batch.total;
    native.batches = batch.batches;
    native.longest_batch = batch.longest;
    report.tables.push(native);

    if let Some(dir) = log_dir {
        let started = Instant::now();
        let mut entry = TablePrune::new("log_files");
        entry.deleted = prune_log_files(dir, policy.now, LOG_RETENTION_DAYS)?;
        entry.longest_batch = started.elapsed();
        entry.batches = u32::from(entry.deleted > 0);
        report.tables.push(entry);
    }

    report.longest_batch = report
        .tables
        .iter()
        .map(|table| table.longest_batch)
        .max()
        .unwrap_or(Duration::ZERO);

    if connection.is_autocommit() && should_vacuum(connection)? {
        connection.execute_batch("VACUUM")?;
        report.vacuumed = true;
    }
    Ok(report)
}

/// Whether undo of `operation` is still inside [`UNDO_WINDOW_DAYS`].
///
/// # Errors
///
/// [`RetentionError::UnknownOperation`] when the row does not exist,
/// [`RetentionError::UndoExpired`] when it started before the window, and
/// [`RetentionError`] when the query fails.
pub fn ensure_undoable(
    connection: &Connection,
    operation: &OperationId,
    now: SystemTime,
) -> Result<(), RetentionError> {
    let now_seconds = unix_seconds(now);
    let modifier = format!("-{UNDO_WINDOW_DAYS} days");
    let expired: Option<i64> = connection
        .query_row(
            "SELECT started_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?2, 'unixepoch', ?3) \
               FROM operations WHERE id = ?1",
            rusqlite::params![operation.as_str(), now_seconds, modifier],
            |row| row.get(0),
        )
        .optional()?;
    match expired {
        None => Err(RetentionError::UnknownOperation {
            operation: operation.as_str().to_owned(),
        }),
        Some(0) => Ok(()),
        Some(_) => Err(RetentionError::UndoExpired {
            operation: operation.as_str().to_owned(),
            days: UNDO_WINDOW_DAYS,
        }),
    }
}

/// Deletes regular files in `dir` whose modification time is older than
/// `retention_days`.
///
/// A missing directory is not an error: logging may be disabled. Symlinks and
/// subdirectories are left alone. Returns how many files were deleted.
///
/// # Errors
///
/// [`RetentionError::Io`] when the directory cannot be read or a file cannot be
/// removed.
pub fn prune_log_files(
    dir: &Path,
    now: SystemTime,
    retention_days: u32,
) -> Result<u64, RetentionError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(RetentionError::Io {
                path: dir.to_path_buf(),
                source: error,
            });
        }
    };
    let cutoff = now
        .checked_sub(Duration::from_secs(
            u64::from(retention_days) * SECONDS_PER_DAY,
        ))
        .unwrap_or(UNIX_EPOCH);
    let mut deleted = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|error| RetentionError::Io {
            path: dir.to_path_buf(),
            source: error,
        })?;
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified >= cutoff {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RetentionError::Io {
                    path,
                    source: error,
                });
            }
        }
    }
    Ok(deleted)
}

/// Rows removed by one table's batch loop.
struct Batch {
    total: u64,
    batches: u32,
    longest: Duration,
}

/// Run `sql` in [`PRUNE_BATCH_SIZE`]-row transactions until it stops matching.
///
/// Every iteration is its own transaction, so a prune of a large table never
/// holds the write lock for long and a failure leaves completed batches applied.
fn execute_batches(
    connection: &Connection,
    sql: &str,
    parameters: &[&dyn ToSql],
) -> Result<Batch, RetentionError> {
    let mut total = 0_u64;
    let mut batches = 0_u32;
    let mut longest = Duration::ZERO;
    loop {
        let started = Instant::now();
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(sql, parameters)?;
        transaction.commit()?;
        let elapsed = started.elapsed();
        if changed == 0 {
            break;
        }
        total += u64::try_from(changed).unwrap_or(u64::MAX);
        batches += 1;
        longest = longest.max(elapsed);
        if changed < PRUNE_BATCH_SIZE {
            break;
        }
    }
    Ok(Batch {
        total,
        batches,
        longest,
    })
}

/// Wrap a [`Batch`] as a report entry.
fn table_prune(table: &'static str, batch: Batch) -> TablePrune {
    TablePrune {
        table,
        deleted: batch.total,
        marked: 0,
        batches: batch.batches,
        longest_batch: batch.longest,
    }
}

/// `running` operations started at or after `cutoff_seconds`.
fn active_operations(connection: &Connection, cutoff_seconds: i64) -> Result<u64, RetentionError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM operations \
          WHERE status = 'running' \
            AND started_at >= strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')",
        [cutoff_seconds],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Whether the free pages exceed [`VACUUM_FREE_PAGE_RATIO`].
fn should_vacuum(connection: &Connection) -> Result<bool, RetentionError> {
    let free: i64 = connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let total: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    if total <= 0 {
        return Ok(false);
    }
    Ok(free as f64 / total as f64 > VACUUM_FREE_PAGE_RATIO)
}

/// Unix seconds of `at`, saturating at the representable range.
fn unix_seconds(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// Delete resolved drift events older than the drift window.
const DRIFT_EVENTS_SQL: &str = "\
DELETE FROM drift_events WHERE id IN ( \
    SELECT id FROM drift_events \
     WHERE resolved_at IS NOT NULL \
       AND resolved_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?2) \
     LIMIT 500 \
)";

/// Delete old backups that are unpinned and outside the newest three of their
/// original path.
const BACKUPS_SQL: &str = "\
DELETE FROM backups WHERE id IN ( \
    SELECT id FROM ( \
        SELECT id, created_at, pinned, \
               ROW_NUMBER() OVER ( \
                   PARTITION BY original_path ORDER BY created_at DESC, id DESC \
               ) AS newest_rank \
          FROM backups \
    ) \
     WHERE newest_rank > 3 \
       AND pinned = 0 \
       AND created_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?2) \
     LIMIT 500 \
)";

/// Delete old operations unless a pinned backup references them or they are
/// still inside the undo window.
const OPERATIONS_SQL: &str = "\
DELETE FROM operations WHERE id IN ( \
    SELECT o.id FROM operations o \
     WHERE o.started_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?2) \
       AND o.started_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?3) \
       AND NOT EXISTS ( \
             SELECT 1 FROM backups b \
              WHERE b.operation_id = o.id AND b.pinned = 1 \
           ) \
       AND NOT EXISTS ( \
             SELECT 1 FROM operation_files f \
               JOIN backups b ON b.id = f.backup_id \
              WHERE f.operation_id = o.id AND b.pinned = 1 \
           ) \
     LIMIT 500 \
)";

/// Delete file rows whose operation row is gone (foreign keys are on, so this
/// only matters after an import or a repair).
const ORPHAN_FILES_SQL: &str = "\
DELETE FROM operation_files WHERE id IN ( \
    SELECT f.id FROM operation_files f \
     WHERE NOT EXISTS (SELECT 1 FROM operations o WHERE o.id = f.operation_id) \
     LIMIT 500 \
)";

/// Mark native items that have not been seen in the native window.
const NATIVE_MISSING_SQL: &str = "\
UPDATE native_items SET status = 'missing' WHERE id IN ( \
    SELECT id FROM native_items \
     WHERE status != 'missing' \
       AND last_seen_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?2) \
     LIMIT 500 \
)";
