//! TU-011: the project registry repository over `projects`.

mod common;

use std::fs;
use std::path::Path;

use at_db::projects::{
    AddedProject, ProjectSummary, add_project, find_project_by_id, find_project_by_path,
    hide_project, list_projects, touch_project,
};
use at_db::{Database, DbError, Migration, Migrations};
use rusqlite::params;
use tempfile::TempDir;

/// A temp file database with the migrations this story needs, plus stand-ins
/// for the `items` and `deployments` tables owned by TF-012/TF-013 (v3/v4,
/// later waves). The column names below match those stories' migrations.
struct Fixture {
    database: Database,
    _dir: TempDir,
}

const FUTURE_TABLES: &str = "\
CREATE TABLE items (
    id TEXT PRIMARY KEY,
    kind TEXT,
    scope TEXT,
    project_id TEXT,
    slug TEXT
);
CREATE TABLE deployments (
    id INTEGER PRIMARY KEY,
    item_id TEXT,
    agent_id TEXT,
    scope TEXT,
    project_id TEXT,
    state TEXT
);";

fn fixture(versions: &[u32]) -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("index.sqlite");
    let migrations: Vec<Migration> = versions
        .iter()
        .map(|&version| Migration::new(version, common::sql_for(version)))
        .collect();
    let set = Migrations::sparse(&migrations).expect("test migrations are valid");
    let database = Database::open_file_with(&path, set).expect("database opens");
    database
        .connection()
        .execute_batch(FUTURE_TABLES)
        .expect("future tables are created");
    Fixture {
        database,
        _dir: dir,
    }
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonical path")
        .to_string_lossy()
        .into_owned()
}

fn counts(project: &ProjectSummary) -> (i64, i64, i64) {
    (
        project.skill_count,
        project.server_count,
        project.rule_count,
    )
}

#[test]
fn empty_registry_lists_nothing() {
    let fixture = fixture(&[1, 2, 9]);
    let projects = list_projects(fixture.database.connection()).expect("list projects");
    assert!(projects.is_empty());
}

#[cfg(unix)]
#[test]
fn adding_the_same_folder_through_a_symlink_resolves_to_one_row() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let root = fixture._dir.path().join("workspace");
    fs::create_dir(&root).expect("create project folder");
    let link = fixture._dir.path().join("workspace-link");
    std::os::unix::fs::symlink(&root, &link).expect("create symlink");

    let first = add_project(connection, &root, Some("Workspace"), None).expect("add project");
    assert!(first.inserted);
    let second = add_project(connection, &link, None, None).expect("add through symlink");
    assert!(!second.inserted, "the symlink resolves to the existing row");
    assert_eq!(first.id, second.id);

    let projects = list_projects(connection).expect("list projects");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, canonical(&root));
    assert_eq!(
        projects[0].name, "Workspace",
        "a re-add keeps the display name"
    );

    let by_link = find_project_by_path(connection, &link)
        .expect("lookup by path")
        .expect("found through symlink");
    let by_id = find_project_by_id(connection, &first.id)
        .expect("lookup by id")
        .expect("found by id");
    assert_eq!(by_link.id, by_id.id);
    assert!(by_link.last_used_at.is_some());
}

#[test]
fn the_canonical_path_index_rejects_case_variants() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let root = fixture._dir.path().join("Case Project");
    fs::create_dir(&root).expect("create project folder");
    add_project(connection, &root, None, None).expect("add project");

    let variant = canonical(&root).to_uppercase();
    let error = connection
        .execute(
            "INSERT INTO projects (id, path) VALUES ('prj_other', ?1)",
            [variant],
        )
        .expect_err("a case variant is the same canonical path");
    match DbError::from(error) {
        DbError::Constraint { message } => {
            assert!(message.to_lowercase().contains("unique"), "{message}");
        }
        other => panic!("expected unique constraint, got {other:?}"),
    }
}

