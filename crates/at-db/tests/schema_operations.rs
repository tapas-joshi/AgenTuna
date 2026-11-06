//! TF-016: schema for operations, operation files and backups.

mod common;

use at_db::DbError;
use rusqlite::params;

use common::{columns, index_columns, index_names, open_with, triggers};

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
fn status_and_action_check_constraints() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();

    for (index, status) in ["running", "succeeded", "failed", "interrupted", "undone"]
        .into_iter()
        .enumerate()
    {
        connection
            .execute(
                "INSERT INTO operations (id, status) VALUES (?1, ?2)",
                params![format!("op{index}"), status],
            )
            .expect("known status is accepted");
    }
    let error = connection
        .execute(
            "INSERT INTO operations (id, status) VALUES ('bad', 'timeout')",
            [],
        )
        .expect_err("unknown status fails");
    assert_constraint_naming(constraint(error), "check");

    connection
        .execute("INSERT INTO operations (id) VALUES ('op-file')", [])
        .expect("insert operation");
    for action in ["created", "modified", "deleted", "linked", "moved"] {
        connection
            .execute(
                "INSERT INTO operation_files (operation_id, path, action) VALUES ('op-file', '/f', ?1)",
                [action],
            )
            .expect("known action is accepted");
    }
    let error = connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) VALUES ('op-file', '/f', 'renamed')",
            [],
        )
        .expect_err("unknown action fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn operation_file_foreign_key_cascades_from_operations() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();

    let error = connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) VALUES ('missing', '/f', 'created')",
            [],
        )
        .expect_err("unknown operation fails");
    assert_constraint_naming(constraint(error), "foreign key");

    connection
        .execute("INSERT INTO operations (id) VALUES ('op1')", [])
        .expect("insert operation");
    for path in ["/a", "/b"] {
        connection
            .execute(
                "INSERT INTO operation_files (operation_id, path, action) VALUES ('op1', ?1, 'modified')",
                [path],
            )
            .expect("insert file");
    }
    connection
        .execute("DELETE FROM operations WHERE id = 'op1'", [])
        .expect("delete operation");
    let files: u32 = connection
        .query_row("SELECT COUNT(*) FROM operation_files", [], |row| row.get(0))
        .expect("count files");
    assert_eq!(files, 0, "operation_files cascade from operations");
}

#[test]
fn linked_action_stores_no_before_hash() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();
    connection
        .execute("INSERT INTO operations (id) VALUES ('op1')", [])
        .expect("insert operation");
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action, before_hash, after_hash) \
             VALUES ('op1', '/skills/a', 'linked', NULL, 'sha256:abc')",
            [],
        )
        .expect("insert linked file");
    let (before, after): (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT before_hash, after_hash FROM operation_files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read hashes");
    assert!(before.is_none(), "a link has no previous content");
    assert_eq!(after.as_deref(), Some("sha256:abc"));
}

#[test]
fn apply_and_rollback_are_atomic_across_both_tables() {
    let mut database = open_with(&[1, 7]);
    let connection = database.connection_mut();

    let transaction = connection.transaction().expect("begin transaction");
    transaction
        .execute("INSERT INTO operations (id) VALUES ('op-apply')", [])
        .expect("insert operation");
    transaction
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) VALUES ('op-apply', '/a', 'created')",
            [],
        )
        .expect("insert file");
    transaction.commit().expect("commit apply");

    let transaction = connection.transaction().expect("begin transaction");
    transaction
        .execute("DELETE FROM operations WHERE id = 'op-apply'", [])
        .expect("delete operation");
    transaction.commit().expect("commit delete");
    let files: u32 = connection
        .query_row("SELECT COUNT(*) FROM operation_files", [], |row| row.get(0))
        .expect("count files");
    assert_eq!(files, 0, "operation and files disappear together");

    let transaction = connection.transaction().expect("begin transaction");
    transaction
        .execute("INSERT INTO operations (id) VALUES ('op-bad')", [])
        .expect("insert operation");
    transaction
        .execute(
            "INSERT INTO operation_files (operation_id, action) VALUES ('op-bad', 'bogus')",
            [],
        )
        .expect_err("bad action fails");
    drop(transaction);

    let operations: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE id = 'op-bad'",
            [],
            |row| row.get(0),
        )
        .expect("count operations");
    assert_eq!(operations, 0, "a failed apply rolls back both tables");
}

