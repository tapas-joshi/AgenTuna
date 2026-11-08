//! Native items repository over `native_items` (migration v5, owned by TF-014).
//!
//! A native item is something found only in an agent's own folders — a skill,
//! MCP server or rule that never went through AgenTuna. [`replace_scan`] is the
//! transactional rescan entry point: it upserts every path the scan reported,
//! keeps `first_seen_at` for rows it already knew, refreshes `last_seen_at` and
//! marks the agent/scope rows the scan no longer sees as `missing` instead of
//! deleting them, so a vanished item can be recognised again.
//!
//! The table is rebuildable (TF-019 regenerates it from a disk scan); nothing
//! here is durable history.
//!
//! This module issues no DDL: every statement is a query or row change against
//! the schema TF-014 created.

use std::collections::BTreeSet;

use at_model::{AgentId, ItemId, ItemKind, Scope};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use ulid::Ulid;

use crate::DbError;

/// Separator between an agent configuration document and the entry key inside it.
///
/// An MCP configuration holds several servers in one document while `native_items.path` is
/// unique, so a native MCP row stores `<document>#<entry key>`: the document is the file the
/// agent loads, the fragment names the entry inside it. The split uses the rightmost separator,
/// so a document path that itself contains one stays readable; discovery refuses an entry key
/// containing it, which keeps the split unambiguous.
pub const MCP_ENTRY_SEPARATOR: char = '#';

/// The native-row path of one MCP entry inside `document`.
#[must_use]
pub fn mcp_entry_path(document: &str, name: &str) -> String {
    let mut path = String::with_capacity(document.len() + 1 + name.len());
    path.push_str(document);
    path.push(MCP_ENTRY_SEPARATOR);
    path.push_str(name);
    path
}

/// Splits a native MCP row path into its document and entry key.
///
/// Returns `None` for a path without the separator: that is a row recorded for a whole document
/// (an adoption or an older scan) rather than one MCP entry.
#[must_use]
pub fn split_mcp_entry_path(path: &str) -> Option<(&str, &str)> {
    path.rsplit_once(MCP_ENTRY_SEPARATOR)
}

/// How a native row's lifecycle stands.
///
/// The wire names match the `status` CHECK constraint in migration v5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NativeStatus {
    /// Seen by a scan and never adopted or dismissed.
    New,
    /// Adopted into the canonical store; `adopted_item_id` names the item.
    Adopted,
    /// Dismissed by the user; scans keep refreshing it but it stays hidden.
    Ignored,
    /// Not seen by the latest scan of its agent and scope.
    Missing,
}

impl NativeStatus {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::New, Self::Adopted, Self::Ignored, Self::Missing];

    /// The value stored in `native_items.status`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Adopted => "adopted",
            Self::Ignored => "ignored",
            Self::Missing => "missing",
        }
    }

    /// Parses a stored status.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known status.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "native_items.status",
                reason: format!("unknown status `{value}`"),
            })
    }
}

impl std::fmt::Display for NativeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `native_items` row as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeItem {
    /// Row id (`nat_<ulid>` for rows this repository created).
    pub id: String,
    /// Agent whose folder the item was found in.
    pub agent_id: AgentId,
    /// Scope of the agent folder the item lives under.
    pub scope: Scope,
    /// What the item is.
    pub kind: ItemKind,
    /// Display name reported by the scan.
    pub name: String,
    /// Filesystem path; the natural key of the row.
    pub path: String,
    /// Content hash the scan computed, when it did.
    pub content_hash: Option<String>,
    /// First scan that saw the path (RFC 3339 text).
    pub first_seen_at: String,
    /// Most recent scan that saw the path (RFC 3339 text).
    pub last_seen_at: String,
    /// Canonical item this row was adopted into, when adopted.
    pub adopted_item_id: Option<String>,
    /// Lifecycle status.
    pub status: NativeStatus,
}

/// One path a scan found, as [`replace_scan`] consumes it.
///
/// `agent_id` and `scope` are parameters of [`replace_scan`], not of the entry:
/// one call replaces exactly one agent's view of one scope.
#[derive(Clone, Debug)]
pub struct NativeScanEntry<'a> {
    /// What the item is.
    pub kind: ItemKind,
    /// Display name.
    pub name: &'a str,
    /// Absolute path; the row's natural key.
    pub path: &'a str,
    /// Content hash the scanner computed, when available.
    pub content_hash: Option<&'a str>,
}

