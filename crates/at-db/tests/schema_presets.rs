//! TF-017: schema for presets, their members and application history.

mod common;

use at_db::DbError;
use at_model::ItemId;
use rusqlite::Connection;
use rusqlite::params;

use common::{ALL_VERSIONS, columns, index_names, open_with};

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

fn insert_preset(connection: &Connection, id: &str, name: &str, scope: &str) {
    connection
        .execute(
            "INSERT INTO presets (id, name, description, scope) VALUES (?1, ?2, 'Bundle', ?3)",
            params![id, name, scope],
        )
        .expect("preset inserts");
}

fn item_id() -> String {
    ItemId::new().to_string()
}

/// Apply `preset_id` to an agent, recording the prior enablement of every member.
fn apply_preset(
    connection: &Connection,
    preset_id: &str,
    target_kind: &str,
    target_id: &str,
) -> i64 {
    let members: Vec<(String, String, i64)> = connection
        .prepare("SELECT item_id, kind, enabled FROM preset_members WHERE preset_id = ?1")
        .expect("prepare member query")
        .query_map([preset_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("run member query")
        .collect::<Result<_, _>>()
        .expect("members read back");

    connection
        .execute(
            "INSERT INTO preset_applications (preset_id, target_kind, target_id, operation_id, member_count) \
             VALUES (?1, ?2, ?3, 'op_apply', ?4)",
            params![preset_id, target_kind, target_id, members.len() as i64],
        )
        .expect("application inserts");
    let application_id = connection.last_insert_rowid();
    for (item_id, _kind, enabled) in members {
        // The prior state is the state before apply; for a new member it is off.
        connection
            .execute(
                "INSERT INTO preset_application_members (application_id, item_id, prior_enabled) \
                 VALUES (?1, ?2, ?3)",
                params![application_id, item_id, if enabled == 1 { 0 } else { 1 }],
            )
            .expect("application member inserts");
    }
    application_id
}

#[test]
fn preset_names_are_case_insensitively_unique_per_scope() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    insert_preset(connection, "prs_frontend", "Frontend", "user");
    let error = connection
        .execute(
            "INSERT INTO presets (id, name, scope) VALUES ('prs_dupe', 'frontend', 'user')",
            [],
        )
        .expect_err("`Frontend` and `frontend` collide in one scope");
    assert_constraint_naming(constraint(error), "unique");

    // A different scope is a different namespace.
    insert_preset(connection, "prs_project", "frontend", "project");
    let total: u32 = connection
        .query_row("SELECT COUNT(*) FROM presets", [], |row| row.get(0))
        .expect("count presets");
    assert_eq!(total, 2);

    // Defaults: not builtin, description optional, timestamps present.
    let defaults: (i64, Option<String>, String, String) = connection
        .query_row(
            "SELECT is_builtin, description, created_at, updated_at FROM presets WHERE id = 'prs_frontend'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("defaults read back");
    assert_eq!(defaults.0, 0);
    assert_eq!(defaults.1, Some("Bundle".to_owned()));
    assert!(!defaults.2.is_empty());
    assert!(!defaults.3.is_empty());
}

#[test]
fn members_are_unique_and_an_explicit_opt_out_round_trips() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    insert_preset(connection, "prs_1", "Frontend", "user");
    let item = item_id();

    connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind, enabled) VALUES ('prs_1', ?1, 'skill', 0)",
            [&item],
        )
        .expect("opt-out member inserts");
    let error = connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_1', ?1, 'skill')",
            [&item],
        )
        .expect_err("a member appears once per preset");
    assert_constraint_naming(constraint(error), "unique");

    let stored: (i64, String) = connection
        .query_row(
            "SELECT enabled, kind FROM preset_members WHERE preset_id = 'prs_1' AND item_id = ?1",
            [&item],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("member reads back");
    assert_eq!(
        stored,
        (0, "skill".to_owned()),
        "enabled=0 survives round-trip"
    );

    // The default is enabled.
    let second = item_id();
    connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_1', ?1, 'mcp_server')",
            [&second],
        )
        .expect("default member inserts");
    let enabled: i64 = connection
        .query_row(
            "SELECT enabled FROM preset_members WHERE item_id = ?1",
            [&second],
            |row| row.get(0),
        )
        .expect("default reads back");
    assert_eq!(enabled, 1);

    let error = connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_1', ?1, 'prompt')",
            [&item_id()],
        )
        .expect_err("unknown kind fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn applying_a_preset_records_the_prior_state_of_every_member() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    insert_preset(connection, "prs_1", "Frontend", "user");

    let mut members = Vec::new();
    for enabled in [1_i64, 0, 1] {
        let item = item_id();
        members.push((item.clone(), enabled));
        connection
            .execute(
                "INSERT INTO preset_members (preset_id, item_id, kind, enabled) \
                 VALUES ('prs_1', ?1, 'skill', ?2)",
                params![item, enabled],
            )
            .expect("member inserts");
    }

    let application = apply_preset(connection, "prs_1", "agent", "claude_code");
    let application_row: (String, String, i64) = connection
        .query_row(
            "SELECT target_kind, target_id, member_count FROM preset_applications WHERE id = ?1",
            [application],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("application reads back");
    assert_eq!(
        application_row,
        ("agent".to_owned(), "claude_code".to_owned(), 3)
    );

    let recorded: Vec<(String, Option<i64>)> = connection
        .prepare(
            "SELECT item_id, prior_enabled FROM preset_application_members \
             WHERE application_id = ?1 ORDER BY id",
        )
        .expect("prepare prior-state query")
        .query_map([application], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("run prior-state query")
        .collect::<Result<_, _>>()
        .expect("prior states read back");
    assert_eq!(recorded.len(), 3, "one row per member");
    for ((item, enabled), (recorded_item, prior)) in members.iter().zip(recorded) {
        assert_eq!(&recorded_item, item);
        assert_eq!(prior, Some(if *enabled == 1 { 0 } else { 1 }));
    }

    let error = connection
        .execute(
            "INSERT INTO preset_applications (preset_id, target_kind, target_id) \
             VALUES ('prs_1', 'workspace', 'x')",
            [],
        )
        .expect_err("unknown target kind fails");
    assert_constraint_naming(constraint(error), "check");

    // An unapply restores from the recorded prior state; the item existed, so
    // prior_enabled is not NULL.
    connection
        .execute(
            "UPDATE preset_application_members SET prior_enabled = NULL \
             WHERE application_id = ?1 AND item_id = ?2",
            params![application, members[0].0],
        )
        .expect("a missing item leaves prior_enabled NULL");
    let prior: Option<i64> = connection
        .query_row(
            "SELECT prior_enabled FROM preset_application_members \
             WHERE application_id = ?1 AND item_id = ?2",
            params![application, members[0].0],
            |row| row.get(0),
        )
        .expect("prior state reads back");
    assert_eq!(prior, None);
}

#[test]
fn deleting_a_preset_cascades_members_and_keeps_application_history() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    insert_preset(connection, "prs_1", "Frontend", "user");
    let first = item_id();
    let second = item_id();
    for item in [&first, &second] {
        connection
            .execute(
                "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_1', ?1, 'skill')",
                [item],
            )
            .expect("member inserts");
    }
    let application = apply_preset(connection, "prs_1", "project", "prj_a");

    connection
        .execute("DELETE FROM presets WHERE id = 'prs_1'", [])
        .expect("delete preset");

    let members: u32 = connection
        .query_row("SELECT COUNT(*) FROM preset_members", [], |row| row.get(0))
        .expect("count members");
    assert_eq!(members, 0, "members cascade");

    let applications: u32 = connection
        .query_row("SELECT COUNT(*) FROM preset_applications", [], |row| {
            row.get(0)
        })
        .expect("count applications");
    assert_eq!(applications, 1, "application history survives");

    let application_members: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM preset_application_members WHERE application_id = ?1",
            [application],
            |row| row.get(0),
        )
        .expect("count application members");
    assert_eq!(application_members, 2, "recorded prior states survive");
}

