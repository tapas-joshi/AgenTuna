//! Shared helpers for at-db integration tests.
//!
//! Test binaries that include this module use different subsets, so unused
//! helpers are expected in any single binary.
#![allow(dead_code)]

use at_db::migrations::{MIGRATION_V1_SQL, MIGRATION_V2_SQL, MIGRATION_V7_SQL, MIGRATION_V9_SQL};
use at_db::{Database, Migration, Migrations};
use rusqlite::Connection;

/// Open an in-memory database with the requested shipped versions.
///
/// Versions may skip numbers: migrations v3..v6 and v8 belong to other stories
/// and are not needed by these schema tests.
pub fn open_with(versions: &[u32]) -> Database {
    let migrations: Vec<Migration> = versions
        .iter()
        .map(|&version| Migration::new(version, sql_for(version)))
        .collect();
    let set = Migrations::sparse(&migrations).expect("test migration set is valid");
    Database::open_in_memory_with(set).expect("in-memory database opens")
}

/// The embedded SQL for a migration version used by these tests.
pub fn sql_for(version: u32) -> &'static str {
    match version {
        1 => MIGRATION_V1_SQL,
        2 => MIGRATION_V2_SQL,
        7 => MIGRATION_V7_SQL,
        9 => MIGRATION_V9_SQL,
        other => panic!("no test migration v{other}"),
    }
}

/// Column names of a table in declaration order.
pub fn columns(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("table_info prepares");
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("table_info runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("column names are text")
}

/// Names of the explicit (non-auto) indexes on a table.
pub fn index_names(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA index_list({table})"))
        .expect("index_list prepares");
    statement
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let origin: String = row.get(3)?;
            Ok((name, origin))
        })
        .expect("index_list runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("index rows are text")
        .into_iter()
        .filter(|(_, origin)| origin == "c")
        .map(|(name, _)| name)
        .collect()
}

/// Column names of an index, in index order.
pub fn index_columns(connection: &Connection, index: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA index_info({index})"))
        .expect("index_info prepares");
    statement
        .query_map([], |row| row.get::<_, String>(2))
        .expect("index_info runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("index columns are text")
}

/// Triggers attached to a table.
pub fn triggers(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1")
        .expect("trigger query prepares");
    statement
        .query_map([table], |row| row.get::<_, String>(0))
        .expect("trigger query runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("trigger names are text")
}
