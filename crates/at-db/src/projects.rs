//! Project registry: canonical paths, display names, hidden state, and the
//! per-project item counts the project list shows (TU-011).
//!
//! [`add_project`] canonicalises the path before it is stored, so adding the
//! same folder through a symlink resolves to the existing row instead of a
//! second one; the `projects.path` unique index compares case-insensitively for
//! case-insensitive volumes. Every write of `last_opened_at` is mirrored into
//! `last_used_at` (TF-011 names the column, TU-011 reads it).
//!
//! The summary queries read `items` (migration v3, TF-012) and `deployments`
//! (migration v4, TF-013) to project skill/server/rule counts in the same
//! statement as the project rows, so callers never issue a per-row query.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::{Database, DbError};

/// A project registry write failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", .path.display())]
pub struct ProjectRegistryError {
    /// Project path whose registry row could not be updated.
    pub path: PathBuf,
    /// Database failure details.
    pub message: String,
}

/// Registry operations required to reconcile a filesystem scan.
pub trait ProjectRegistry {
    /// Registers a newly discovered project.
    ///
    /// # Errors
    /// Returns a database error with the affected project path.
    fn insert_discovered(
        &mut self,
        path: &Path,
        name: &str,
        source_root: &Path,
    ) -> Result<(), ProjectRegistryError>;

    /// Hides a registered project that no longer exists or has no project marker.
    ///
    /// # Errors
    /// Returns a database error with the affected project path.
    fn mark_missing(&mut self, path: &Path) -> Result<(), ProjectRegistryError>;
}

impl ProjectRegistry for Database {
    fn insert_discovered(
        &mut self,
        path: &Path,
        name: &str,
        source_root: &Path,
    ) -> Result<(), ProjectRegistryError> {
        add_project(self.connection(), path, Some(name), Some(source_root))
            .map(|_| ())
            .map_err(|error| ProjectRegistryError {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
    }

    fn mark_missing(&mut self, path: &Path) -> Result<(), ProjectRegistryError> {
        let stored = lookup_path(path);
        self.connection()
            .execute(
                "UPDATE projects SET hidden = 1 WHERE path = ?1",
                [stored.to_string_lossy().as_ref()],
            )
            .map(|_| ())
            .map_err(|error| ProjectRegistryError {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
    }
}

/// One project as the project list renders it, including counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummary {
    /// Opaque project id (`prj_` followed by a UUIDv7).
    pub id: String,
    /// Display name; falls back to the path when the row has none.
    pub name: String,
    /// Canonical filesystem path stored in the registry.
    pub path: String,
    /// Discovery root this project was found under, when it came from a scan.
    pub source_root: Option<String>,
    /// When the project was last selected or added, as UTC RFC 3339 text.
    pub last_used_at: Option<String>,
    /// Hidden projects stay registered (their deployments and history stay
    /// intact) but the project picker does not offer them.
    pub hidden: bool,
    /// Project-scoped skills, plus skills deployed into the project.
    pub skill_count: i64,
    /// Project-scoped MCP servers, plus servers deployed into the project.
    pub server_count: i64,
    /// Project-scoped rules, plus rules deployed into the project.
    pub rule_count: i64,
}

/// Result of registering a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedProject {
    /// Id of the new or existing row.
    pub id: String,
    /// `true` when this call inserted the row, `false` when the canonical path
    /// was already registered (through the same path, a symlink, or case).
    pub inserted: bool,
}

/// Column list and count projection shared by every summary query.
const SUMMARY_SELECT: &str = "\
WITH project_items AS (\
    SELECT i.project_id AS project_id, i.id AS item_id, i.kind AS kind \
      FROM items i \
     WHERE i.scope = 'project' AND i.project_id IS NOT NULL \
    UNION \
    SELECT d.project_id AS project_id, d.item_id AS item_id, i.kind AS kind \
      FROM deployments d \
      JOIN items i ON i.id = d.item_id \
     WHERE d.scope = 'project' AND d.project_id IS NOT NULL\
), \
item_counts AS (\
    SELECT project_id, \
           SUM(kind = 'skill') AS skill_count, \
           SUM(kind = 'mcp_server') AS server_count, \
           SUM(kind = 'rule') AS rule_count \
      FROM project_items \
     GROUP BY project_id\
) \
SELECT p.id AS id, \
       COALESCE(p.name, p.path) AS name, \
       p.path AS path, \
       p.source_root AS source_root, \
       p.last_used_at AS last_used_at, \
       p.hidden AS hidden, \
       COALESCE(c.skill_count, 0) AS skill_count, \
       COALESCE(c.server_count, 0) AS server_count, \
       COALESCE(c.rule_count, 0) AS rule_count \
  FROM projects p \
  LEFT JOIN item_counts c ON c.project_id = p.id";

/// Every registered project, hidden ones included (with `hidden = true`).
///
/// Ordered by `last_used_at` descending (never-used projects last) and then by
/// display name. One statement, no per-row queries.
pub fn list_projects(connection: &Connection) -> Result<Vec<ProjectSummary>, DbError> {
    let sql = format!("{SUMMARY_SELECT} ORDER BY p.last_used_at DESC, p.name ASC");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], summary_from_row)?;
    let mut projects = Vec::new();
    for row in rows {
        projects.push(row?);
    }
    Ok(projects)
}