#[test]
fn a_rebuild_reproduces_presets_and_members_but_preserves_applications() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    insert_preset(connection, "prs_builtin", "Frontend", "user");
    let item = item_id();
    connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_builtin', ?1, 'skill')",
            [&item],
        )
        .expect("member inserts");
    let application = apply_preset(connection, "prs_builtin", "agent", "claude_code");

    // Rebuild: derived rows are dropped and re-read from presets.json.
    connection
        .execute("DELETE FROM presets", [])
        .expect("clear derived presets");
    insert_preset(connection, "prs_builtin", "Frontend", "user");
    connection
        .execute(
            "INSERT INTO preset_members (preset_id, item_id, kind) VALUES ('prs_builtin', ?1, 'skill')",
            [&item],
        )
        .expect("member is re-created");

    let rebuilt: (u32, u32, u32) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM presets), \
                    (SELECT COUNT(*) FROM preset_members), \
                    (SELECT COUNT(*) FROM preset_applications)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("counts read back");
    assert_eq!(
        rebuilt,
        (1, 1, 1),
        "presets and members rebuild; applications stay"
    );

    let recorded: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM preset_application_members WHERE application_id = ?1",
            [application],
            |row| row.get(0),
        )
        .expect("recorded states read back");
    assert_eq!(
        recorded, 1,
        "the recorded prior state is preserved, not recreated"
    );
}

#[test]
fn preset_columns_and_indexes_match_the_story() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    assert_eq!(
        columns(connection, "presets"),
        vec![
            "id",
            "name",
            "description",
            "scope",
            "is_builtin",
            "created_at",
            "updated_at",
        ]
    );
    assert_eq!(
        columns(connection, "preset_members"),
        vec!["id", "preset_id", "item_id", "kind", "enabled"]
    );
    assert_eq!(
        columns(connection, "preset_applications"),
        vec![
            "id",
            "preset_id",
            "target_kind",
            "target_id",
            "applied_at",
            "operation_id",
            "member_count",
        ]
    );
    assert_eq!(
        columns(connection, "preset_application_members"),
        vec!["id", "application_id", "item_id", "prior_enabled"]
    );

    let mut applications = index_names(connection, "preset_applications");
    applications.sort();
    assert_eq!(
        applications,
        vec![
            "idx_preset_applications_preset_id",
            "idx_preset_applications_target",
        ]
    );
    // UNIQUE constraints own the remaining lookups: preset_members by
    // (preset_id, item_id) and preset_application_members by
    // (application_id, item_id).
    assert!(index_names(connection, "preset_members").is_empty());
    assert!(index_names(connection, "preset_application_members").is_empty());
}
