//! Behaviour of the migration runner and the pragmas applied on open.

use std::fs;

use at_db::migrations::{MIGRATION_V1_SQL, MIGRATION_V2_SQL, MIGRATION_V7_SQL, SHIPPED_MIGRATIONS};
use at_db::{Database, DbError, Migration, Migrations};
use rusqlite::Connection;
use rusqlite::types::FromSql;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CUSTOM_V1: &str = "CREATE TABLE IF NOT EXISTS schema_version (\
    version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL, sha256 TEXT NOT NULL);";

const CUSTOM_V1_EDITED: &str = "CREATE TABLE IF NOT EXISTS schema_version (\
    version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL, sha256 TEXT NOT NULL);\
    -- edited after release";

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn v1_only(sql: &'static str) -> Migrations {
    Migrations::new(&[Migration::new(1, sql)]).expect("v1-only migration list is valid")
}

/// Versions a shipped binary applies: the contiguous prefix of
/// [`SHIPPED_MIGRATIONS`] starting at v1. Numbered migrations land in waves, so
/// higher versions may sit in the registry before their neighbours exist.
fn contiguous_shipped_versions() -> Vec<u32> {
    let mut versions: Vec<u32> = SHIPPED_MIGRATIONS
        .iter()
        .map(|migration| migration.version())
        .collect();
    versions.sort_unstable();
    let mut prefix = Vec::new();
    let mut expected = 1_u32;
    for version in versions {
        if version == expected {
            prefix.push(version);
            expected += 1;
        }
    }
    prefix
}

fn database_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("index.sqlite")
}

fn pragma<T: FromSql>(database: &Database, name: &str) -> T {
    database
        .connection()
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .expect("pragma is readable")
}

#[test]
fn fresh_file_applies_v1_and_creates_only_schema_version() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let database =
        Database::open_file_with(&path, v1_only(MIGRATION_V1_SQL)).expect("fresh file opens");

    let applied = database
        .applied_migrations()
        .expect("migrations are readable");
    assert_eq!(applied.len(), 1, "exactly one migration runs");
    assert_eq!(applied[0].version, 1);
    assert_eq!(
        applied[0].sha256,
        sha256_hex(MIGRATION_V1_SQL.as_bytes()),
        "recorded hash covers the embedded SQL"
    );
    assert!(
        applied[0].applied_at.contains('T') && applied[0].applied_at.ends_with('Z'),
        "applied_at is RFC 3339 UTC: {}",
        applied[0].applied_at
    );
    assert_eq!(database.schema_version().expect("version"), 1);

    let mut statement = database
        .connection()
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("query sqlite_master");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("read table names")
        .collect::<Result<_, _>>()
        .expect("table names are text");
    assert_eq!(
        tables,
        vec!["schema_version".to_owned()],
        "v1 creates no domain tables"
    );

    let rows: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
        .expect("count rows");
    assert_eq!(rows, 1);
}

#[test]
fn migrate_twice_is_a_noop() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let database = Database::open_file(&path).expect("first open");
    let first = database.applied_migrations().expect("read migrations");
    drop(database);

    let database = Database::open_file(&path).expect("second open");
    let second = database.applied_migrations().expect("read migrations");

    assert_eq!(first, second, "applied_at and hashes are unchanged");
    assert_eq!(
        second
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>(),
        contiguous_shipped_versions(),
        "no migration is recorded twice"
    );
}

#[test]
fn edited_migration_is_refused_and_database_is_untouched() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let database = Database::open_file_with(&path, v1_only(CUSTOM_V1)).expect("database created");
    drop(database);
    let before = fs::read(&path).expect("read database file");

    let error = match Database::open_file_with(&path, v1_only(CUSTOM_V1_EDITED)) {
        Ok(_) => panic!("edited migration must be refused"),
        Err(error) => error,
    };

    match error {
        DbError::MigrationChecksumMismatch {
            version,
            recorded,
            expected,
        } => {
            assert_eq!(version, 1);
            assert_eq!(recorded, sha256_hex(CUSTOM_V1.as_bytes()));
            assert_eq!(expected, sha256_hex(CUSTOM_V1_EDITED.as_bytes()));
            assert_ne!(recorded, expected);
        }
        other => panic!("expected checksum mismatch, got {other:?}"),
    }

    assert_eq!(
        fs::read(&path).expect("read database file"),
        before,
        "a refused database keeps its bytes"
    );
}

