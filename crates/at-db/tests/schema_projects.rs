//! TF-011: schema for projects, agents and agent paths.

mod common;

use at_db::migrations::{MIGRATION_V1_SQL, MIGRATION_V2_SQL};
use at_db::{Database, DbError, Migration, Migrations};
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
fn v1_to_v2_upgrade_applies_in_order_and_records_version_two() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("index.sqlite");

    let v1 = Migrations::new(&[Migration::new(1, MIGRATION_V1_SQL)]).expect("v1 is valid");
    let database = Database::open_file_with(&path, v1).expect("v1 database opens");
    assert_eq!(database.schema_version().expect("version"), 1);
    drop(database);

    let v1_v2 = Migrations::new(&[
        Migration::new(1, MIGRATION_V1_SQL),
        Migration::new(2, MIGRATION_V2_SQL),
    ])
    .expect("v1+v2 is valid");
    let database = Database::open_file_with(&path, v1_v2).expect("database upgrades to v2");

    assert_eq!(database.schema_version().expect("version"), 2);
    let versions: Vec<u32> = database
        .applied_migrations()
        .expect("read migrations")
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(versions, vec![1, 2], "v2 applies after v1");

    let mut statement = database
        .connection()
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("inspect tables");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("query tables")
        .collect::<Result<_, _>>()
        .expect("table names");
    for expected in ["agent_paths", "agents", "projects", "schema_version"] {
        assert!(
            tables.iter().any(|table| table == expected),
            "v2 created {expected}: {tables:?}"
        );
    }
}

#[test]
fn project_path_upsert_is_case_insensitive_and_updates_opened_at() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();

    const UPSERT: &str = "INSERT INTO projects (id, name, path, last_opened_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(path) DO UPDATE SET \
             name = excluded.name, last_opened_at = excluded.last_opened_at";

    connection
        .execute(
            UPSERT,
            params![
                "p1",
                "App",
                "/Users/me/Projects/App",
                "2026-01-01T00:00:00.000Z"
            ],
        )
        .expect("first insert");
    connection
        .execute(
            UPSERT,
            params![
                "p2",
                "app",
                "/Users/me/Projects/app",
                "2026-02-02T00:00:00.000Z"
            ],
        )
        .expect("case-different path upserts the same row");

    let count: u32 = connection
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .expect("count projects");
    assert_eq!(count, 1, "one row for one folder");
    let (id, last_opened_at): (String, String) = connection
        .query_row("SELECT id, last_opened_at FROM projects", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read row");
    assert_eq!(id, "p1", "the original id is kept");
    assert_eq!(
        last_opened_at, "2026-02-02T00:00:00.000Z",
        "re-upserting updates last_opened_at"
    );
}

#[test]
fn deleting_an_agent_cascades_to_its_paths() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();
    connection
        .execute("INSERT INTO agents (id) VALUES ('claude')", [])
        .expect("insert agent");
    connection
        .execute(
            "INSERT INTO agent_paths (agent_id, scope, kind, path) \
             VALUES ('claude', 'user', 'skills_dir', '/Users/me/.claude/skills')",
            [],
        )
        .expect("insert path");

    connection
        .execute("DELETE FROM agents WHERE id = 'claude'", [])
        .expect("delete agent");
    let paths: u32 = connection
        .query_row("SELECT COUNT(*) FROM agent_paths", [], |row| row.get(0))
        .expect("count paths");
    assert_eq!(paths, 0, "agent_paths cascade");
}

#[test]
fn detected_claude_install_keeps_rows_when_paths_disappear() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO agents (id, display_name, kind, installed) \
             VALUES ('claude_code', 'Claude Code', 'claude_code', 1)",
            [],
        )
        .expect("insert detected agent");
    let detected = [
        ("user", "skills_dir", "/Users/me/.claude/skills", "dir"),
        ("user", "mcp_file", "/Users/me/.claude.json", "json"),
        (
            "user",
            "rules_file",
            "/Users/me/.claude/CLAUDE.md",
            "markdown",
        ),
        (
            "user",
            "settings_file",
            "/Users/me/.claude/settings.json",
            "json",
        ),
    ];
    for (scope, kind, path, format) in detected {
        connection
            .execute(
                "INSERT INTO agent_paths (agent_id, scope, kind, path, format, is_primary, \"exists\") \
                 VALUES ('claude_code', ?1, ?2, ?3, ?4, 1, 1)",
                params![scope, kind, path, format],
            )
            .expect("insert detected path");
    }

    let row: (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), SUM(verified), SUM(is_override) FROM agent_paths",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read defaults");
    assert_eq!(row, (4, 0, 0), "verified and is_override default to 0");
    let managed: i64 = connection
        .query_row(
            "SELECT managed FROM agents WHERE id = 'claude_code'",
            [],
            |row| row.get(0),
        )
        .expect("managed flag");
    assert_eq!(managed, 1, "detected agents are managed by default");

    connection
        .execute(
            "UPDATE agent_paths SET \"exists\" = 0 WHERE path = '/Users/me/.claude/skills'",
            [],
        )
        .expect("mark path missing");
    let kept: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM agent_paths WHERE path = '/Users/me/.claude/skills'",
            [],
            |row| row.get(0),
        )
        .expect("count kept path");
    assert_eq!(kept, 1, "a missing path keeps its row");
}