impl<'a> NativeScanEntry<'a> {
    /// A scan entry with only kind, name and path set.
    #[must_use]
    pub const fn new(kind: ItemKind, name: &'a str, path: &'a str) -> Self {
        Self {
            kind,
            name,
            path,
            content_hash: None,
        }
    }

    /// Sets the content hash.
    #[must_use]
    pub const fn with_content_hash(mut self, content_hash: &'a str) -> Self {
        self.content_hash = Some(content_hash);
        self
    }
}

/// One native path to associate with an adopted canonical item.
///
/// `native_items.path` is the unique key, so provide one entry per distinct path. The existing
/// schema stores one agent for each path.
#[derive(Clone, Copy, Debug)]
pub struct NativeAdoptionPath<'a> {
    /// Agent whose native folder contains this path.
    pub agent_id: &'a AgentId,
    /// Scope of the native folder.
    pub scope: Scope,
    /// What the item is.
    pub kind: ItemKind,
    /// Display name reported by the scan.
    pub name: &'a str,
    /// Native filesystem path; the row's natural key.
    pub path: &'a str,
    /// Content hash reported by the scan, when available.
    pub content_hash: Option<&'a str>,
}

/// What one [`replace_scan`] call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanReplaceReport {
    /// Rows inserted or refreshed by path.
    pub upserted: u64,
    /// Rows whose `status` became `missing` because the scan no longer sees them.
    pub marked_missing: u64,
    /// Rows that were `missing` and are visible again after this scan.
    pub resurrected: u64,
}

/// Filters for [`list_native_items`].
///
/// Every field is optional; an unset filter matches every row. `include_missing`
/// defaults to `false` because missing rows are history, not findings.
#[derive(Clone, Debug, Default)]
pub struct NativeItemFilter {
    /// Only this scope.
    pub scope: Option<Scope>,
    /// Only this agent.
    pub agent_id: Option<AgentId>,
    /// Only this lifecycle status.
    pub status: Option<NativeStatus>,
    /// Include `missing` rows (default: excluded).
    pub include_missing: bool,
}