#[test]
fn newer_schema_is_refused_and_database_is_untouched() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let raw = Connection::open(&path).expect("create raw database");
    raw.execute_batch(
        "CREATE TABLE schema_version (\
             version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL, sha256 TEXT NOT NULL);\
         INSERT INTO schema_version VALUES \
             (1, '2026-01-01T00:00:00.000Z', 'aaa'), \
             (99, '2026-01-01T00:00:00.000Z', 'bbb');",
    )
    .expect("seed newer schema");
    drop(raw);
    let before = fs::read(&path).expect("read database file");

    let error = match Database::open_file(&path) {
        Ok(_) => panic!("newer schema must be refused"),
        Err(error) => error,
    };

    match error {
        DbError::SchemaTooNew { found, supported } => {
            assert_eq!(found, 99);
            assert_eq!(
                supported,
                *contiguous_shipped_versions().last().expect("v1 ships")
            );
        }
        other => panic!("expected schema-too-new, got {other:?}"),
    }

    assert_eq!(
        fs::read(&path).expect("read database file"),
        before,
        "a refused database keeps its bytes"
    );
}

#[test]
fn malformed_recorded_version_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let raw = Connection::open(&path).expect("create raw database");
    raw.execute_batch(
        "CREATE TABLE schema_version (\
             version INTEGER PRIMARY KEY NOT NULL, applied_at TEXT NOT NULL, sha256 TEXT NOT NULL);\
         INSERT INTO schema_version VALUES (0, '2026-01-01T00:00:00.000Z', 'aaa');",
    )
    .expect("seed malformed schema");
    drop(raw);

    let error = match Database::open_file(&path) {
        Ok(_) => panic!("unknown recorded version must be refused"),
        Err(error) => error,
    };
    assert!(
        matches!(error, DbError::MigrationMissing { version: 0 }),
        "expected migration-missing, got {error:?}"
    );
}

#[test]
fn file_database_uses_wal_and_expected_pragmas() {
    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);

    let database = Database::open_file(&path).expect("open file database");

    assert_eq!(pragma::<String>(&database, "journal_mode"), "wal");
    assert_eq!(pragma::<i64>(&database, "foreign_keys"), 1);
    assert_eq!(pragma::<i64>(&database, "synchronous"), 1, "NORMAL");
    assert_eq!(pragma::<i64>(&database, "busy_timeout"), 5_000);
    assert_eq!(database.path(), Some(path.as_path()));
}

#[test]
fn memory_database_disables_wal_and_keeps_other_pragmas() {
    let database = Database::open_in_memory().expect("open in-memory database");

    assert_eq!(pragma::<String>(&database, "journal_mode"), "memory");
    assert_eq!(pragma::<i64>(&database, "foreign_keys"), 1);
    assert_eq!(pragma::<i64>(&database, "synchronous"), 1, "NORMAL");
    assert_eq!(pragma::<i64>(&database, "busy_timeout"), 5_000);
    assert_eq!(
        database.schema_version().expect("version"),
        *contiguous_shipped_versions().last().expect("v1 ships")
    );
    assert!(database.path().is_none());
}

#[test]
fn later_migration_applies_once_and_in_order() {
    const V2: &str =
        "CREATE TABLE demo (id INTEGER PRIMARY KEY); INSERT INTO demo (id) VALUES (1), (2);";

    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);
    let build = || {
        Migrations::new(&[Migration::new(1, MIGRATION_V1_SQL), Migration::new(2, V2)])
            .expect("v1+v2 is a valid list")
    };

    let database = Database::open_file_with(&path, build()).expect("first open");
    assert_eq!(database.schema_version().expect("version"), 2);
    let first = database.applied_migrations().expect("read migrations");
    assert_eq!(
        first.iter().map(|row| row.version).collect::<Vec<_>>(),
        vec![1, 2],
        "migrations are recorded in order"
    );
    let demo_rows: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM demo", [], |row| row.get(0))
        .expect("count demo rows");
    assert_eq!(demo_rows, 2);
    drop(database);

    let database = Database::open_file_with(&path, build()).expect("second open");
    assert_eq!(
        database.applied_migrations().expect("read migrations"),
        first
    );
    let demo_rows: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM demo", [], |row| row.get(0))
        .expect("count demo rows");
    assert_eq!(demo_rows, 2, "v2 ran exactly once");
}

#[test]
fn failing_migration_rolls_back_without_recording() {
    const BAD_V2: &str = "CREATE TABLE partial (id INTEGER);\n\
        INSERT INTO partial (id) VALUES (1);\n\
        THIS IS NOT SQL;";

    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);
    let migrations = Migrations::new(&[
        Migration::new(1, MIGRATION_V1_SQL),
        Migration::new(2, BAD_V2),
    ])
    .expect("list is structurally valid");

    let error = match Database::open_file_with(&path, migrations) {
        Ok(_) => panic!("broken migration must fail the open"),
        Err(error) => error,
    };
    assert!(matches!(error, DbError::Sqlite(_)), "got {error:?}");

    let database =
        Database::open_file_with(&path, v1_only(MIGRATION_V1_SQL)).expect("database reopens at v1");
    assert_eq!(
        database.schema_version().expect("version"),
        1,
        "v2 not recorded"
    );
    let partial_tables: u32 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'partial'",
            [],
            |row| row.get(0),
        )
        .expect("inspect sqlite_master");
    assert_eq!(partial_tables, 0, "v2 DDL rolled back");
}