#[test]
fn backups_are_unique_and_looked_up_newest_first() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();
    connection
        .execute("INSERT INTO operations (id) VALUES ('op1')", [])
        .expect("insert operation");

    for (id, backup_path, created_at) in [
        ("b1", "/backups/b1", "2026-01-01T00:00:00.000Z"),
        ("b2", "/backups/b2", "2026-02-01T00:00:00.000Z"),
    ] {
        connection
            .execute(
                "INSERT INTO backups (id, operation_id, original_path, backup_path, content_hash, size, created_at) \
                 VALUES (?1, 'op1', '/skills/a/SKILL.md', ?2, 'sha256:abc', 120, ?3)",
                params![id, backup_path, created_at],
            )
            .expect("insert backup");
    }

    let newest: Vec<String> = {
        let mut statement = connection
            .prepare(
                "SELECT backup_path FROM backups \
                 WHERE original_path = '/skills/a/SKILL.md' \
                 ORDER BY created_at DESC",
            )
            .expect("prepare lookup");
        statement
            .query_map([], |row| row.get(0))
            .expect("query backups")
            .collect::<Result<_, _>>()
            .expect("backup paths")
    };
    assert_eq!(newest, vec!["/backups/b2", "/backups/b1"]);

    let (pinned, restore_count, expires_at): (i64, i64, Option<String>) = connection
        .query_row(
            "SELECT pinned, restore_count, expires_at FROM backups WHERE id = 'b1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read backup defaults");
    assert_eq!((pinned, restore_count), (0, 0));
    assert!(expires_at.is_none());

    let error = connection
        .execute(
            "INSERT INTO backups (id, backup_path) VALUES ('b3', '/backups/b1')",
            [],
        )
        .expect_err("duplicate backup path fails");
    assert_constraint_naming(constraint(error), "unique");

    connection
        .execute("DELETE FROM operations WHERE id = 'op1'", [])
        .expect("delete operation");
    let backups: u32 = connection
        .query_row("SELECT COUNT(*) FROM backups", [], |row| row.get(0))
        .expect("count backups");
    assert_eq!(backups, 2, "the backup catalogue outlives its operation");
}

#[test]
fn history_tables_have_no_mutable_columns_or_delete_triggers() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();

    for table in ["operations", "backups"] {
        let table_columns = columns(connection, table);
        assert!(
            !table_columns.iter().any(|column| column == "updated_at"),
            "{table} must not expose updated_at: {table_columns:?}"
        );
        assert!(
            triggers(connection, table).is_empty(),
            "{table} must have no triggers"
        );
    }
    assert!(
        columns(connection, "operations")
            .iter()
            .any(|column| column == "created_at"),
        "history queries read operations.created_at"
    );
}

#[test]
fn operation_and_backup_indexes_are_created() {
    let database = open_with(&[1, 7]);
    let connection = database.connection();

    let operation_indexes = index_names(connection, "operations");
    for index in [
        "idx_operations_started_at",
        "idx_operations_kind_status",
        "idx_operations_correlation_id",
    ] {
        assert!(
            operation_indexes.contains(&index.to_owned()),
            "missing {index}"
        );
    }
    assert_eq!(
        index_columns(connection, "idx_operations_kind_status"),
        vec!["kind", "status"]
    );

    let file_indexes = index_names(connection, "operation_files");
    for index in [
        "idx_operation_files_operation_id",
        "idx_operation_files_path",
    ] {
        assert!(file_indexes.contains(&index.to_owned()), "missing {index}");
    }

    let backup_indexes = index_names(connection, "backups");
    for index in [
        "idx_backups_original_path",
        "idx_backups_expires_at",
        "idx_backups_content_hash",
    ] {
        assert!(
            backup_indexes.contains(&index.to_owned()),
            "missing {index}"
        );
    }
    assert_eq!(
        index_columns(connection, "idx_backups_original_path"),
        vec!["original_path", "created_at"],
        "newest-first lookup is index-backed"
    );
}
