//! Canonical item repository mutations.

use at_model::{ItemId, Scope};
use rusqlite::{Connection, OptionalExtension, params};

use crate::DbError;

/// Updates an item's canonical scope and path while retaining its stable identity and versions.
///
/// Project and Local items require `new_project`; Global and User items require `None`.
/// The target scope's `(kind, scope, project, slug)` uniqueness is enforced by SQLite.
///
/// # Errors
/// Returns a typed validation error for incompatible project identity, `ItemNotFound` when the
/// id does not exist, `DuplicateItemIdentity` for a target-scope slug clash, or a SQLite error.
pub fn rehome_item(
    connection: &Connection,
    item_id: &ItemId,
    new_scope: Scope,
    new_slug: &str,
    new_path: &str,
    new_project: Option<&str>,
) -> Result<(), DbError> {
    validate_scope_project(new_scope, new_project)?;
    let identity = connection
        .query_row(
            "SELECT kind, slug FROM items WHERE id = ?1",
            params![item_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or(DbError::ItemNotFound { item_id: *item_id })?;
    ensure_identity_available(
        connection,
        &identity.0,
        new_scope,
        new_project,
        new_slug,
        item_id,
    )?;
    connection.execute(
        "UPDATE items SET scope = ?2, project_id = ?3, slug = ?4, source_path = ?5, \
         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
        params![
            item_id.to_string(),
            new_scope.wire_name(),
            new_project,
            new_slug,
            new_path,
        ],
    )?;
    Ok(())
}

/// Creates a new canonical identity for a copied item and its version history.
///
/// # Errors
/// Returns a scope/project validation error, `ItemNotFound`, `DuplicateItemIdentity`, or SQLite
/// failure. The new id is never derived from the source id or slug.
pub fn duplicate_item(
    connection: &Connection,
    source_id: &ItemId,
    new_id: &ItemId,
    new_slug: &str,
    new_path: &str,
    new_scope: Scope,
    new_project: Option<&str>,
) -> Result<(), DbError> {
    validate_scope_project(new_scope, new_project)?;
    let kind = connection
        .query_row(
            "SELECT kind FROM items WHERE id = ?1",
            params![source_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(DbError::ItemNotFound {
            item_id: *source_id,
        })?;
    ensure_identity_available(connection, &kind, new_scope, new_project, new_slug, new_id)?;
    let inserted = connection.execute(
        "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, \
         content_hash, state, managed, enabled_default, tags_json) \
         SELECT ?2, kind, ?3, ?4, ?5, ?5, ?6, content_hash, state, managed, \
         enabled_default, tags_json FROM items WHERE id = ?1",
        params![
            source_id.to_string(),
            new_id.to_string(),
            new_scope.wire_name(),
            new_project,
            new_slug,
            new_path,
        ],
    )?;
    if inserted != 1 {
        return Err(DbError::ItemNotFound {
            item_id: *source_id,
        });
    }
    connection.execute(
        "INSERT INTO item_versions (item_id, scope, content_hash, path, size, mtime, \
         version_label, operation_id, last_seen_at) \
         SELECT ?2, ?3, content_hash, path, size, mtime, version_label, operation_id, last_seen_at \
         FROM item_versions WHERE item_id = ?1",
        params![
            source_id.to_string(),
            new_id.to_string(),
            new_scope.wire_name()
        ],
    )?;
    Ok(())
}

fn validate_scope_project(scope: Scope, project: Option<&str>) -> Result<(), DbError> {
    if matches!(scope, Scope::Project | Scope::Local) != project.is_some() {
        return Err(DbError::InvalidValue {
            field: "project_id",
            reason: format!("scope {scope} requires a matching project id"),
        });
    }
    if project.is_some_and(str::is_empty) {
        return Err(DbError::InvalidValue {
            field: "project_id",
            reason: "must not be empty".to_owned(),
        });
    }
    Ok(())
}

fn ensure_identity_available(
    connection: &Connection,
    kind: &str,
    scope: Scope,
    project: Option<&str>,
    slug: &str,
    item_id: &ItemId,
) -> Result<(), DbError> {
    let collision = connection
        .query_row(
            "SELECT id FROM items WHERE kind = ?1 AND scope = ?2 \
             AND COALESCE(project_id, '') = COALESCE(?3, '') AND slug = ?4 AND id != ?5",
            params![kind, scope.wire_name(), project, slug, item_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if collision.is_some() {
        return Err(DbError::DuplicateItemIdentity {
            scope,
            slug: slug.to_owned(),
        });
    }
    Ok(())
}