/// Replaces one agent's native rows for `scope` with `entries`, atomically.
///
/// The scan is the source of truth for the paths it covers:
///
/// - every entry upserts by `path` — a row the scan already knew keeps its id
///   and `first_seen_at`, takes the new name/kind/hash and a fresh
///   `last_seen_at`;
/// - a `missing` row whose path is seen again comes back as `new` (or `adopted`
///   when it carries an `adopted_item_id`); `new`, `adopted` and `ignored`
///   statuses are never rewritten by a scan;
/// - rows of the same `(agent_id, scope)` whose path the scan did not report
///   are marked `missing` — other agents and other scopes are untouched.
///
/// Everything runs in one transaction: a failed upsert leaves the previous
/// scan state intact.
///
/// # Errors
///
/// [`DbError`] when a statement fails or a stored value cannot be written.
pub fn replace_scan(
    connection: &Connection,
    agent_id: &AgentId,
    scope: Scope,
    entries: &[NativeScanEntry<'_>],
) -> Result<ScanReplaceReport, DbError> {
    let transaction = connection.unchecked_transaction()?;

    // Snapshot which of this slice's rows are missing *before* the upserts so
    // `resurrected` counts exactly the rows this scan brought back.
    let mut missing_paths: Vec<String> = transaction
        .prepare(
            "SELECT path FROM native_items \
              WHERE agent_id = ?1 AND scope = ?2 AND status = 'missing'",
        )?
        .query_map(params![agent_id.to_string(), scope.wire_name()], |row| {
            row.get(0)
        })?
        .collect::<Result<_, _>>()?;

    let mut report = ScanReplaceReport::default();
    for entry in entries {
        let id = format!("nat_{}", Ulid::generate());
        transaction.execute(
            "INSERT INTO native_items \
                 (id, agent_id, scope, kind, name, path, content_hash, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'new') \
             ON CONFLICT(path) DO UPDATE SET \
                 agent_id = excluded.agent_id, \
                 scope = excluded.scope, \
                 kind = excluded.kind, \
                 name = excluded.name, \
                 content_hash = excluded.content_hash, \
                 last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 status = CASE \
                     WHEN native_items.status = 'missing' THEN \
                         CASE WHEN native_items.adopted_item_id IS NULL \
                              THEN 'new' ELSE 'adopted' END \
                     ELSE native_items.status END",
            params![
                id,
                agent_id.to_string(),
                scope.wire_name(),
                entry.kind.wire_name(),
                entry.name,
                entry.path,
                entry.content_hash,
            ],
        )?;
        report.upserted += 1;
    }

    report.marked_missing = mark_missing(&transaction, agent_id, scope, entries)?;

    if !missing_paths.is_empty() {
        missing_paths.sort_unstable();
        let seen: std::collections::BTreeSet<&str> =
            entries.iter().map(|entry| entry.path).collect();
        report.resurrected = missing_paths
            .iter()
            .filter(|path| seen.contains(path.as_str()))
            .count() as u64;
    }

    transaction.commit()?;
    Ok(report)
}

/// Creates or refreshes native rows for `paths` and associates them with `item_id`.
///
/// Each distinct path is inserted or updated as `adopted`; an existing row keeps its id and
/// `first_seen_at`, while its scan metadata and `last_seen_at` are refreshed. Missing rows are
/// created from the supplied scan metadata. All changes use the caller's transaction, allowing
/// the caller to commit the canonical item and every native path/agent together.
///
/// # Errors
///
/// Returns [`DbError::InvalidValue`] if `paths` contains the same path more than once, or a
/// SQLite error when a row cannot be written.
pub fn adopt_paths(
    transaction: &Transaction<'_>,
    paths: &[NativeAdoptionPath<'_>],
    item_id: &ItemId,
) -> Result<usize, DbError> {
    let mut seen_paths = BTreeSet::new();
    for entry in paths {
        if !seen_paths.insert(entry.path) {
            return Err(DbError::InvalidValue {
                field: "native_items.path",
                reason: format!(
                    "path `{}` appears more than once in one adoption",
                    entry.path
                ),
            });
        }
    }

    let item_id = item_id.to_string();
    let mut statement = transaction.prepare(
        "INSERT INTO native_items \
             (id, agent_id, scope, kind, name, path, content_hash, adopted_item_id, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'adopted') \
         ON CONFLICT(path) DO UPDATE SET \
             agent_id = excluded.agent_id, \
             scope = excluded.scope, \
             kind = excluded.kind, \
             name = excluded.name, \
             content_hash = excluded.content_hash, \
             adopted_item_id = excluded.adopted_item_id, \
             last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             status = 'adopted'",
    )?;
    let mut adopted = 0;
    for entry in paths {
        let id = format!("nat_{}", Ulid::generate());
        statement.execute(params![
            id,
            entry.agent_id.to_string(),
            entry.scope.wire_name(),
            entry.kind.wire_name(),
            entry.name,
            entry.path,
            entry.content_hash,
            item_id,
        ])?;
        adopted += 1;
    }
    Ok(adopted)
}

/// Clears the adoption of every native row that points at `item_id`.
///
/// The rows keep their history but become unmanaged findings again, so an undo of the adopt
/// does not leave a dangling `adopted_item_id`.
///
/// # Errors
///
/// [`DbError`] when the update fails.
pub fn release_adoptions(connection: &Connection, item_id: &ItemId) -> Result<usize, DbError> {
    let released = connection.execute(
        "UPDATE native_items             SET adopted_item_id = NULL, status = 'new'           WHERE adopted_item_id = ?1",
        [item_id.to_string()],
    )?;
    Ok(released)
}

/// How many rows the index currently holds in `native_items`, missing ones included.
///
/// The startup log reports what the index holds after a rebuild and its follow-up native
/// refresh; the rebuild transaction itself counts before the refresh writes its discoveries.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn count_native_items(connection: &Connection) -> Result<u64, DbError> {
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM native_items", [], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Every native row matching `filter`, ordered by agent, scope, kind, name.
///
/// Missing rows are excluded unless [`NativeItemFilter::include_missing`] is
/// set — they are scan history, not findings.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn list_native_items(
    connection: &Connection,
    filter: &NativeItemFilter,
) -> Result<Vec<NativeItem>, DbError> {
    let mut sql = String::from(
        "SELECT id, agent_id, scope, kind, name, path, content_hash, first_seen_at, \
                last_seen_at, adopted_item_id, status FROM native_items WHERE 1 = 1",
    );
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(scope) = filter.scope {
        sql.push_str(" AND scope = ?");
        values.push(Box::new(scope.wire_name().to_owned()));
    }
    if let Some(agent) = filter.agent_id.clone() {
        sql.push_str(" AND agent_id = ?");
        values.push(Box::new(agent.to_string()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND status = ?");
        values.push(Box::new(status.as_str().to_owned()));
    } else if !filter.include_missing {
        sql.push_str(" AND status != 'missing'");
    }
    sql.push_str(" ORDER BY agent_id ASC, scope ASC, kind ASC, name ASC, path ASC");

    let mut statement = connection.prepare(&sql)?;
    let params = rusqlite::params_from_iter(values.iter().map(|value| value.as_ref()));
    let rows = statement
        .query_map(params, native_item_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The native row at `path`, when one exists (missing rows included).
///
/// `path` is the natural key, so at most one row can match.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn native_item_by_path(
    connection: &Connection,
    path: &str,
) -> Result<Option<NativeItem>, DbError> {
    connection
        .query_row(
            "SELECT id, agent_id, scope, kind, name, path, content_hash, first_seen_at, \
                    last_seen_at, adopted_item_id, status \
               FROM native_items WHERE path = ?1",
            [path],
            native_item_from_row,
        )
        .optional()
        .map_err(DbError::from)
}

/// Marks every `(agent_id, scope)` row the scan did not see as `missing`.
///
/// Seen paths are exactly the entries just upserted; the `NOT IN` list names
/// them explicitly so two scans inside the same millisecond cannot
/// mis-mark each other's rows.
fn mark_missing(
    connection: &Connection,
    agent_id: &AgentId,
    scope: Scope,
    entries: &[NativeScanEntry<'_>],
) -> Result<u64, DbError> {
    if entries.is_empty() {
        // An empty scan means the agent's scope folder is gone or empty: every
        // row in this slice is stale.
        let changed = connection.execute(
            "UPDATE native_items SET status = 'missing' \
              WHERE agent_id = ?1 AND scope = ?2 AND status != 'missing'",
            params![agent_id.to_string(), scope.wire_name()],
        )?;
        return Ok(u64::try_from(changed).unwrap_or(u64::MAX));
    }

    let placeholders = entries.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "UPDATE native_items SET status = 'missing' \
          WHERE agent_id = ? AND scope = ? AND status != 'missing' \
            AND path NOT IN ({placeholders})"
    );
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(agent_id.to_string()),
        Box::new(scope.wire_name().to_owned()),
    ];
    for entry in entries {
        values.push(Box::new(entry.path.to_owned()));
    }
    let changed = connection.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|value| value.as_ref())),
    )?;
    Ok(u64::try_from(changed).unwrap_or(u64::MAX))
}