/// One project by id, with counts.
pub fn find_project_by_id(
    connection: &Connection,
    id: &str,
) -> Result<Option<ProjectSummary>, DbError> {
    let sql = format!("{SUMMARY_SELECT} WHERE p.id = ?1");
    connection
        .query_row(&sql, [id], summary_from_row)
        .optional()
        .map_err(DbError::from)
}

/// One project by path, with counts.
///
/// The path is canonicalised when it still exists, so a symlinked or
/// case-different spelling finds the same row. A path that no longer exists is
/// looked up as given: missing projects stay listed until the user removes
/// them.
pub fn find_project_by_path(
    connection: &Connection,
    path: &Path,
) -> Result<Option<ProjectSummary>, DbError> {
    let sql = format!("{SUMMARY_SELECT} WHERE p.path = ?1");
    let stored = lookup_path(path);
    connection
        .query_row(&sql, [stored.to_string_lossy().as_ref()], summary_from_row)
        .optional()
        .map_err(DbError::from)
}

/// Register a folder, or return the existing row for the same canonical path.
///
/// The path must exist: it is canonicalised before it is written. Re-adding an
/// existing project updates its display name (when one is given), its source
/// root (when one is given), both timestamps, and un-hides it. `source_root`
/// records the discovery root the folder was found under.
pub fn add_project(
    connection: &Connection,
    path: &Path,
    name: Option<&str>,
    source_root: Option<&Path>,
) -> Result<AddedProject, DbError> {
    let canonical = std::fs::canonicalize(path).map_err(|source| DbError::PathUnresolved {
        path: path.to_path_buf(),
        source,
    })?;
    let id = format!("prj_{}", Uuid::now_v7().hyphenated());
    let fallback_name = canonical.file_name().map_or_else(
        || canonical.to_string_lossy(),
        |name| name.to_string_lossy(),
    );
    let stored_id: String = connection.query_row(
        "INSERT INTO projects (id, name, path, source_root, last_opened_at, last_used_at) \
         VALUES (?1, COALESCE(?2, ?3), ?4, ?5, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(path) DO UPDATE SET \
             name = COALESCE(?2, projects.name), \
             source_root = COALESCE(excluded.source_root, projects.source_root), \
             last_opened_at = excluded.last_opened_at, \
             last_used_at = excluded.last_used_at, \
             hidden = 0 \
         RETURNING id",
        params![
            id,
            name,
            fallback_name.as_ref(),
            canonical.to_string_lossy().as_ref(),
            source_root.map(|root| root.to_string_lossy().into_owned()),
        ],
        |row| row.get(0),
    )?;
    Ok(AddedProject {
        inserted: stored_id == id,
        id: stored_id,
    })
}

/// Record that `project_id` became the active project.
///
/// The selection is stored under the `last_project_id` settings key (CONTRACT
/// decision 18), so this requires the `settings` table from migration v9. A
/// call that names the already-active project changes nothing and returns
/// `false`; a real selection change updates both project timestamps and the
/// setting in one transaction and returns `true`. An unknown project id changes
/// nothing and returns `false`.
pub fn touch_project(connection: &Connection, project_id: &str) -> Result<bool, DbError> {
    let selected = serde_json::to_string(project_id)?;
    let stored: Option<String> = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'last_project_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if stored.as_deref() == Some(selected.as_str()) {
        return Ok(false);
    }

    let transaction = connection.unchecked_transaction()?;
    let updated = transaction.execute(
        "UPDATE projects \
            SET last_opened_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?1",
        [project_id],
    )?;
    if updated == 0 {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO settings (key, value_json, updated_at) \
         VALUES ('last_project_id', ?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(key) DO UPDATE SET \
             value_json = excluded.value_json, \
             updated_at = excluded.updated_at",
        [&selected],
    )?;
    transaction.commit()?;
    Ok(true)
}

/// Hide a project from the project picker without deleting anything.
///
/// Deployments, operations and backup rows are untouched. Returns `true` when a
/// row matched.
pub fn hide_project(connection: &Connection, project_id: &str) -> Result<bool, DbError> {
    let updated =
        connection.execute("UPDATE projects SET hidden = 1 WHERE id = ?1", [project_id])?;
    Ok(updated == 1)
}

/// Map a summary row; every query above selects the same columns in this order.
fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<ProjectSummary> {
    Ok(ProjectSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        source_root: row.get(3)?,
        last_used_at: row.get(4)?,
        hidden: row.get::<_, i64>(5)? != 0,
        skill_count: row.get(6)?,
        server_count: row.get(7)?,
        rule_count: row.get(8)?,
    })
}

/// Canonicalise when the folder exists so symlinked spellings match the stored
/// row; otherwise keep the given path for the comparison.
fn lookup_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