#[test]
fn list_projects_returns_counts_in_one_query() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let root = fixture._dir.path().join("with-items");
    fs::create_dir(&root).expect("create project folder");
    let empty = fixture._dir.path().join("empty");
    fs::create_dir(&empty).expect("create empty folder");
    let project = add_project(connection, &root, None, None).expect("add project");
    let empty_project = add_project(connection, &empty, None, None).expect("add empty project");

    let items = [
        ("itm_1", "skill", "project", Some(project.id.as_str())),
        ("itm_2", "skill", "project", Some(project.id.as_str())),
        ("itm_3", "mcp_server", "project", Some(project.id.as_str())),
        ("itm_4", "rule", "project", Some(project.id.as_str())),
        ("itm_5", "skill", "user", None),
        ("itm_6", "mcp_server", "user", None),
        ("itm_7", "rule", "project", Some("prj_elsewhere")),
    ];
    for (index, (id, kind, scope, project_id)) in items.into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO items (id, kind, scope, project_id, slug) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, kind, scope, project_id, format!("slug{index}")],
            )
            .expect("insert item");
    }
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state) \
             VALUES ('itm_5', 'claude_code', 'project', ?1, 'linked')",
            [project.id.as_str()],
        )
        .expect("deploy a user skill into the project");

    let projects = list_projects(connection).expect("list projects");
    let with_items = projects
        .iter()
        .find(|summary| summary.id == project.id)
        .expect("project is listed");
    assert_eq!(counts(with_items), (3, 1, 1));
    let without_items = projects
        .iter()
        .find(|summary| summary.id == empty_project.id)
        .expect("empty project is listed");
    assert_eq!(counts(without_items), (0, 0, 0));
}

#[test]
fn list_projects_orders_by_last_used_then_name() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let mut added = Vec::new();
    for name in ["Bravo", "Zulu", "Alpha", "Never"] {
        let folder = fixture._dir.path().join(format!("folder-{name}"));
        fs::create_dir(&folder).expect("create folder");
        added.push(add_project(connection, &folder, Some(name), None).expect("add project"));
    }

    connection
        .execute(
            "UPDATE projects SET last_used_at = '2026-01-01T00:00:00.000Z', \
             last_opened_at = '2026-01-01T00:00:00.000Z' WHERE id = ?1",
            [added[0].id.as_str()],
        )
        .expect("age Bravo");
    for index in [1, 2] {
        connection
            .execute(
                "UPDATE projects SET last_used_at = '2026-03-01T00:00:00.000Z', \
                 last_opened_at = '2026-03-01T00:00:00.000Z' WHERE id = ?1",
                [added[index].id.as_str()],
            )
            .expect("set tie timestamp");
    }
    connection
        .execute(
            "UPDATE projects SET last_used_at = NULL WHERE id = ?1",
            [added[3].id.as_str()],
        )
        .expect("clear never-used timestamp");

    let names: Vec<String> = list_projects(connection)
        .expect("list projects")
        .into_iter()
        .map(|project| project.name)
        .collect();
    assert_eq!(names, vec!["Alpha", "Zulu", "Bravo", "Never"]);
}

#[test]
fn touch_project_updates_only_on_selection_change() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let first = fixture._dir.path().join("first");
    let second = fixture._dir.path().join("second");
    fs::create_dir(&first).expect("create folder");
    fs::create_dir(&second).expect("create folder");
    let first = add_project(connection, &first, None, None).expect("add first");
    let second = add_project(connection, &second, None, None).expect("add second");

    connection
        .execute(
            "UPDATE projects SET last_opened_at = '2020-01-01T00:00:00.000Z', \
             last_used_at = '2020-01-01T00:00:00.000Z'",
            [],
        )
        .expect("reset timestamps");

    assert!(touch_project(connection, &first.id).expect("touch first"));
    let (first_used, first_opened): (String, String) = connection
        .query_row(
            "SELECT last_used_at, last_opened_at FROM projects WHERE id = ?1",
            [first.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read timestamps");
    assert_ne!(first_used, "2020-01-01T00:00:00.000Z");
    assert_eq!(first_used, first_opened, "both names hold the same instant");

    assert!(
        !touch_project(connection, &first.id).expect("touch same project"),
        "re-selecting the active project is a no-op"
    );
    let first_used_again: String = connection
        .query_row(
            "SELECT last_used_at FROM projects WHERE id = ?1",
            [first.id.as_str()],
            |row| row.get(0),
        )
        .expect("read timestamp");
    assert_eq!(first_used_again, first_used, "the timestamp did not move");

    assert!(touch_project(connection, &second.id).expect("touch second"));
    let first_after_second: String = connection
        .query_row(
            "SELECT last_used_at FROM projects WHERE id = ?1",
            [first.id.as_str()],
            |row| row.get(0),
        )
        .expect("read first timestamp");
    assert_eq!(
        first_after_second, first_used,
        "touching another project leaves the previous one alone"
    );
    let stored: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'last_project_id'",
            [],
            |row| row.get(0),
        )
        .expect("read active project setting");
    assert_eq!(stored, format!("\"{}\"", second.id));

    assert!(!touch_project(connection, "prj_missing").expect("touch unknown"));
    let stored_after: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'last_project_id'",
            [],
            |row| row.get(0),
        )
        .expect("read active project setting");
    assert_eq!(stored_after, stored, "an unknown id changes nothing");
}

