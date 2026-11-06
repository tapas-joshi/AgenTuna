//! Backups catalogue repository over `backups` (migration v7, owned by TF-016).
//!
//! Every pre-write copy an operation takes is catalogued here: the file
//! itself lives under the operation backup folder (layout owned by TC-014),
//! and this table is the index the restore flow and the drift screen query —
//! by original path newest-first, by creation date range, and by the
//! operation that took the backup.
//!
//! The catalogue is durable history: it is never rebuilt from files, which is
//! why the database file itself is backed up on a schedule. [`durable_history`]
//! reports how much of that non-rebuildable state a fresh database still has,
//! so a recovery flow can tell the user what a deleted index cost.
//!
//! This module issues no DDL: every statement is a query or row change against
//! the schema TF-016 created.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Row, params};
use ulid::Ulid;

use crate::DbError;

/// One `backups` row as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    /// Catalogue id (`bkp_<ulid>` for rows this repository created).
    pub id: String,
    /// Operation that took the backup, when recorded.
    pub operation_id: Option<String>,
    /// Path the backup restores to.
    pub original_path: Option<String>,
    /// Where the backup bytes live.
    pub backup_path: Option<String>,
    /// SHA-256 of the backed-up content.
    pub content_hash: Option<String>,
    /// Size in bytes.
    pub size: Option<i64>,
    /// When the backup was taken (RFC 3339 text).
    pub created_at: String,
    /// Whether retention must never prune it.
    pub pinned: bool,
    /// When the backup becomes eligible for pruning (RFC 3339 text).
    pub expires_at: Option<String>,
    /// How many times it has been restored.
    pub restore_count: i64,
}

/// One backup to catalogue, as [`record_backup`] consumes it.
#[derive(Clone, Debug)]
pub struct NewBackup<'a> {
    /// Catalogue id; `None` generates `bkp_<ulid>`.
    pub id: Option<&'a str>,
    /// Operation that took the backup.
    pub operation_id: Option<&'a str>,
    /// Path the backup restores to.
    pub original_path: &'a str,
    /// Where the backup bytes live; unique across the catalogue.
    pub backup_path: &'a str,
    /// SHA-256 of the backed-up content.
    pub content_hash: Option<&'a str>,
    /// Size in bytes.
    pub size: Option<i64>,
    /// Pinned rows are never pruned.
    pub pinned: bool,
    /// RFC 3339 time after which retention may prune the row.
    pub expires_at: Option<&'a str>,
}

impl<'a> NewBackup<'a> {
    /// A catalogue entry for `original_path` stored at `backup_path`.
    #[must_use]
    pub const fn new(original_path: &'a str, backup_path: &'a str) -> Self {
        Self {
            id: None,
            operation_id: None,
            original_path,
            backup_path,
            content_hash: None,
            size: None,
            pinned: false,
            expires_at: None,
        }
    }

    /// Sets the owning operation.
    #[must_use]
    pub fn with_operation(mut self, operation_id: &'a str) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    /// Sets the content hash and size.
    #[must_use]
    pub const fn with_content(mut self, content_hash: Option<&'a str>, size: Option<i64>) -> Self {
        self.content_hash = content_hash;
        self.size = size;
        self
    }

    /// Pins the backup against pruning and sets its expiry.
    #[must_use]
    pub const fn pinned_until(mut self, expires_at: Option<&'a str>) -> Self {
        self.pinned = true;
        self.expires_at = expires_at;
        self
    }
}

const BACKUP_COLUMNS: &str = "id, operation_id, original_path, backup_path, content_hash, size, created_at, \
     pinned, expires_at, restore_count";

/// Records one backup in the catalogue and returns its id.
///
/// The bytes themselves are written by the caller (at-fs/TC-014); this row is
/// the index that makes them restorable. `backup_path` is unique — cataloguing
/// the same path twice is a constraint error.
///
/// # Errors
///
/// [`DbError::Constraint`] when `backup_path` is already catalogued, or
/// [`DbError`] when the insert fails.
pub fn record_backup(connection: &Connection, backup: &NewBackup<'_>) -> Result<String, DbError> {
    let id = backup
        .id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("bkp_{}", Ulid::generate()));
    connection.execute(
        "INSERT INTO backups \
             (id, operation_id, original_path, backup_path, content_hash, size, pinned, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            backup.operation_id,
            backup.original_path,
            backup.backup_path,
            backup.content_hash,
            backup.size,
            i64::from(backup.pinned),
            backup.expires_at,
        ],
    )?;
    Ok(id)
}

