//! Skill version history over `item_versions` (TS-002).
//!
//! Every successful skill write records exactly one `item_versions` row whose
//! `content_hash` is the SHA-256 over the sorted relative paths and bytes of
//! the skill folder (see `at_store::hash_skill_folder`, which the caller runs —
//! this module never touches the filesystem). The row links back to the
//! `operations` row that produced it through `operation_id`, and `scope`
//! records where the version was captured, so promote/move/import/update all
//! leave an audit trail the skill detail screen renders newest-first.
//!
//! `item_versions` has `UNIQUE (item_id, content_hash)`: one row per distinct
//! content, not one row per write call. [`record_skill_version`] upserts on
//! that key — a write whose bytes match an earlier version refreshes
//! `captured_at`/`last_seen_at` and adopts the newer `operation_id`, while a
//! rescan that carries no operation keeps the original link
//! (`COALESCE(excluded.operation_id, item_versions.operation_id)`). This is
//! what lets a file rescan mark `last_seen_at` without erasing history.
//!
//! ## History is not rebuildable from files
//!
//! `content_hash`, `path`, `size` and `mtime` can be recomputed by scanning the
//! store (TF-019 does exactly that for the derived projection), but
//! `operation_id`, `version_label` and the link to `operations` /
//! `operation_files` are write-time facts no scan can reconstruct. A rebuild
//! therefore repopulates hashes while the operation linkage written here is
//! the durable history this module exists to keep. Do not "fix" a missing
//! `operation_id` by re-deriving it.
//!
//! ## The stored file map
//!
//! `item_versions` has no column for a per-file map and this story adds no DDL,
//! so [`version_content`] returns the file map the producing operation
//! recorded: the `operation_files` rows of the linked operation (path, action,
//! before/after hash, byte counts). For a skill package write those rows are
//! the package's files; the UI compares each `after_hash` against a fresh scan
//! of the current folder to render the diff. Versions captured without an
//! operation (a bare scan) return an empty file list — diff those by
//! `content_hash` only. When retention prunes the operation row the file list
//! empties but the version row and its hash remain.
//!
//! Deleting an item cascades its `item_versions` rows (TF-012); skill deletes
//! must therefore mark the item missing or keep the row (TS-014) for history
//! to survive, matching TA-043's `state` convention.

use std::str::FromStr;

use at_model::{ItemId, Scope};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::DbError;
use crate::ops::{FileAction, OperationActor, OperationKind, OperationStatus};

/// One `item_versions` row plus the operation that produced it, as
/// [`versions_for`] returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionEntry {
    /// Version row id (monotonic within the database).
    pub id: i64,
    /// Item the version belongs to.
    pub item_id: ItemId,
    /// Scope the version was captured at.
    pub scope: Scope,
    /// SHA-256 of the canonical content (for a skill, the folder tree hash).
    pub content_hash: String,
    /// Store-relative path of the captured copy, when recorded.
    pub path: Option<String>,
    /// Byte size of the captured copy, when recorded.
    pub size: Option<i64>,
    /// Modification time of the captured copy in unix milliseconds.
    pub mtime: Option<i64>,
    /// When this content was first captured (RFC 3339 text).
    pub captured_at: String,
    /// Free-form label (release name, import batch, …), when recorded.
    pub version_label: Option<String>,
    /// Operation that produced this version, when the write went through one.
    pub operation_id: Option<String>,
    /// When a scan last saw this content (RFC 3339 text).
    pub last_seen_at: Option<String>,
    /// Kind of the producing operation, joined from `operations`.
    pub operation_kind: Option<OperationKind>,
    /// Actor of the producing operation.
    pub actor: Option<OperationActor>,
    /// Status of the producing operation.
    pub operation_status: Option<OperationStatus>,
    /// How many `operation_files` rows the producing operation recorded.
    pub operation_file_count: i64,
}

