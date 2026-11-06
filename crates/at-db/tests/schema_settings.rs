//! TF-018: schema for settings, secret references and remote backup state.

mod common;

use at_db::DbError;
use rusqlite::params;

use common::{columns, index_columns, index_names, open_with};

fn constraint(error: rusqlite::Error) -> DbError {
    DbError::from(error)
}

fn assert_constraint_naming(error: DbError, needle: &str) {
    match error {
        DbError::Constraint { message } => assert!(
            message.to_lowercase().contains(&needle.to_lowercase()),
            "constraint message names {needle}: {message}"
        ),
        other => panic!("expected constraint error, got {other:?}"),
    }
}

#[test]
fn secret_tables_store_references_only() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();

    let expected = vec![
        "name".to_owned(),
        "keychain_service".to_owned(),
        "account".to_owned(),
        "created_at".to_owned(),
        "rotated_at".to_owned(),
        "last_read_ok_at".to_owned(),
    ];
    assert_eq!(columns(connection, "secret_refs"), expected);

    for table in ["secret_refs", "secret_ref_uses"] {
        for column in columns(connection, table) {
            let lower = column.to_lowercase();
            for forbidden in ["value", "token", "password", "note"] {
                assert!(
                    !lower.contains(forbidden),
                    "{table}.{column} could hold a secret"
                );
            }
        }
    }
}

#[test]
fn secret_ref_names_are_unique_and_uses_cascade() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();

    connection
        .execute(
            "INSERT INTO secret_refs (name, keychain_service, account) \
             VALUES ('GITHUB_TOKEN', 'agentuna', 'GITHUB_TOKEN')",
            [],
        )
        .expect("insert secret reference");
    let error = connection
        .execute("INSERT INTO secret_refs (name) VALUES ('GITHUB_TOKEN')", [])
        .expect_err("duplicate name fails");
    assert_constraint_naming(constraint(error), "unique");

    for (item, scope) in [("itm_1", "project"), ("itm_2", "project")] {
        connection
            .execute(
                "INSERT INTO secret_ref_uses (secret_name, item_id, scope) \
                 VALUES ('GITHUB_TOKEN', ?1, ?2)",
                params![item, scope],
            )
            .expect("insert use");
    }
    let error = connection
        .execute(
            "INSERT INTO secret_ref_uses (secret_name, item_id, scope) \
             VALUES ('GITHUB_TOKEN', 'itm_1', 'project')",
            [],
        )
        .expect_err("duplicate use fails");
    assert_constraint_naming(constraint(error), "unique");

    connection
        .execute("DELETE FROM secret_refs WHERE name = 'GITHUB_TOKEN'", [])
        .expect("delete reference");
    let uses: u32 = connection
        .query_row("SELECT COUNT(*) FROM secret_ref_uses", [], |row| row.get(0))
        .expect("count uses");
    assert_eq!(uses, 0, "uses cascade from the reference");
}

#[test]
fn removing_the_last_use_keeps_the_reference() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();
    connection
        .execute("INSERT INTO secret_refs (name) VALUES ('API_KEY')", [])
        .expect("insert reference");
    connection
        .execute(
            "INSERT INTO secret_ref_uses (secret_name, item_id, scope) VALUES ('API_KEY', 'itm_1', 'user')",
            [],
        )
        .expect("insert use");

    connection
        .execute(
            "DELETE FROM secret_ref_uses WHERE secret_name = 'API_KEY'",
            [],
        )
        .expect("remove last use");
    let (uses, refs): (u32, u32) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM secret_ref_uses), (SELECT COUNT(*) FROM secret_refs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("count rows");
    assert_eq!(
        (uses, refs),
        (0, 1),
        "the reference waits for the Keychain item"
    );
}

#[test]
fn secret_timestamps_update_without_storing_a_value() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();
    connection
        .execute("INSERT INTO secret_refs (name) VALUES ('TOKEN')", [])
        .expect("insert reference");

    connection
        .execute(
            "UPDATE secret_refs \
                SET rotated_at = '2026-01-01T00:00:00.000Z', \
                    last_read_ok_at = '2026-01-02T00:00:00.000Z' \
              WHERE name = 'TOKEN'",
            [],
        )
        .expect("record rotation and successful read");
    let (rotated, read_ok): (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT rotated_at, last_read_ok_at FROM secret_refs WHERE name = 'TOKEN'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read timestamps");
    assert_eq!(rotated.as_deref(), Some("2026-01-01T00:00:00.000Z"));
    assert_eq!(read_ok.as_deref(), Some("2026-01-02T00:00:00.000Z"));
    assert_eq!(
        columns(connection, "secret_refs").len(),
        6,
        "no value column"
    );
}

#[test]
fn remote_backup_state_is_a_singleton_starting_unconfigured() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();

    let (count, status): (u32, String) = connection
        .query_row(
            "SELECT COUNT(*), MAX(status) FROM remote_backup_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read singleton");
    assert_eq!(count, 1, "the migration seeds the singleton");
    assert_eq!(status, "unconfigured");

    let error = connection
        .execute("INSERT INTO remote_backup_state (id) VALUES (1)", [])
        .expect_err("a second row is rejected");
    assert!(matches!(error, rusqlite::Error::SqliteFailure(..)));
    let error = connection
        .execute("INSERT INTO remote_backup_state (id) VALUES (2)", [])
        .expect_err("CHECK(id = 1) rejects other ids");
    assert_constraint_naming(constraint(error), "check");

    connection
        .execute(
            "UPDATE remote_backup_state SET status = 'idle', repo_url = 'https://example.com/repo.git'",
            [],
        )
        .expect("configure the remote");
    let error = connection
        .execute("UPDATE remote_backup_state SET status = 'synced'", [])
        .expect_err("unknown status fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn settings_values_round_trip_byte_identically() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();
    let value = "{\n  \"health_interval_minutes\": 15,\n  \"background_health\": \"all\",\n  \"note\": \"caf\u{e9}\"\n}\n";

    connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('health', ?1)",
            [value],
        )
        .expect("insert setting");
    let stored: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'health'",
            [],
            |row| row.get(0),
        )
        .expect("read setting");
    assert_eq!(stored, value, "settings values are stored verbatim");

    let error = connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('health', '{}')",
            [],
        )
        .expect_err("duplicate key fails");
    assert_constraint_naming(constraint(error), "unique");

    let updated_at: String = connection
        .query_row(
            "SELECT updated_at FROM settings WHERE key = 'health'",
            [],
            |row| row.get(0),
        )
        .expect("read updated_at");
    assert!(updated_at.ends_with('Z'), "updated_at defaults to UTC now");

    let error = connection
        .execute("INSERT INTO settings (key) VALUES ('empty')", [])
        .expect_err("value_json is required");
    assert!(matches!(error, rusqlite::Error::SqliteFailure(..)));
}

#[test]
fn settings_and_secret_use_indexes_are_created() {
    let database = open_with(&[1, 9]);
    let connection = database.connection();

    assert!(index_names(connection, "settings").contains(&"idx_settings_updated_at".to_owned()));
    let use_indexes = index_names(connection, "secret_ref_uses");
    for index in [
        "idx_secret_ref_uses_secret_name",
        "idx_secret_ref_uses_item_id",
    ] {
        assert!(use_indexes.contains(&index.to_owned()), "missing {index}");
    }
    assert_eq!(
        index_columns(connection, "idx_secret_ref_uses_secret_name"),
        vec!["secret_name"]
    );
    assert_eq!(
        index_columns(connection, "idx_secret_ref_uses_item_id"),
        vec!["item_id"]
    );
}
