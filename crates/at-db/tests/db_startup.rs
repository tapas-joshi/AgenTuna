//! Startup integrity and pre-migration backup behaviour.

use std::fs;
use std::path::Path;

use at_db::{Database, Migration, Migrations};
use tempfile::TempDir;
const V1: &str = at_db::migrations::MIGRATION_V1_SQL;

fn one_version(sql: &'static str) -> Migrations {
    Migrations::new(&[Migration::new(1, sql)]).expect("valid migration list")
}

fn path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("agentuna.db")
}

#[test]
fn pending_migration_keeps_a_fsynced_copy_of_the_prior_schema() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = path(&dir);
    drop(Database::open_file_with(&db_path, one_version(V1)).expect("create v1"));
    let original = fs::read(&db_path).expect("read original");
    let database = Database::open_file(&db_path).expect("migrate to current schema");
    assert_eq!(
        database.schema_version().expect("current schema version"),
        9
    );
    drop(database);
    let backup_dir = dir.path().join("backups/db");
    let backups = fs::read_dir(&backup_dir)
        .expect("backup directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).expect("read backup"), original);
}

#[test]
fn failed_migration_restores_the_pre_migration_database() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = path(&dir);
    drop(Database::open_file_with(&db_path, one_version(V1)).expect("create v1"));
    let migrations = Migrations::new(&[
        Migration::new(1, V1),
        Migration::new(
            2,
            "CREATE TABLE transient(value TEXT); SELECT * FROM missing_table;",
        ),
    ])
    .expect("valid migration list");
    assert!(Database::open_file_with(&db_path, migrations).is_err());
    let reopened = Database::open_file_with(&db_path, one_version(V1)).expect("reopen restored v1");
    assert_eq!(
        reopened.schema_version().expect("original schema version"),
        1
    );
    let transient: bool = reopened
        .connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transient')",
            [],
            |row| row.get(0),
        )
        .expect("check rollback");
    assert!(!transient);
}

#[test]
fn corrupt_file_is_quarantined_and_new_database_exposes_warning_path() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = path(&dir);
    fs::write(&db_path, b"corrupt sqlite bytes").expect("write corrupt fixture");
    let database = Database::open_file(&db_path).expect("recover fresh database");
    let warning = database.startup_warning().expect("quarantine warning");
    assert!(
        warning
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("agentuna.db.corrupt-"))
    );
    assert!(warning.exists());
    assert_eq!(database.schema_version().expect("new schema version"), 9);
}

#[test]
fn clean_close_truncates_wal_and_clears_unclean_marker() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = path(&dir);
    let database = Database::open_file(&db_path).expect("create database");
    database
        .connection()
        .execute(
            "INSERT INTO agents(id,display_name) VALUES ('codex','Codex')",
            [],
        )
        .expect("write row");
    drop(database);
    let wal = Path::new(&format!("{}-wal", db_path.display())).to_path_buf();
    assert!(!wal.exists() || fs::metadata(wal).expect("wal metadata").len() == 0);
    assert!(!db_path.with_extension("unclean").exists());
}

#[test]
fn successful_migrations_retain_exactly_the_three_newest_db_backups() {
    let dir = TempDir::new().expect("temp dir");
    let db_path = path(&dir);
    let steps = [
        "CREATE TABLE stage_two(value TEXT);",
        "CREATE TABLE stage_three(value TEXT);",
        "CREATE TABLE stage_four(value TEXT);",
        "CREATE TABLE stage_five(value TEXT);",
    ];
    drop(Database::open_file_with(&db_path, one_version(V1)).expect("create v1"));
    for newest in 2..=5 {
        let migrations = (1..=newest)
            .map(|version| {
                Migration::new(
                    version,
                    if version == 1 {
                        V1
                    } else {
                        steps[(version - 2) as usize]
                    },
                )
            })
            .collect::<Vec<_>>();
        let db = Database::open_file_with(
            &db_path,
            Migrations::new(&migrations).expect("build migration sequence"),
        )
        .expect("apply next migration");
        drop(db);
    }
    let backups = fs::read_dir(dir.path().join("backups/db"))
        .expect("backup directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 3);
}