#[test]
fn agent_path_foreign_key_and_unique_matrix_are_enforced() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();

    let error = connection
        .execute(
            "INSERT INTO agent_paths (agent_id, scope, kind, path) \
             VALUES ('missing_agent', 'user', 'skills_dir', '/x')",
            [],
        )
        .expect_err("unknown agent fails");
    assert_constraint_naming(constraint(error), "foreign key");

    connection
        .execute("INSERT INTO agents (id) VALUES ('claude_code')", [])
        .expect("insert agent");
    let insert = "INSERT INTO agent_paths (agent_id, scope, kind, path) \
                  VALUES ('claude_code', 'user', 'skills_dir', '/x')";
    connection.execute(insert, []).expect("first path insert");
    let error = connection
        .execute(insert, [])
        .expect_err("duplicate matrix cell fails");
    assert_constraint_naming(constraint(error), "unique");
}

#[test]
fn defaults_and_flag_round_trips_survive_managed_changes() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();

    connection
        .execute("INSERT INTO agents (id) VALUES ('codex')", [])
        .expect("minimal agent insert uses defaults");
    let (installed, managed): (i64, i64) = connection
        .query_row("SELECT installed, managed FROM agents", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read agent defaults");
    assert_eq!((installed, managed), (0, 1));

    connection
        .execute("INSERT INTO agent_paths (agent_id) VALUES ('codex')", [])
        .expect("minimal path insert uses defaults");
    let (verified, is_override): (i64, i64) = connection
        .query_row("SELECT verified, is_override FROM agent_paths", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read path defaults");
    assert_eq!((verified, is_override), (0, 0));

    connection
        .execute("UPDATE agents SET managed = 0 WHERE id = 'codex'", [])
        .expect("clear managed");
    connection
        .execute(
            "UPDATE agent_paths SET verified = 1, is_override = 1 WHERE agent_id = 'codex'",
            [],
        )
        .expect("set flags");
    let (agents, paths): (u32, u32) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM agents), (SELECT COUNT(*) FROM agent_paths)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("count rows");
    assert_eq!((agents, paths), (1, 1), "managed=0 keeps the rows");
    let (verified, is_override): (i64, i64) = connection
        .query_row("SELECT verified, is_override FROM agent_paths", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read flags");
    assert_eq!((verified, is_override), (1, 1), "flags round-trip");
}

#[test]
fn project_defaults_match_the_schema() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO projects (id, path) VALUES ('p1', '/tmp/project')",
            [],
        )
        .expect("minimal project insert");
    let (hidden, is_global, created_at, last_used_at, last_opened_at, name): (
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT hidden, is_global, created_at, last_used_at, last_opened_at, name FROM projects",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read project defaults");
    assert_eq!((hidden, is_global), (0, 0));
    assert!(created_at.ends_with('Z'), "created_at defaults to UTC now");
    assert!(last_used_at.is_none() && last_opened_at.is_none());
    assert!(name.is_none());
}

#[test]
fn registry_indexes_are_created() {
    let database = open_with(&[1, 2]);
    let connection = database.connection();

    let project_indexes = index_names(connection, "projects");
    assert!(project_indexes.contains(&"idx_projects_path".to_owned()));
    assert!(project_indexes.contains(&"idx_projects_last_used_at".to_owned()));
    assert_eq!(index_columns(connection, "idx_projects_path"), vec!["path"]);

    let unique: i64 = connection
        .query_row(
            "SELECT \"unique\" FROM pragma_index_list('projects') WHERE name = 'idx_projects_path'",
            [],
            |row| row.get(0),
        )
        .expect("read index uniqueness");
    assert_eq!(unique, 1, "the canonical path index is unique");

    assert!(index_names(connection, "agents").contains(&"idx_agents_installed".to_owned()));
    let path_indexes = index_names(connection, "agent_paths");
    for index in [
        "idx_agent_paths_agent_id",
        "idx_agent_paths_path",
        "idx_agent_paths_scope",
    ] {
        assert!(path_indexes.contains(&index.to_owned()), "missing {index}");
    }
}

#[test]
fn projects_columns_include_registry_fields() {
    let database = open_with(&[1, 2]);
    let project_columns = columns(database.connection(), "projects");
    for expected in [
        "id",
        "name",
        "path",
        "source_root",
        "config_hash",
        "last_opened_at",
        "last_used_at",
        "hidden",
        "created_at",
        "is_global",
    ] {
        assert!(
            project_columns.iter().any(|column| column == expected),
            "projects.{expected} exists: {project_columns:?}"
        );
    }
}