/// One file of the stored file map returned by [`version_content`].
///
/// Mirrors an `operation_files` row: `after_hash` is the content hash the
/// version recorded for `path`; `action` says whether the file was created,
/// modified, deleted, linked or moved by the producing operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionFile {
    /// Path the operation touched, as recorded.
    pub path: Option<String>,
    /// What the operation did to it.
    pub action: Option<FileAction>,
    /// Content hash before the operation.
    pub before_hash: Option<String>,
    /// Content hash after the operation — the hash this version captured.
    pub after_hash: Option<String>,
    /// Backup taken for the change, when one exists.
    pub backup_id: Option<String>,
    /// Size before the operation.
    pub bytes_before: Option<i64>,
    /// Size after the operation.
    pub bytes_after: Option<i64>,
}

/// A version row together with its stored file map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionContent {
    /// The version row and its operation linkage.
    pub version: VersionEntry,
    /// Files the producing operation recorded, in apply order; empty when the
    /// version was captured without an operation or the operation was pruned.
    pub files: Vec<VersionFile>,
}

/// Everything [`record_skill_version`] writes into one `item_versions` row.
#[derive(Clone, Debug)]
pub struct NewVersion<'a> {
    /// Item the version belongs to; must already exist in `items`.
    pub item_id: &'a ItemId,
    /// Scope the version was captured at.
    pub scope: Scope,
    /// Content hash of the write (SHA-256 hex, e.g. `hash_skill_folder`).
    pub content_hash: &'a str,
    /// Store-relative path of the captured copy.
    pub path: Option<&'a str>,
    /// Byte size of the captured copy.
    pub size: Option<i64>,
    /// Modification time of the captured copy in unix milliseconds.
    pub mtime: Option<i64>,
    /// Free-form label for the version.
    pub version_label: Option<&'a str>,
    /// Operation that produced the write; `None` for scan-captured versions.
    pub operation_id: Option<&'a str>,
}

impl<'a> NewVersion<'a> {
    /// A minimal version record: item, scope and content hash.
    #[must_use]
    pub const fn new(item_id: &'a ItemId, scope: Scope, content_hash: &'a str) -> Self {
        Self {
            item_id,
            scope,
            content_hash,
            path: None,
            size: None,
            mtime: None,
            version_label: None,
            operation_id: None,
        }
    }

    /// Sets the captured path.
    #[must_use]
    pub const fn with_path(mut self, path: &'a str) -> Self {
        self.path = Some(path);
        self
    }

    /// Sets size and mtime together.
    #[must_use]
    pub const fn with_size_mtime(mut self, size: i64, mtime: i64) -> Self {
        self.size = Some(size);
        self.mtime = Some(mtime);
        self
    }

    /// Sets the version label.
    #[must_use]
    pub const fn with_version_label(mut self, label: &'a str) -> Self {
        self.version_label = Some(label);
        self
    }

    /// Links the version to the operation that produced it.
    #[must_use]
    pub const fn with_operation(mut self, operation_id: &'a str) -> Self {
        self.operation_id = Some(operation_id);
        self
    }
}

const VERSION_LIST_COLUMNS: &str = "v.id, v.item_id, v.scope, v.content_hash, v.path, \
     v.size, v.mtime, v.captured_at, v.version_label, v.operation_id, \
     v.last_seen_at, o.kind, o.actor, o.status, \
     (SELECT COUNT(*) FROM operation_files f WHERE f.operation_id = v.operation_id) \
         AS file_count";

const VERSION_ROW_SQL: &str = "\
SELECT id, item_id, scope, content_hash, path, size, mtime, captured_at, \
       version_label, operation_id, last_seen_at \
  FROM item_versions WHERE id = ?1";

const VERSION_FILES_SQL: &str = "\
SELECT path, action, before_hash, after_hash, backup_id, bytes_before, bytes_after \
  FROM operation_files WHERE operation_id = ?1 ORDER BY id ASC";

const OPERATION_FOR_VERSION_SQL: &str = "\
SELECT kind, actor, status FROM operations WHERE id = ?1";

