//! Drift events repository over `drift_events` (migration v5, owned by TF-014).
//!
//! A drift event records one external edit AgenTuna noticed: the watcher
//! ([`crate::retention`] keeps the resolved history for 90 days) writes it
//! open, and the drift screen resolves it as `adopt`, `restore` or `ignore`.
//! [`record_drift`] opens an event, [`open_drift_events`] lists the unresolved
//! ones newest-first and [`resolve_drift`] closes one — idempotently, so a
//! double click or a retried request cannot rewrite the first resolution.
//!
//! This module issues no DDL: every statement is a query or row change against
//! the schema TF-014 created.
//!
//! ## Why the classification is not stored
//!
//! `drift_events` has no `kind` column and migrations v1..v9 are frozen, so the
//! `Drifted`/`LinkReplacedByFile`/`LinkRemoved`/`FileDeleted`/
//! `StoreFileEditedWhileLinked`/`Unreadable` classification stays on the
//! engine's `DriftNotice` and reaches the UI through the `agentuna:drift-detected`
//! event. Encoding it into `path`, `expected_hash` or `backup_id` would corrupt
//! values those columns are compared on; a resolution row is not a
//! classification. A future migration may add the column; until then the row
//! records identity, scope, path, both hashes and the detection time, which is
//! what the drift screen lists.

use at_model::{AgentId, Scope};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::DbError;

/// How a drift event was resolved.
///
/// The wire names match the `resolution` CHECK constraint in migration v5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DriftResolution {
    /// The on-disk version won; it was copied into the store.
    Adopt,
    /// AgenTuna's version won; it was written back to disk.
    Restore,
    /// The drift was dismissed; nothing was written.
    Ignore,
}

impl DriftResolution {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Adopt, Self::Restore, Self::Ignore];

    /// The value stored in `drift_events.resolution`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::Restore => "restore",
            Self::Ignore => "ignore",
        }
    }

    /// Parses a stored resolution.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known resolution.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|resolution| resolution.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "drift_events.resolution",
                reason: format!("unknown resolution `{value}`"),
            })
    }
}

impl std::fmt::Display for DriftResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `drift_events` row as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftEvent {
    /// Row id.
    pub id: i64,
    /// Canonical or native item the drift belongs to, when known.
    pub item_id: Option<String>,
    /// Agent whose target drifted.
    pub agent_id: AgentId,
    /// Scope of the drifted deployment.
    pub scope: Scope,
    /// Path that drifted.
    pub path: String,
    /// Hash the deployment expected.
    pub expected_hash: Option<String>,
    /// Hash actually found on disk.
    pub actual_hash: Option<String>,
    /// When drift was detected (RFC 3339 text).
    pub detected_at: String,
    /// When it was resolved (RFC 3339 text); `None` while open.
    pub resolved_at: Option<String>,
    /// Resolution taken; `None` while open.
    pub resolution: Option<DriftResolution>,
    /// Backup taken during the resolution, when one exists.
    pub backup_id: Option<String>,
}

impl DriftEvent {
    /// Whether the event still waits for a resolution.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.resolved_at.is_none()
    }
}

/// One detected external edit, as [`record_drift`] consumes it.
#[derive(Clone, Debug)]
pub struct NewDriftEvent<'a> {
    /// Canonical item id (`itm_…`) or native row id the drift belongs to.
    pub item_id: Option<&'a str>,
    /// Agent whose target drifted.
    pub agent_id: &'a AgentId,
    /// Scope of the drifted deployment.
    pub scope: Scope,
    /// Path that drifted.
    pub path: &'a str,
    /// Hash the deployment expected.
    pub expected_hash: Option<&'a str>,
    /// Hash actually found on disk.
    pub actual_hash: Option<&'a str>,
    /// RFC 3339 detection time; `None` stamps the current time.
    pub detected_at: Option<&'a str>,
}

impl<'a> NewDriftEvent<'a> {
    /// An open drift event for `path` under `agent_id`/`scope`.
    #[must_use]
    pub fn new(agent_id: &'a AgentId, scope: Scope, path: &'a str) -> Self {
        Self {
            item_id: None,
            agent_id,
            scope,
            path,
            expected_hash: None,
            actual_hash: None,
            detected_at: None,
        }
    }

    /// Sets the item the drift belongs to.
    #[must_use]
    pub fn for_item(mut self, item_id: &'a str) -> Self {
        self.item_id = Some(item_id);
        self
    }

    /// Sets both hashes.
    #[must_use]
    pub fn with_hashes(
        mut self,
        expected_hash: Option<&'a str>,
        actual_hash: Option<&'a str>,
    ) -> Self {
        self.expected_hash = expected_hash;
        self.actual_hash = actual_hash;
        self
    }

    /// Overrides the detection timestamp (RFC 3339), for deterministic callers.
    #[must_use]
    pub fn detected_at(mut self, detected_at: &'a str) -> Self {
        self.detected_at = Some(detected_at);
        self
    }
}

const EVENT_COLUMNS: &str = "id, item_id, agent_id, scope, path, expected_hash, actual_hash, \
     detected_at, resolved_at, resolution, backup_id";

