//! Canonical item repository mutations.

use at_model::{ItemId, ItemKind, Scope};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::DbError;

/// Canonical item fields and proposed id used when a native item is adopted.
///
/// Project and Local items require `project_id`; Global and User items require `None`.
#[derive(Clone, Copy, Debug)]
pub struct AdoptedItemSpec<'a> {
    /// ID to store if the canonical identity does not already exist.
    pub proposed_id: ItemId,
    /// What the item is.
    pub kind: ItemKind,
    /// Canonical store scope.
    pub scope: Scope,
    /// Owning project for Project and Local items.
    pub project_id: Option<&'a str>,
    /// URL-safe canonical identity within `(kind, scope, project)`.
    pub slug: &'a str,
    /// Display name.
    pub name: &'a str,
    /// Path of the canonical content in the store.
    pub source_path: &'a str,
    /// SHA-256 of the canonical content, when available.
    pub content_hash: Option<&'a str>,
}

/// Finds an item by its canonical `(kind, scope, project, slug)` identity.
///
/// # Errors
/// Returns a scope/project validation error, a SQLite error, or an error when a stored id is
/// malformed.
pub fn item_id_by_identity(
    connection: &Connection,
    kind: ItemKind,
    scope: Scope,
    project_id: Option<&str>,
    slug: &str,
) -> Result<Option<ItemId>, DbError> {
    validate_scope_project(scope, project_id)?;
    let stored_id: Option<String> = connection
        .query_row(
            "SELECT id FROM items WHERE kind = ?1 AND scope = ?2 \
             AND COALESCE(project_id, '') = COALESCE(?3, '') AND slug = ?4",
            params![kind.wire_name(), scope.wire_name(), project_id, slug],
            |row| row.get(0),
        )
        .optional()?;
    stored_id
        .map(|id| id.parse::<ItemId>().map_err(DbError::from))
        .transpose()
}

/// Creates or refreshes the canonical identity for an adopted item.
///
/// The `spec.proposed_id` is used only when the `(kind, scope, project, slug)` identity is absent.
/// Existing identities keep their id and version history. The item is marked `present` and
/// managed. This uses the caller's transaction so the item upsert and native-path adoption can
/// commit or roll back together.
///
/// # Errors
/// Returns a scope/project validation error, a database constraint or SQLite error, or an error
/// when an existing stored id is malformed.
pub fn upsert_adopted_item(
    transaction: &Transaction<'_>,
    spec: &AdoptedItemSpec<'_>,
) -> Result<ItemId, DbError> {
    validate_scope_project(spec.scope, spec.project_id)?;
    let stored_id: String = transaction.query_row(
        "INSERT INTO items \
             (id, kind, scope, project_id, slug, name, source_path, content_hash, state, managed, \
              last_scanned_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'present', 1, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(kind, scope, COALESCE(project_id, ''), slug) DO UPDATE SET \
             project_id = excluded.project_id, \
             name = excluded.name, \
             source_path = excluded.source_path, \
             content_hash = excluded.content_hash, \
             state = 'present', \
             managed = 1, \
             last_scanned_at = excluded.last_scanned_at, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         RETURNING id",
        params![
            spec.proposed_id.to_string(),
            spec.kind.wire_name(),
            spec.scope.wire_name(),
            spec.project_id,
            spec.slug,
            spec.name,
            spec.source_path,
            spec.content_hash,
        ],
        |row| row.get(0),
    )?;
    Ok(stored_id.parse()?)
}

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

#[cfg(test)]
mod tests {
    use super::{AdoptedItemSpec, item_id_by_identity, upsert_adopted_item};
    use crate::{
        Database,
        native::{NativeAdoptionPath, adopt_paths},
    };
    use at_model::{AgentId, ItemId, ItemKind, Scope};

    #[test]
    fn adopted_item_and_paths_share_one_transaction() {
        let mut database = Database::open_in_memory().expect("open database");
        let transaction = database
            .connection_mut()
            .transaction()
            .expect("begin transaction");
        let proposed_id = ItemId::new();
        assert_eq!(
            item_id_by_identity(&transaction, ItemKind::Skill, Scope::User, None, "review",)
                .expect("look up absent item identity"),
            None
        );
        let spec = AdoptedItemSpec {
            proposed_id,
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: "review",
            name: "Review",
            source_path: "/store/skills/review",
            content_hash: Some("hash-1"),
        };
        let item_id = upsert_adopted_item(&transaction, &spec).expect("upsert item");
        assert_eq!(
            item_id_by_identity(&transaction, ItemKind::Skill, Scope::User, None, "review",)
                .expect("look up item identity"),
            Some(item_id)
        );
        let refreshed_id = upsert_adopted_item(
            &transaction,
            &AdoptedItemSpec {
                name: "Review checks",
                source_path: "/store/skills/review-checks",
                content_hash: Some("hash-2"),
                ..spec
            },
        )
        .expect("refresh existing item");
        assert_eq!(refreshed_id, item_id);

        let codex = AgentId::Codex;
        let omp = AgentId::Omp;
        let paths = [
            NativeAdoptionPath {
                agent_id: &codex,
                scope: Scope::User,
                kind: ItemKind::Skill,
                name: "Review",
                path: "/home/codex/skills/review",
                content_hash: Some("native-hash-1"),
            },
            NativeAdoptionPath {
                agent_id: &omp,
                scope: Scope::User,
                kind: ItemKind::Skill,
                name: "Review",
                path: "/home/omp/skills/review",
                content_hash: Some("native-hash-2"),
            },
        ];
        assert_eq!(
            adopt_paths(&transaction, &paths, &item_id).expect("adopt paths"),
            2
        );

        let native_rows = {
            let mut statement = transaction
                .prepare("SELECT agent_id, adopted_item_id, status FROM native_items ORDER BY path")
                .expect("prepare native rows");
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .expect("query native rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect native rows")
        };
        assert_eq!(
            native_rows,
            vec![
                (
                    "codex".to_owned(),
                    item_id.to_string(),
                    "adopted".to_owned()
                ),
                ("omp".to_owned(), item_id.to_string(), "adopted".to_owned()),
            ]
        );
        let item_count: i64 = transaction
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .expect("count items");
        assert_eq!(item_count, 1);
        transaction.commit().expect("commit adoption");
    }
}