const MARK_SEEN_SQL: &str = "\
UPDATE item_versions \
   SET last_seen_at = COALESCE(?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
 WHERE item_id = ?1 AND content_hash = ?2";

/// Records one version row for a successful skill write.
///
/// Upserts on `UNIQUE (item_id, content_hash)`: new content inserts a row;
/// content already recorded refreshes `captured_at`/`last_seen_at` and adopts
/// the new `operation_id` when one is supplied (a scan with `operation_id:
/// None` keeps the existing link). Exactly one row exists per distinct content
/// hash afterwards, and the hash changes whenever any byte of any file in the
/// skill folder changes because the caller hashes the whole tree.
///
/// # Errors
///
/// [`DbError::Constraint`] when `item_id` has no `items` row (the foreign key
/// cascades the other way), or [`DbError`] when the statement fails.
pub fn record_skill_version(connection: &Connection, new: &NewVersion<'_>) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO item_versions \
             (item_id, scope, content_hash, path, size, mtime, version_label, \
              operation_id, last_seen_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(item_id, content_hash) DO UPDATE SET \
             scope = excluded.scope, \
             path = excluded.path, \
             size = excluded.size, \
             mtime = excluded.mtime, \
             version_label = COALESCE(excluded.version_label, item_versions.version_label), \
             operation_id = COALESCE(excluded.operation_id, item_versions.operation_id)",
        params![
            new.item_id.to_string(),
            new.scope.wire_name(),
            new.content_hash,
            new.path,
            new.size,
            new.mtime,
            new.version_label,
            new.operation_id,
        ],
    )?;
    Ok(())
}

/// Marks the version for `(item_id, content_hash)` as seen by a scan.
///
/// This is the rescan half of the version lifecycle: unchanged content writes
/// no new row, it only refreshes `last_seen_at`. `seen_at` is an RFC 3339
/// timestamp for deterministic tests; `None` uses the database clock.
///
/// Returns whether a matching version row exists.
///
/// # Errors
/// A SQLite failure.
pub fn mark_version_seen(
    connection: &Connection,
    item_id: &ItemId,
    content_hash: &str,
    seen_at: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        MARK_SEEN_SQL,
        params![item_id.to_string(), content_hash, seen_at],
    )?;
    Ok(changed == 1)
}

/// Every version of `item_id` at `scope`, newest first.
///
/// Newest is `captured_at` descending with `id` descending as the tiebreak, so
/// two versions captured in the same millisecond still order by write sequence.
/// Each row carries the producing operation's kind, actor, status and file
/// count; rows captured without an operation read those fields as `None`.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored enum value is unknown to this
/// binary.
pub fn versions_for(
    connection: &Connection,
    item_id: &ItemId,
    scope: Scope,
) -> Result<Vec<VersionEntry>, DbError> {
    let sql = format!(
        "SELECT {VERSION_LIST_COLUMNS} \
           FROM item_versions v \
           LEFT JOIN operations o ON o.id = v.operation_id \
          WHERE v.item_id = ?1 AND v.scope = ?2 \
          ORDER BY v.captured_at DESC, v.id DESC"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![item_id.to_string(), scope.wire_name()],
        raw_version_entry_from_row,
    )?;
    collect_versions(rows)
}