/// Maps one `native_items` row.
fn native_item_from_row(row: &Row<'_>) -> rusqlite::Result<NativeItem> {
    let agent_id: String = row.get(1)?;
    let scope: String = row.get(2)?;
    let kind: String = row.get(3)?;
    let status: String = row.get(10)?;
    Ok(NativeItem {
        id: row.get(0)?,
        agent_id: agent_id.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        scope: scope.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        kind: kind.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        name: row.get(4)?,
        path: row.get(5)?,
        content_hash: row.get(6)?,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        adopted_item_id: row.get(9)?,
        status: NativeStatus::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(&status))
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(UnknownNativeStatus {
                        value: status.clone(),
                    }),
                )
            })?,
    })
}

/// Error for a stored `native_items.status` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored native status `{value}` is not one of new, adopted, ignored, missing")]
struct UnknownNativeStatus {
    value: String,
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let statements = [
            "SELECT path FROM native_items WHERE agent_id = ?1 AND scope = ?2 AND status = 'missing'",
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, content_hash, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'new') ON CONFLICT(path) DO UPDATE SET agent_id = excluded.agent_id, scope = excluded.scope, kind = excluded.kind, name = excluded.name, content_hash = excluded.content_hash, last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), status = CASE WHEN native_items.status = 'missing' THEN CASE WHEN native_items.adopted_item_id IS NULL THEN 'new' ELSE 'adopted' END ELSE native_items.status END",
            "SELECT id, agent_id, scope, kind, name, path, content_hash, first_seen_at, last_seen_at, adopted_item_id, status FROM native_items WHERE 1 = 1 ORDER BY agent_id ASC, scope ASC, kind ASC, name ASC, path ASC",
            "SELECT id, agent_id, scope, kind, name, path, content_hash, first_seen_at, last_seen_at, adopted_item_id, status FROM native_items WHERE path = ?1",
            "UPDATE native_items SET status = 'missing' WHERE agent_id = ?1 AND scope = ?2 AND status != 'missing'",
            "UPDATE native_items SET status = 'missing' WHERE agent_id = ? AND scope = ? AND status != 'missing' AND path NOT IN (?)",
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
