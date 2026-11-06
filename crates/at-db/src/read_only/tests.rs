//! Behaviour tests for the read-only index handle.

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::Database;

#[test]
fn an_existing_index_reads_without_changing_a_byte() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("agentuna.db");
    let version = {
        let database = Database::open_file(&path).expect("create the index");
        let version = database.schema_version().expect("schema version");
        assert!(version >= 1);
        version
    };
    let before = fs::read(&path).expect("read the index bytes");

    let read_only = ReadOnlyDatabase::open(&path).expect("open read-only");
    assert_eq!(read_only.path(), path.as_path());
    assert_eq!(read_only.schema_version().expect("schema version"), version);

    let query_only: i64 = read_only
        .connection()
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .expect("query_only pragma");
    assert_eq!(query_only, 1);

    let error = read_only
        .execute("CREATE TABLE probe (id INTEGER PRIMARY KEY)", [])
        .expect_err("writes are rejected");
    assert!(matches!(error, ReadOnlyError::ReadOnly), "{error:?}");

    let error = read_only
        .execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY);")
        .expect_err("batch writes are rejected");
    assert!(matches!(error, ReadOnlyError::ReadOnly), "{error:?}");

    assert_eq!(
        fs::read(&path).expect("read the index bytes"),
        before,
        "the read-only handle must not modify the file"
    );
}

#[test]
fn a_database_without_the_schema_table_reports_zero_and_is_not_migrated() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("plain.db");
    {
        let connection = rusqlite::Connection::open(&path).expect("create the file");
        connection
            .execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);")
            .expect("create a table");
    }
    let before = fs::read(&path).expect("read the database bytes");

    let read_only = ReadOnlyDatabase::open(&path).expect("open read-only");
    assert_eq!(read_only.schema_version().expect("schema version"), 0);

    let tables: Vec<String> = read_only
        .connection()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert!(tables.is_empty(), "read-only must never migrate");

    assert_eq!(fs::read(&path).expect("read the database bytes"), before);
}

#[test]
fn a_missing_index_is_reported_and_not_created() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("missing.db");

    let error = ReadOnlyDatabase::open(&path).expect_err("missing index");
    assert!(matches!(error, ReadOnlyError::Open { .. }), "{error:?}");
    assert!(!path.exists(), "read-only must not create the file");
}

#[test]
fn read_queries_still_work() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("agentuna.db");
    {
        let database = Database::open_file(&path).expect("create the index");
        database
            .connection()
            .execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, body TEXT);")
            .expect("create table");
        database
            .connection()
            .execute("INSERT INTO probe (body) VALUES ('hello')", [])
            .expect("insert");
    }

    let read_only = ReadOnlyDatabase::open(&path).expect("open read-only");
    let body: String = read_only
        .connection()
        .query_row("SELECT body FROM probe", [], |row| row.get(0))
        .expect("select");
    assert_eq!(body, "hello");
}