/// Every version of `item_id` across all scopes, newest first.
///
/// Same row shape as [`versions_for`]; use it for the skill detail history,
/// which shows where each version was captured via [`VersionEntry::scope`].
///
/// # Errors
/// Same as [`versions_for`].
pub fn versions_for_item(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Vec<VersionEntry>, DbError> {
    let sql = format!(
        "SELECT {VERSION_LIST_COLUMNS} \
           FROM item_versions v \
           LEFT JOIN operations o ON o.id = v.operation_id \
          WHERE v.item_id = ?1 \
          ORDER BY v.captured_at DESC, v.id DESC"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([item_id.to_string()], raw_version_entry_from_row)?;
    collect_versions(rows)
}

/// One version with its stored file map.
///
/// Returns `None` for an unknown version id. See the module documentation for
/// what the file map contains and when it is empty.
///
/// # Errors
///
/// [`DbError`] when a query fails or a stored enum value is unknown.
pub fn version_content(
    connection: &Connection,
    version_id: i64,
) -> Result<Option<VersionContent>, DbError> {
    let mut statement = connection.prepare(VERSION_ROW_SQL)?;
    let raw = statement
        .query_row([version_id], raw_version_row_from_row)
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };

    let operation = match raw.operation_id.as_deref() {
        Some(operation_id) => {
            let mut op = connection.prepare(OPERATION_FOR_VERSION_SQL)?;
            op.query_row([operation_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .optional()?
        }
        None => None,
    };
    let (operation_kind, actor, operation_status) = match operation {
        Some((kind, actor, status)) => (
            parse_optional(kind.as_deref())?,
            parse_optional(actor.as_deref())?,
            parse_optional(status.as_deref())?,
        ),
        None => (None, None, None),
    };

    let file_count = match raw.operation_id.as_deref() {
        Some(operation_id) => count_files(connection, operation_id)?,
        None => 0,
    };
    let files = match raw.operation_id.as_deref() {
        Some(operation_id) => version_files(connection, operation_id)?,
        None => Vec::new(),
    };

    let version = VersionEntry {
        id: raw.id,
        item_id: parse_item_id(&raw.item_id)?,
        scope: parse_scope(&raw.scope)?,
        content_hash: raw.content_hash,
        path: raw.path,
        size: raw.size,
        mtime: raw.mtime,
        captured_at: raw.captured_at,
        version_label: raw.version_label,
        operation_id: raw.operation_id,
        last_seen_at: raw.last_seen_at,
        operation_kind,
        actor,
        operation_status,
        operation_file_count: file_count,
    };

    Ok(Some(VersionContent { version, files }))
}

/// The `item_versions` columns before enum and item-id parsing.
struct RawVersionRow {
    id: i64,
    item_id: String,
    scope: String,
    content_hash: String,
    path: Option<String>,
    size: Option<i64>,
    mtime: Option<i64>,
    captured_at: String,
    version_label: Option<String>,
    operation_id: Option<String>,
    last_seen_at: Option<String>,
}

/// [`RawVersionRow`] plus the joined operation columns.
struct RawVersionEntry {
    row: RawVersionRow,
    operation_kind: Option<String>,
    actor: Option<String>,
    operation_status: Option<String>,
    file_count: i64,
}

/// One `operation_files` row before enum parsing.
struct RawVersionFile {
    path: Option<String>,
    action: Option<String>,
    before_hash: Option<String>,
    after_hash: Option<String>,
    backup_id: Option<String>,
    bytes_before: Option<i64>,
    bytes_after: Option<i64>,
}

fn raw_version_row_from_row(row: &Row<'_>) -> rusqlite::Result<RawVersionRow> {
    Ok(RawVersionRow {
        id: row.get(0)?,
        item_id: row.get(1)?,
        scope: row.get(2)?,
        content_hash: row.get(3)?,
        path: row.get(4)?,
        size: row.get(5)?,
        mtime: row.get(6)?,
        captured_at: row.get(7)?,
        version_label: row.get(8)?,
        operation_id: row.get(9)?,
        last_seen_at: row.get(10)?,
    })
}

fn raw_version_entry_from_row(row: &Row<'_>) -> rusqlite::Result<RawVersionEntry> {
    Ok(RawVersionEntry {
        row: RawVersionRow {
            id: row.get(0)?,
            item_id: row.get(1)?,
            scope: row.get(2)?,
            content_hash: row.get(3)?,
            path: row.get(4)?,
            size: row.get(5)?,
            mtime: row.get(6)?,
            captured_at: row.get(7)?,
            version_label: row.get(8)?,
            operation_id: row.get(9)?,
            last_seen_at: row.get(10)?,
        },
        operation_kind: row.get(11)?,
        actor: row.get(12)?,
        operation_status: row.get(13)?,
        file_count: row.get(14)?,
    })
}

impl TryFrom<RawVersionRow> for VersionEntry {
    type Error = DbError;

    fn try_from(raw: RawVersionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            item_id: parse_item_id(&raw.item_id)?,
            scope: parse_scope(&raw.scope)?,
            content_hash: raw.content_hash,
            path: raw.path,
            size: raw.size,
            mtime: raw.mtime,
            captured_at: raw.captured_at,
            version_label: raw.version_label,
            operation_id: raw.operation_id,
            last_seen_at: raw.last_seen_at,
            operation_kind: None,
            actor: None,
            operation_status: None,
            operation_file_count: 0,
        })
    }
}

fn version_entry_from_raw(raw: RawVersionEntry) -> Result<VersionEntry, DbError> {
    let mut entry = VersionEntry::try_from(raw.row)?;
    entry.operation_kind = parse_optional(raw.operation_kind.as_deref())?;
    entry.actor = parse_optional(raw.actor.as_deref())?;
    entry.operation_status = parse_optional(raw.operation_status.as_deref())?;
    entry.operation_file_count = raw.file_count;
    Ok(entry)
}

fn collect_versions(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<RawVersionEntry>>,
) -> Result<Vec<VersionEntry>, DbError> {
    let mut entries = Vec::new();
    for row in rows {
        entries.push(version_entry_from_raw(row?)?);
    }
    Ok(entries)
}

fn version_files(connection: &Connection, operation_id: &str) -> Result<Vec<VersionFile>, DbError> {
    let mut statement = connection.prepare(VERSION_FILES_SQL)?;
    let rows = statement.query_map([operation_id], |row| {
        Ok(RawVersionFile {
            path: row.get(0)?,
            action: row.get(1)?,
            before_hash: row.get(2)?,
            after_hash: row.get(3)?,
            backup_id: row.get(4)?,
            bytes_before: row.get(5)?,
            bytes_after: row.get(6)?,
        })
    })?;
    let mut files = Vec::new();
    for row in rows {
        let raw = row?;
        files.push(VersionFile {
            path: raw.path,
            action: parse_optional(raw.action.as_deref())?,
            before_hash: raw.before_hash,
            after_hash: raw.after_hash,
            backup_id: raw.backup_id,
            bytes_before: raw.bytes_before,
            bytes_after: raw.bytes_after,
        });
    }
    Ok(files)
}

fn count_files(connection: &Connection, operation_id: &str) -> Result<i64, DbError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM operation_files WHERE operation_id = ?1",
        [operation_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn parse_item_id(text: &str) -> Result<ItemId, DbError> {
    text.parse::<ItemId>()
        .map_err(|error| DbError::InvalidValue {
            field: "item_versions.item_id",
            reason: error.to_string(),
        })
}

fn parse_scope(text: &str) -> Result<Scope, DbError> {
    Scope::from_str(text).map_err(DbError::from)
}

fn parse_optional<T>(value: Option<&str>) -> Result<Option<T>, DbError>
where
    T: FromStr<Err = at_model::ParseWireError>,
{
    match value {
        Some(text) => Ok(Some(T::from_str(text)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every statement this module issues must compile against the shipped
    /// schema and must not change it.
    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        for statement in [
            "INSERT INTO item_versions (item_id, scope, content_hash, path, size, mtime, \
             version_label, operation_id, last_seen_at) VALUES ('itm_x', 'user', 'h', NULL, \
             NULL, NULL, NULL, NULL, 'now') ON CONFLICT(item_id, content_hash) DO UPDATE SET \
             scope = excluded.scope",
            MARK_SEEN_SQL,
            VERSION_ROW_SQL,
            VERSION_FILES_SQL,
            OPERATION_FOR_VERSION_SQL,
        ] {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
        }
    }
}