/// Records one open drift event and returns its row id.
///
/// `resolved_at` and `resolution` start `NULL`; [`resolve_drift`] closes the
/// event later.
///
/// # Errors
///
/// [`DbError`] when the row cannot be inserted.
pub fn record_drift(connection: &Connection, event: &NewDriftEvent<'_>) -> Result<i64, DbError> {
    connection.execute(
        "INSERT INTO drift_events \
             (item_id, agent_id, scope, path, expected_hash, actual_hash, detected_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')))",
        params![
            event.item_id,
            event.agent_id.to_string(),
            event.scope.wire_name(),
            event.path,
            event.expected_hash,
            event.actual_hash,
            event.detected_at,
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// Every unresolved drift event, newest first.
///
/// `scope` of `None` lists every scope. Ordering is `detected_at` descending
/// with `id` descending as the tiebreak, so two events in the same
/// millisecond still sort by insertion order.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn open_drift_events(
    connection: &Connection,
    scope: Option<Scope>,
) -> Result<Vec<DriftEvent>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM drift_events \
          WHERE resolved_at IS NULL \
            AND (?1 IS NULL OR scope = ?1) \
          ORDER BY detected_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map(params![scope.map(Scope::wire_name)], drift_event_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every drift event for `item_id` — open and resolved — newest first.
///
/// Returns an empty list for an item that never drifted.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn drift_events_for_item(
    connection: &Connection,
    item_id: &str,
) -> Result<Vec<DriftEvent>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM drift_events \
          WHERE item_id = ?1 \
          ORDER BY detected_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map([item_id], drift_event_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One drift event by id, open or resolved.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn drift_event(connection: &Connection, id: i64) -> Result<Option<DriftEvent>, DbError> {
    connection
        .query_row(
            &format!("SELECT {EVENT_COLUMNS} FROM drift_events WHERE id = ?1"),
            [id],
            drift_event_from_row,
        )
        .optional()
        .map_err(DbError::from)
}

/// Resolves an open drift event, idempotently.
///
/// Writes `resolved_at` (now), `resolution` and, when given, `backup_id` — but
/// only while the event is still open. A second call, a retry after a crash or
/// a resolve of an unknown id returns `Ok(false)` and changes nothing: the
/// first resolution always wins.
///
/// # Errors
///
/// [`DbError`] when the update cannot be written.
pub fn resolve_drift(
    connection: &Connection,
    id: i64,
    resolution: DriftResolution,
    backup_id: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        "UPDATE drift_events \
            SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                resolution = ?2, \
                backup_id = COALESCE(?3, backup_id) \
          WHERE id = ?1 AND resolved_at IS NULL",
        params![id, resolution.as_str(), backup_id],
    )?;
    Ok(changed == 1)
}

/// Maps one `drift_events` row; every query above selects [`EVENT_COLUMNS`].
fn drift_event_from_row(row: &Row<'_>) -> rusqlite::Result<DriftEvent> {
    let agent_id: String = row.get(2)?;
    let scope: String = row.get(3)?;
    let resolution: Option<String> = row.get(9)?;
    Ok(DriftEvent {
        id: row.get(0)?,
        item_id: row.get(1)?,
        agent_id: agent_id.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        scope: scope.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        path: row.get(4)?,
        expected_hash: row.get(5)?,
        actual_hash: row.get(6)?,
        detected_at: row.get(7)?,
        resolved_at: row.get(8)?,
        resolution: resolution
            .as_deref()
            .map(|value| {
                DriftResolution::ALL
                    .iter()
                    .copied()
                    .find(|candidate| candidate.as_str().eq_ignore_ascii_case(value))
                    .ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            9,
                            rusqlite::types::Type::Text,
                            Box::new(UnknownDriftResolution {
                                value: value.to_owned(),
                            }),
                        )
                    })
            })
            .transpose()?,
        backup_id: row.get(10)?,
    })
}

/// Error for a stored `drift_events.resolution` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored drift resolution `{value}` is not one of adopt, restore, ignore")]
struct UnknownDriftResolution {
    value: String,
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let statements = [
            "INSERT INTO drift_events (item_id, agent_id, scope, path, expected_hash, actual_hash, detected_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')))",
            "SELECT id, item_id, agent_id, scope, path, expected_hash, actual_hash, detected_at, resolved_at, resolution, backup_id FROM drift_events WHERE resolved_at IS NULL AND (?1 IS NULL OR scope = ?1) ORDER BY detected_at DESC, id DESC",
            "SELECT id, item_id, agent_id, scope, path, expected_hash, actual_hash, detected_at, resolved_at, resolution, backup_id FROM drift_events WHERE item_id = ?1 ORDER BY detected_at DESC, id DESC",
            "SELECT id, item_id, agent_id, scope, path, expected_hash, actual_hash, detected_at, resolved_at, resolution, backup_id FROM drift_events WHERE id = ?1",
            "UPDATE drift_events SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), resolution = ?2, backup_id = COALESCE(?3, backup_id) WHERE id = ?1 AND resolved_at IS NULL",
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