#[test]
fn foreign_key_violation_surfaces_as_constraint() {
    const V2: &str = "CREATE TABLE parent (id INTEGER PRIMARY KEY);\
        CREATE TABLE child (\
            id INTEGER PRIMARY KEY,\
            parent_id INTEGER NOT NULL REFERENCES parent(id)\
        );";
    const V3: &str = "INSERT INTO child (id, parent_id) VALUES (1, 404);";

    let dir = TempDir::new().expect("temp dir");
    let path = database_path(&dir);
    let migrations = Migrations::new(&[
        Migration::new(1, MIGRATION_V1_SQL),
        Migration::new(2, V2),
        Migration::new(3, V3),
    ])
    .expect("list is structurally valid");

    let error = match Database::open_file_with(&path, migrations) {
        Ok(_) => panic!("foreign key violation must fail the open"),
        Err(error) => error,
    };
    match error {
        DbError::Constraint { message } => {
            assert!(
                message.to_lowercase().contains("foreign key"),
                "message names the constraint: {message}"
            );
        }
        other => panic!("expected constraint error, got {other:?}"),
    }

    let database = Database::open_file_with(
        &path,
        Migrations::new(&[Migration::new(1, MIGRATION_V1_SQL), Migration::new(2, V2)])
            .expect("v1+v2 is valid"),
    )
    .expect("database reopens at v2");
    assert_eq!(database.schema_version().expect("version"), 2);
    let children: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM child", [], |row| row.get(0))
        .expect("count child rows");
    assert_eq!(children, 0, "v3 insert rolled back");
}

#[test]
fn migration_lists_must_start_at_v1_without_gaps() {
    assert!(matches!(
        Migrations::new(&[]),
        Err(DbError::InvalidMigrations { .. })
    ));
    assert!(matches!(
        Migrations::new(&[Migration::new(2, "SELECT 1;")]),
        Err(DbError::InvalidMigrations { .. })
    ));
    assert!(matches!(
        Migrations::new(&[
            Migration::new(1, "SELECT 1;"),
            Migration::new(1, "SELECT 2;")
        ]),
        Err(DbError::InvalidMigrations { .. })
    ));
    assert!(matches!(
        Migrations::new(&[Migration::new(1, "   ")]),
        Err(DbError::InvalidMigrations { .. })
    ));
}

#[test]
fn shipped_migration_set_is_valid() {
    let migrations = Migrations::shipped().expect("shipped list is valid");
    assert_eq!(
        migrations
            .iter()
            .map(|migration| migration.version())
            .collect::<Vec<_>>(),
        contiguous_shipped_versions(),
        "shipped applies the longest contiguous prefix"
    );
    assert_eq!(
        migrations.get(1).map(|migration| migration.sha256()),
        Some(sha256_hex(MIGRATION_V1_SQL.as_bytes()))
    );
    assert!(migrations.get(0).is_none());
    assert_eq!(
        migrations.get(2).map(|migration| migration.sha256()),
        Some(sha256_hex(MIGRATION_V2_SQL.as_bytes()))
    );
}

#[test]
fn sparse_sets_allow_gaps_but_require_order() {
    let set = Migrations::sparse(&[
        Migration::new(1, MIGRATION_V1_SQL),
        Migration::new(7, MIGRATION_V7_SQL),
    ])
    .expect("v1+v7 is a valid sparse set");
    assert_eq!(
        set.iter()
            .map(|migration| migration.version())
            .collect::<Vec<_>>(),
        vec![1, 7]
    );
    assert!(set.get(2).is_none(), "lookup is by version, not position");
    assert!(set.get(7).is_some());

    assert!(matches!(
        Migrations::sparse(&[Migration::new(2, "SELECT 1;")]),
        Err(DbError::InvalidMigrations { .. })
    ));
    assert!(matches!(
        Migrations::sparse(&[
            Migration::new(1, "SELECT 1;"),
            Migration::new(1, "SELECT 2;")
        ]),
        Err(DbError::InvalidMigrations { .. })
    ));
    assert!(matches!(
        Migrations::sparse(&[
            Migration::new(1, "SELECT 1;"),
            Migration::new(7, MIGRATION_V7_SQL),
            Migration::new(2, "SELECT 2;")
        ]),
        Err(DbError::InvalidMigrations { .. })
    ));
}
