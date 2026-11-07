//! Canonical item repository mutations.

use std::str::FromStr;

use at_model::{ItemId, ItemKind, Scope};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::DbError;

/// One indexed item offered as a preset member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCandidate {
    /// Stable item identity.
    pub id: ItemId,
    /// Item kind.
    pub kind: ItemKind,
    /// Scope holding the canonical copy.
    pub scope: Scope,
    /// Display name.
    pub name: String,
}

/// Lists indexed skills and MCP servers for one scope, ordered by kind then name.
///
/// Used by the preset editor to offer members; rules are excluded because presets apply only
/// deployable skills and MCP servers.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn preset_member_candidates(
    connection: &Connection,
    scope: Scope,
) -> Result<Vec<ItemCandidate>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, kind, name FROM items           WHERE scope = ?1 AND kind IN ('skill', 'mcp_server')           ORDER BY kind ASC, name COLLATE NOCASE ASC, id ASC",
    )?;
    let rows = statement.query_map([scope.wire_name()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut items = Vec::new();
    for row in rows {
        let (id, kind, name) = row?;
        items.push(ItemCandidate {
            id: ItemId::from_str(&id).map_err(DbError::from)?,
            kind: ItemKind::from_str(&kind).map_err(DbError::from)?,
            scope,
            name,
        });
    }
    Ok(items)
}

/// Deletes one canonical item row and its dependent version and deployment rows.
///
/// Used by undo of a Migrate or Adopt operation once the store files it created are gone and
/// no deployment still references the item.
///
/// # Errors
///
/// [`DbError`] when the delete fails.
pub fn delete_item(connection: &Connection, item_id: &ItemId) -> Result<bool, DbError> {
    let deleted = connection.execute("DELETE FROM items WHERE id = ?1", [item_id.to_string()])?;
    Ok(deleted == 1)
}

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

/// Specification for an item recorded directly from a store manifest.
#[derive(Clone, Copy, Debug)]
pub struct ManifestItemSpec<'a> {
    /// Item identifier from the manifest.
    pub id: ItemId,
    /// Kind of item (skill, rule, MCP server).
    pub kind: ItemKind,
    /// Target scope.
    pub scope: Scope,
    /// Optional owning project id.
    pub project_id: Option<&'a str>,
    /// Slug within scope/project.
    pub slug: &'a str,
    /// Display name.
    pub name: &'a str,
    /// Path relative to store or workspace root.
    pub source_path: &'a str,
    /// SHA-256 hash of canonical content.
    pub content_hash: &'a str,
}

/// Synchronizes a manifest item into the `items` repository.
///
/// If an item already exists with matching `(kind, scope, project_id, slug)` identity,
/// its `name`, `source_path`, `content_hash`, `state` ('present'), and `managed` (1)
/// fields are updated, and the existing `ItemId` is returned.
///
/// Otherwise, the item is inserted using `spec.id` (updating on id conflict) and
/// `spec.id` is returned.
///
/// # Errors
/// Returns [`DbError`] when validation or execution fails.
pub fn sync_manifest_item(
    connection: &Connection,
    spec: &ManifestItemSpec<'_>,
) -> Result<ItemId, DbError> {
    validate_scope_project(spec.scope, spec.project_id)?;
    if let Some(existing) = item_id_by_identity(
        connection,
        spec.kind,
        spec.scope,
        spec.project_id,
        spec.slug,
    )? {
        connection.execute(
            "UPDATE items SET name = ?2, source_path = ?3, content_hash = ?4, \
             state = 'present', managed = 1, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![
                existing.to_string(),
                spec.name,
                spec.source_path,
                spec.content_hash
            ],
        )?;
        return Ok(existing);
    }
    connection.execute(
        "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, \
         content_hash, state, managed) \
         VALUES (?1, ?8, ?2, ?3, ?4, ?5, ?6, ?7, 'present', 1) \
         ON CONFLICT(id) DO UPDATE SET scope = excluded.scope, \
             project_id = excluded.project_id, slug = excluded.slug, name = excluded.name, \
             source_path = excluded.source_path, content_hash = excluded.content_hash, \
             state = 'present', managed = 1, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![
            spec.id.to_string(),
            spec.scope.wire_name(),
            spec.project_id,
            spec.slug,
            spec.name,
            spec.source_path,
            spec.content_hash,
            spec.kind.wire_name(),
        ],
    )?;
    Ok(spec.id)
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

    #[test]
    fn sync_manifest_item_inserts_and_updates_by_identity() {
        use super::{ManifestItemSpec, sync_manifest_item};

        let db = Database::open_in_memory().expect("open database");
        let connection = db.connection();

        let item_id = ItemId::new();
        let spec = ManifestItemSpec {
            id: item_id,
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: "test-skill",
            name: "Test Skill",
            source_path: "skills/test-skill",
            content_hash: "hash_v1",
        };

        let returned_id = sync_manifest_item(connection, &spec).expect("insert manifest item");
        assert_eq!(returned_id, item_id);

        // Re-syncing with a different proposed ID but same identity updates the existing row
        let new_proposed_id = ItemId::new();
        let updated_spec = ManifestItemSpec {
            id: new_proposed_id,
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: "test-skill",
            name: "Updated Skill",
            source_path: "skills/test-skill-new",
            content_hash: "hash_v2",
        };

        let existing_id =
            sync_manifest_item(connection, &updated_spec).expect("update manifest item");
        assert_eq!(existing_id, item_id);

        let row: (String, String, String) = connection
            .query_row(
                "SELECT name, source_path, content_hash FROM items WHERE id = ?1",
                [item_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read updated item");
        assert_eq!(row.0, "Updated Skill");
        assert_eq!(row.1, "skills/test-skill-new");
        assert_eq!(row.2, "hash_v2");
    }
}

/// Snapshot of one `items` row captured before a delete removed it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UndoItemSnapshot {
    /// Stable item identity.
    pub id: String,
    /// Item kind wire name.
    pub kind: String,
    /// Scope wire name.
    pub scope: String,
    /// Owning project id for Project and Local rows.
    pub project_id: Option<String>,
    /// Slug within the scope.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Canonical store or rule path.
    pub source_path: Option<String>,
    /// Link target for a linked rule.
    pub link_target: Option<String>,
    /// Canonical content hash.
    pub content_hash: Option<String>,
    /// Projection state.
    pub state: Option<String>,
    /// Whether AgenTuna owns the item.
    pub managed: i64,
    /// Source row reference.
    pub source_id: Option<String>,
    /// Default enabled flag.
    pub enabled_default: Option<i64>,
    /// Adapter tags JSON.
    pub tags_json: Option<String>,
}

