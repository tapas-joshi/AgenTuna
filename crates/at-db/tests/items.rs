//! Integration tests for moving database items between scopes.

use at_db::Database;
use at_db::items::rehome_item;
use at_model::{ItemId, Scope};
use rusqlite::params;

fn seed_item(connection: &rusqlite::Connection, id: ItemId, slug: &str, scope: Scope) {
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path) \
             VALUES (?1, 'skill', ?2, ?3, ?3, 'skills/source')",
            params![id.to_string(), scope.wire_name(), slug],
        )
        .expect("seed item");
}

#[test]
fn rehome_updates_scope_and_path_without_replacing_identity_or_versions() {
    let database = Database::open_in_memory().expect("database");
    let connection = database.connection();
    let id = ItemId::new();
    seed_item(connection, id, "review", Scope::Project);
    connection
        .execute(
            "INSERT INTO item_versions (item_id, scope, content_hash) VALUES (?1, 'project', ?2)",
            params![id.to_string(), "a".repeat(64)],
        )
        .expect("seed version");

    rehome_item(
        connection,
        &id,
        Scope::User,
        "review",
        "skills/review",
        None,
    )
    .expect("rehome");

    let row: (String, String, String) = connection
        .query_row(
            "SELECT id, scope, source_path FROM items WHERE id = ?1",
            params![id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read item");
    assert_eq!(row.0, id.to_string());
    assert_eq!(row.1, "user");
    assert_eq!(row.2, "skills/review");
    let version_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM item_versions WHERE item_id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .expect("count versions");
    assert_eq!(version_count, 1);
}

#[test]
fn rehome_refuses_target_slug_collision() {
    let database = Database::open_in_memory().expect("database");
    let connection = database.connection();
    let moving = ItemId::new();
    let existing = ItemId::new();
    seed_item(connection, moving, "review", Scope::Project);
    seed_item(connection, existing, "review", Scope::User);

    assert!(matches!(
        rehome_item(
            connection,
            &moving,
            Scope::User,
            "review",
            "skills/review",
            None
        ),
        Err(at_db::DbError::DuplicateItemIdentity {
            scope: Scope::User,
            ..
        })
    ));
}

#[test]
fn rehome_missing_item_and_scope_project_mismatch_are_typed_errors() {
    let database = Database::open_in_memory().expect("database");
    let connection = database.connection();
    let missing = ItemId::new();
    assert!(matches!(
        rehome_item(
            connection,
            &missing,
            Scope::User,
            "review",
            "skills/review",
            None
        ),
        Err(at_db::DbError::ItemNotFound { .. })
    ));
    let id = ItemId::new();
    seed_item(connection, id, "review", Scope::Project);
    assert!(matches!(
        rehome_item(
            connection,
            &id,
            Scope::User,
            "review",
            "skills/review",
            Some("project-1")
        ),
        Err(at_db::DbError::InvalidValue {
            field: "project_id",
            ..
        })
    ));
}