/// One catalogue row by id, when it exists.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn backup_by_id(connection: &Connection, id: &str) -> Result<Option<Backup>, DbError> {
    connection
        .query_row(
            &format!("SELECT {BACKUP_COLUMNS} FROM backups WHERE id = ?1"),
            [id],
            backup_from_row,
        )
        .optional()
        .map_err(DbError::from)
}

/// Every backup of `original_path`, newest first.
///
/// `limit` of `None` returns the whole history; the drift screen passes a
/// small bound because it only links a file to its latest runs.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn backups_for_path(
    connection: &Connection,
    original_path: &str,
    limit: Option<usize>,
) -> Result<Vec<Backup>, DbError> {
    let effective_limit =
        limit.map(|limit| i64::try_from(limit.clamp(1, 10_000)).unwrap_or(i64::MAX));
    let mut statement = connection.prepare(&format!(
        "SELECT {BACKUP_COLUMNS} FROM backups \
          WHERE original_path = ?1 \
          ORDER BY created_at DESC, id DESC \
          LIMIT ?2"
    ))?;
    let rows = statement
        .query_map(params![original_path, effective_limit], backup_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every backup taken inside the `[from, to)` window, newest first.
///
/// `from`/`to` are instants; `created_at` is RFC 3339 text, so the comparison
/// converts through `unixepoch`. `None` leaves that end open.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn backups_between(
    connection: &Connection,
    from: Option<SystemTime>,
    to: Option<SystemTime>,
) -> Result<Vec<Backup>, DbError> {
    let from_seconds = from.map(unix_seconds);
    let to_seconds = to.map(unix_seconds);
    let mut statement = connection.prepare(&format!(
        "SELECT {BACKUP_COLUMNS} FROM backups \
          WHERE (?1 IS NULL OR created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')) \
            AND (?2 IS NULL OR created_at <  strftime('%Y-%m-%dT%H:%M:%fZ', ?2, 'unixepoch')) \
          ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map(params![from_seconds, to_seconds], backup_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every backup the operation `operation_id` took, oldest first.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn backups_for_operation(
    connection: &Connection,
    operation_id: &str,
) -> Result<Vec<Backup>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {BACKUP_COLUMNS} FROM backups \
          WHERE operation_id = ?1 \
          ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = statement
        .query_map([operation_id], backup_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Counts one restore against `id`.
///
/// Returns `false` when no row matched. The restore flow (TC-014) calls this
/// after a successful restore so the catalogue reflects use.
///
/// # Errors
///
/// [`DbError`] when the update fails.
pub fn mark_restored(connection: &Connection, id: &str) -> Result<bool, DbError> {
    let changed = connection.execute(
        "UPDATE backups SET restore_count = restore_count + 1 WHERE id = ?1",
        [id],
    )?;
    Ok(changed == 1)
}

/// How much durable, non-rebuildable history the database still holds.
///
/// `native_items` and `drift_events`-style scan state can be rebuilt from disk
/// (TF-019); operations, operation files, the backup catalogue, remote backup
/// configuration and settings cannot — after the index file is lost they are
/// simply gone. [`DurableHistory::unrecoverable`] names the empty ones so a
/// recovery flow can tell the user exactly what a deleted database cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DurableHistory {
    /// Rows in `operations`.
    pub operations: u64,
    /// Rows in `operation_files`.
    pub operation_files: u64,
    /// Rows in `backups`.
    pub backups: u64,
    /// Rows in `settings`.
    pub settings: u64,
    /// Whether `remote_backup_state` holds a configured repo (not the
    /// `unconfigured` default the migration seeds).
    pub remote_backup_configured: bool,
}

impl DurableHistory {
    /// Names of the durable tables that hold nothing — the state a deleted
    /// database cannot rebuild.
    ///
    /// Order is fixed: `operations`, `operation_files`, `backups`,
    /// `remote_backup_state`, `settings`.
    #[must_use]
    pub fn unrecoverable(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.operations == 0 {
            missing.push("operations");
        }
        if self.operation_files == 0 {
            missing.push("operation_files");
        }
        if self.backups == 0 {
            missing.push("backups");
        }
        if !self.remote_backup_configured {
            missing.push("remote_backup_state");
        }
        if self.settings == 0 {
            missing.push("settings");
        }
        missing
    }
}

/// Reads [`DurableHistory`] in one pass: five count queries plus the remote
/// state probe. Used by recovery flows after a database is recreated.
///
/// # Errors
///
/// [`DbError`] when a statement fails.
pub fn durable_history(connection: &Connection) -> Result<DurableHistory, DbError> {
    let operations = count_rows(connection, "operations")?;
    let operation_files = count_rows(connection, "operation_files")?;
    let backups = count_rows(connection, "backups")?;
    let settings = count_rows(connection, "settings")?;
    let remote_backup_configured = connection.query_row(
        "SELECT COUNT(*) FROM remote_backup_state \
          WHERE id = 1 AND (repo_url IS NOT NULL OR status != 'unconfigured')",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    Ok(DurableHistory {
        operations,
        operation_files,
        backups,
        settings,
        remote_backup_configured,
    })
}

/// `SELECT COUNT(*)` for one of the durable tables.
fn count_rows(connection: &Connection, table: &'static str) -> Result<u64, DbError> {
    // `table` is a compile-time literal from this module, never caller input.
    let count: i64 = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
}

/// Maps one `backups` row; every query above selects [`BACKUP_COLUMNS`].
fn backup_from_row(row: &Row<'_>) -> rusqlite::Result<Backup> {
    Ok(Backup {
        id: row.get(0)?,
        operation_id: row.get(1)?,
        original_path: row.get(2)?,
        backup_path: row.get(3)?,
        content_hash: row.get(4)?,
        size: row.get(5)?,
        created_at: row.get(6)?,
        pinned: row.get::<_, i64>(7)? != 0,
        expires_at: row.get(8)?,
        restore_count: row.get(9)?,
    })
}

/// Unix seconds of `at`, saturating at the representable range.
fn unix_seconds(at: SystemTime) -> i64 {
    match at.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let statements = [
            "INSERT INTO backups (id, operation_id, original_path, backup_path, content_hash, size, pinned, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            "SELECT id, operation_id, original_path, backup_path, content_hash, size, created_at, pinned, expires_at, restore_count FROM backups WHERE id = ?1",
            "SELECT id, operation_id, original_path, backup_path, content_hash, size, created_at, pinned, expires_at, restore_count FROM backups WHERE original_path = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
            "SELECT id, operation_id, original_path, backup_path, content_hash, size, created_at, pinned, expires_at, restore_count FROM backups WHERE (?1 IS NULL OR created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')) AND (?2 IS NULL OR created_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?2, 'unixepoch')) ORDER BY created_at DESC, id DESC",
            "SELECT id, operation_id, original_path, backup_path, content_hash, size, created_at, pinned, expires_at, restore_count FROM backups WHERE operation_id = ?1 ORDER BY created_at ASC, id ASC",
            "UPDATE backups SET restore_count = restore_count + 1 WHERE id = ?1",
            "SELECT COUNT(*) FROM remote_backup_state WHERE id = 1 AND (repo_url IS NOT NULL OR status != 'unconfigured')",
        ];
        for statement in statements {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
            let lowered = statement.to_ascii_lowercase();
            for keyword in ["create ", "alter ", "drop "] {
                assert!(
                    !lowered.contains(keyword),
                    "statement must not change the schema: {statement}"
                );
            }
        }
    }
}
