//! Locks repository and queries over the `locks` table (TA-041).
//!
//! The `locks` table is owned by TF-014 (migration v5); this module adds the
//! repository and query layer on top — no DDL. It provides:
//!
//! - [`upsert_lock`] — insert or update the lock for `(item_id, scope)`.
//! - [`remove_lock`] — delete a lock.
//! - [`find_lock`] — lookup by item and scope.
//! - [`locks_for_item`] — every lock on one item.
//! - [`locks_for_scope`] — every lock at one scope.
//! - [`count_locks_for_scope`] — the lock count the overview card shows.
//!
//! All reads go through one prepared statement; none hold a write
//! transaction, so the UI can render while a scan runs elsewhere.

use at_model::{ItemId, Scope};
use rusqlite::{Connection, params};

use crate::DbError;
use crate::shared::{LockRow, lock_from_row};

/// Everything [`upsert_lock`] needs to write one lock.
#[derive(Clone, Debug)]
pub struct LockSpec<'a> {
    /// Item being locked.
    pub item_id: &'a ItemId,
    /// Scope whose definition is pinned.
    pub scope: Scope,
    /// Human-readable reason.
    pub reason: Option<&'a str>,
    /// Who placed the lock.
    pub locked_by: Option<&'a str>,
}

/// Insert or update the lock for `(item_id, scope)`.
///
/// `UNIQUE(item_id, scope)` makes the pair a single row: calling this twice
/// updates `reason`, `locked_by` and `locked_at` in place. `locked_at` is set
/// to the current UTC timestamp on every call.
///
/// # Errors
///
/// A SQLite failure.
pub fn upsert_lock(connection: &Connection, spec: &LockSpec<'_>) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO locks (item_id, scope, reason, locked_by, locked_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT (item_id, scope) \
         DO UPDATE SET reason = excluded.reason, \
                       locked_by = excluded.locked_by, \
                       locked_at = excluded.locked_at",
        params![
            spec.item_id.to_string(),
            spec.scope.wire_name(),
            spec.reason,
            spec.locked_by,
        ],
    )?;
    Ok(())
}

/// Remove the lock for `(item_id, scope)`.
///
/// Returns `true` when a row was deleted.
///
/// # Errors
///
/// A SQLite failure.
pub fn remove_lock(
    connection: &Connection,
    item_id: &ItemId,
    scope: Scope,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        "DELETE FROM locks WHERE item_id = ?1 AND scope = ?2",
        params![item_id.to_string(), scope.wire_name()],
    )?;
    Ok(changed > 0)
}

/// The lock for `(item_id, scope)`, when one exists.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn find_lock(
    connection: &Connection,
    item_id: &ItemId,
    scope: Scope,
) -> Result<Option<LockRow>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, item_id, scope, reason, locked_by, locked_at \
           FROM locks WHERE item_id = ?1 AND scope = ?2",
    )?;
    let row = statement.query_row(
        params![item_id.to_string(), scope.wire_name()],
        lock_from_row,
    );
    match row {
        Ok(lock) => Ok(Some(lock)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(DbError::from(e)),
    }
}

/// Every lock on one item, across scopes.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn locks_for_item(connection: &Connection, item_id: &ItemId) -> Result<Vec<LockRow>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, item_id, scope, reason, locked_by, locked_at \
           FROM locks WHERE item_id = ?1 ORDER BY scope",
    )?;
    let rows = statement
        .query_map(params![item_id.to_string()], lock_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every lock at one scope, across items.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn locks_for_scope(connection: &Connection, scope: Scope) -> Result<Vec<LockRow>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, item_id, scope, reason, locked_by, locked_at \
           FROM locks WHERE scope = ?1 ORDER BY item_id",
    )?;
    let rows = statement
        .query_map(params![scope.wire_name()], lock_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// How many locks exist at one scope.
///
/// The overview card reads this; an unknown or empty scope returns `0`.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn count_locks_for_scope(connection: &Connection, scope: Scope) -> Result<i64, DbError> {
    let mut statement = connection.prepare("SELECT COUNT(*) FROM locks WHERE scope = ?1")?;
    let count = statement.query_row(params![scope.wire_name()], |row| row.get(0))?;
    Ok(count)
}

/// Every lock in the database, for the locks screen.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn list_locks(connection: &Connection) -> Result<Vec<LockRow>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, item_id, scope, reason, locked_by, locked_at \
           FROM locks ORDER BY scope, item_id",
    )?;
    let rows = statement
        .query_map([], lock_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