/// Reads one item row for undo metadata.
///
/// # Errors
/// Returns the database failure when the row cannot be read.
pub fn undo_snapshot(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Option<UndoItemSnapshot>, DbError> {
    Ok(connection
        .query_row(
            "SELECT id, kind, scope, project_id, slug, name, source_path, link_target, \
             content_hash, state, managed, source_id, enabled_default, tags_json \
             FROM items WHERE id = ?1",
            [item_id.to_string()],
            |row| {
                Ok(UndoItemSnapshot {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    scope: row.get(2)?,
                    project_id: row.get(3)?,
                    slug: row.get(4)?,
                    name: row.get(5)?,
                    source_path: row.get(6)?,
                    link_target: row.get(7)?,
                    content_hash: row.get(8)?,
                    state: row.get(9)?,
                    managed: row.get(10)?,
                    source_id: row.get(11)?,
                    enabled_default: row.get(12)?,
                    tags_json: row.get(13)?,
                })
            },
        )
        .optional()?)
}

/// Restores one captured item row, replacing any current row with the same id.
///
/// The caller restores the owning deployment rows separately.
///
/// # Errors
/// Returns the database failure when the row cannot be written.
pub fn restore_undo_snapshot(
    connection: &Connection,
    snapshot: &UndoItemSnapshot,
) -> Result<(), DbError> {
    let source_exists = match snapshot.source_id.as_deref() {
        Some(source_id) => connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE id = ?1)",
            [source_id],
            |row| row.get::<_, bool>(0),
        )?,
        None => false,
    };
    let source_id = snapshot.source_id.as_deref().filter(|_| source_exists);
    connection.execute("DELETE FROM items WHERE id = ?1", [snapshot.id.as_str()])?;
    connection.execute(
        "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, \
         link_target, content_hash, state, managed, source_id, enabled_default, tags_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            snapshot.id,
            snapshot.kind,
            snapshot.scope,
            snapshot.project_id,
            snapshot.slug,
            snapshot.name,
            snapshot.source_path,
            snapshot.link_target,
            snapshot.content_hash,
            snapshot.state,
            snapshot.managed,
            source_id,
            snapshot.enabled_default,
            snapshot.tags_json,
        ],
    )?;
    Ok(())
}

/// Lists the ids of managed skills and MCP servers in one scope.
///
/// # Errors
/// Returns the database failure when the query fails.
pub fn managed_ids_for_scope(
    connection: &Connection,
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Vec<String>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id FROM items WHERE kind IN ('skill', 'mcp_server') AND scope = ?1 \
         AND COALESCE(project_id, '') = COALESCE(?2, '')",
    )?;
    let rows = statement.query_map(params![scope.wire_name(), project_id], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Reads one item's display name.
///
/// # Errors
/// Returns the database failure when the row cannot be read.
pub fn name_for_id(connection: &Connection, id: &str) -> Result<Option<String>, DbError> {
    Ok(connection
        .query_row("SELECT name FROM items WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// Records a restored rule file's hash on its item row.
///
/// # Errors
/// Returns the database failure when the update fails.
pub fn update_content_hash_for_source(
    connection: &Connection,
    source_path: &str,
    hash: &str,
) -> Result<usize, DbError> {
    Ok(connection.execute(
        "UPDATE items SET content_hash = ?2, state = 'present' \
         WHERE kind = 'rule' AND source_path = ?1",
        params![source_path, hash],
    )?)
}

/// Records a saved rule file's hash on its item row by identity.
///
/// # Errors
/// Returns the database failure when the update fails.
pub fn update_content_hash_for_id(
    connection: &Connection,
    item_id: &ItemId,
    hash: &str,
) -> Result<usize, DbError> {
    Ok(connection.execute(
        "UPDATE items SET content_hash = ?2, state = 'present' WHERE id = ?1",
        params![item_id.to_string(), hash],
    )?)
}

/// Removes a rule row whose file an undo deleted.
///
/// # Errors
/// Returns the database failure when the delete fails.
pub fn delete_rules_for_source(
    connection: &Connection,
    source_path: &str,
) -> Result<usize, DbError> {
    Ok(connection.execute(
        "DELETE FROM items WHERE kind = 'rule' AND source_path = ?1",
        [source_path],
    )?)
}