#[test]
fn hide_project_keeps_deployments_and_history() {
    let fixture = fixture(&[1, 2, 7, 9]);
    let connection = fixture.database.connection();
    let root = fixture._dir.path().join("hidden-project");
    fs::create_dir(&root).expect("create folder");
    let project = add_project(connection, &root, None, None).expect("add project");

    connection
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug) \
             VALUES ('itm_1', 'skill', 'project', ?1, 'skill')",
            [project.id.as_str()],
        )
        .expect("insert item");
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state) \
             VALUES ('itm_1', 'claude_code', 'project', ?1, 'linked')",
            [project.id.as_str()],
        )
        .expect("insert deployment");
    connection
        .execute(
            "INSERT INTO operations (id, kind, scope, status, summary) \
             VALUES ('op_1', 'Deploy', 'project', 'succeeded', 'deployed skill')",
            [],
        )
        .expect("insert operation");
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) \
             VALUES ('op_1', '/skills/skill', 'linked')",
            [],
        )
        .expect("insert operation file");
    connection
        .execute(
            "INSERT INTO backups (id, original_path, backup_path) \
             VALUES ('bak_1', '/skills/skill/SKILL.md', '/backups/bak_1')",
            [],
        )
        .expect("insert backup");

    assert!(hide_project(connection, &project.id).expect("hide project"));
    let projects = list_projects(connection).expect("list projects");
    let hidden = projects
        .iter()
        .find(|summary| summary.id == project.id)
        .expect("hidden projects stay listed");
    assert!(hidden.hidden);
    assert_eq!(counts(hidden), (1, 0, 0), "counts are unchanged");

    let (deployments, operations, files, backups): (u32, u32, u32, u32) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM deployments), \
                    (SELECT COUNT(*) FROM operations), \
                    (SELECT COUNT(*) FROM operation_files), \
                    (SELECT COUNT(*) FROM backups)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("count related rows");
    assert_eq!((deployments, operations, files, backups), (1, 1, 1, 1));

    assert!(!hide_project(connection, "prj_missing").expect("hide unknown"));
}

#[test]
fn lookups_and_source_root() {
    let fixture = fixture(&[1, 2, 9]);
    let connection = fixture.database.connection();
    let root = fixture._dir.path().join("discovered");
    let scan_root = fixture._dir.path().join("root");
    fs::create_dir(&root).expect("create project folder");
    fs::create_dir(&scan_root).expect("create scan root");
    let added: AddedProject =
        add_project(connection, &root, None, Some(&scan_root)).expect("add project");

    let found = find_project_by_id(connection, &added.id)
        .expect("lookup")
        .expect("project exists");
    assert_eq!(
        found.source_root.as_deref(),
        Some(scan_root.to_string_lossy().as_ref())
    );
    assert_eq!(found.name, "discovered");

    let by_path = find_project_by_path(connection, &root)
        .expect("lookup")
        .expect("project exists");
    assert_eq!(by_path.id, added.id);

    assert!(
        find_project_by_id(connection, "prj_missing")
            .expect("lookup")
            .is_none()
    );
    assert!(
        find_project_by_path(connection, &fixture._dir.path().join("gone"))
            .expect("lookup")
            .is_none()
    );

    match add_project(connection, &fixture._dir.path().join("gone"), None, None) {
        Err(DbError::PathUnresolved { path, .. }) => {
            assert!(path.ends_with("gone"));
        }
        other => panic!("expected PathUnresolved, got {other:?}"),
    }
}
