//! TF-013: schema for the deployment matrix.

mod common;

use std::time::{Duration, Instant};

use at_db::DbError;
use at_model::{ItemId, ItemState, SyncLabel};
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

/// Insert one item and return its id.
fn insert_item(connection: &Connection, slug: &str) -> String {
    let id = ItemId::new().to_string();
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'user', ?2, ?2)",
            params![id, slug],
        )
        .expect("item inserts");
    id
}

fn insert_cell(
    connection: &Connection,
    item_id: &str,
    agent_id: &str,
    state: &str,
    mode: Option<&str>,
) {
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode, target_path) \
             VALUES (?1, ?2, 'user', ?3, ?4, '/home/me/.claude/skills/x')",
            params![item_id, agent_id, state, mode],
        )
        .expect("deployment inserts");
}

/// The query plan for one statement, as SQLite explains it.
fn plan(connection: &Connection, sql: &str, parameters: &[&dyn rusqlite::ToSql]) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("plan prepares");
    statement
        .query_map(parameters, |row| row.get::<_, String>(3))
        .expect("plan runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan rows are text")
}

#[test]
fn stored_modes_take_the_at_fs_values_and_reject_the_at_model_wire_name() {
    // at-fs::StoredLinkMode is the codec for this column: `symlink`, `junction`
    // (TF-038's Windows fallback) and `copy`. at-model::LinkMode serialises as
    // `link`, which is not a stored value and must stay rejected.
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    for (index, mode) in ["symlink", "junction", "copy", "native", "none"]
        .into_iter()
        .enumerate()
    {
        insert_cell(
            connection,
            &insert_item(connection, &format!("s{index}")),
            "claude_code",
            "linked",
            Some(mode),
        );
    }
    let error = connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode) \
             VALUES (?1, 'claude_code', 'user', 'linked', 'link')",
            params![insert_item(connection, "link-mode")],
        )
        .expect_err("at-model's `link` is not a stored mode");
    assert_constraint_naming(constraint(error), "check");

    let error = connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode) \
             VALUES (?1, 'claude_code', 'user', 'linked', 'hardlink')",
            params![insert_item(connection, "bogus-mode")],
        )
        .expect_err("unknown modes fail");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn schema_v4_accepts_only_the_original_persisted_sync_labels() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    for (index, state) in ItemState::ALL.iter().enumerate() {
        insert_cell(
            connection,
            &insert_item(connection, &format!("state-{index}")),
            "claude_code",
            state.wire_name(),
            None,
        );
    }
    for (index, label) in [
        SyncLabel::InSync,
        SyncLabel::ProjectOnly,
        SyncLabel::LibraryNewer,
        SyncLabel::ProjectNewer,
    ]
    .iter()
    .enumerate()
    {
        let item = insert_item(connection, &format!("label-{index}"));
        connection
            .execute(
                "INSERT INTO deployments (item_id, agent_id, scope, state, sync_label) \
                 VALUES (?1, 'codex', 'user', 'linked', ?2)",
                params![item, label.wire_name()],
            )
            .expect("sync label inserts");
    }

    let error = connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state) \
             VALUES (?1, 'codex', 'user', 'shadowed')",
            params![insert_item(connection, "bad-state")],
        )
        .expect_err("unknown state fails");
    assert_constraint_naming(constraint(error), "check");

    let error = connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, sync_label) \
             VALUES (?1, 'codex', 'user', 'linked', 'ahead')",
            params![insert_item(connection, "bad-label")],
        )
        .expect_err("unknown sync label fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn the_matrix_cell_is_unique_and_toggling_updates_one_row() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "matrix");

    insert_cell(connection, &item, "claude_code", "off", Some("none"));
    let error = connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state) \
             VALUES (?1, 'claude_code', 'user', 'off')",
            [&item],
        )
        .expect_err("a second cell for the same matrix position fails");
    assert_constraint_naming(constraint(error), "unique");

    // Toggling on is an update of the existing cell: the repository updates by
    // the matrix key and inserts only when no row matched.
    let updated = connection
        .execute(
            "UPDATE deployments SET state = 'linked', mode = 'symlink', \
             updated_at = '2026-11-05T10:00:00.000Z', operation_id = 'op_toggle' \
             WHERE item_id = ?1 AND agent_id = 'claude_code' AND scope = 'user' \
             AND COALESCE(project_id, '') = ''",
            [&item],
        )
        .expect("update cell");
    assert_eq!(updated, 1);

    let row: (u32, String, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT COUNT(*), MAX(state), MAX(mode), MAX(operation_id) FROM deployments WHERE item_id = ?1",
            [&item],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("cell reads back");
    assert_eq!(row.0, 1, "toggling off then on keeps one row");
    assert_eq!(row.1, "linked");
    assert_eq!(row.2.as_deref(), Some("symlink"));
    assert_eq!(row.3.as_deref(), Some("op_toggle"));

    // The project dimension keeps the same item/agent/scope distinct.
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state) \
             VALUES (?1, 'claude_code', 'project', 'prj_a', 'linked')",
            [&item],
        )
        .expect("project cell inserts");
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state) \
             VALUES (?1, 'claude_code', 'project', 'prj_b', 'linked')",
            [&item],
        )
        .expect("second project cell inserts");
    let cells: u32 = connection
        .query_row("SELECT COUNT(*) FROM deployments", [], |row| row.get(0))
        .expect("count cells");
    assert_eq!(cells, 3);
}

#[test]
fn a_state_change_records_the_operation_and_a_rescan_refreshes_the_verification_time() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "verified");

    insert_cell(connection, &item, "omp", "linked", Some("symlink"));
    connection
        .execute(
            "UPDATE deployments SET operation_id = 'op_1', updated_at = '2026-11-05T10:00:00.000Z' \
             WHERE item_id = ?1",
            [&item],
        )
        .expect("state change records the operation");

    connection
        .execute(
            "UPDATE deployments SET last_verified_at = '2026-11-05T10:05:00.000Z' WHERE item_id = ?1",
            [&item],
        )
        .expect("rescan records verification");

    let row: (String, String) = connection
        .query_row(
            "SELECT operation_id, last_verified_at FROM deployments WHERE item_id = ?1",
            [&item],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row reads back");
    assert_eq!(row.0, "op_1");
    assert_eq!(row.1, "2026-11-05T10:05:00.000Z");
}

#[test]
fn drift_and_off_rows_store_the_hashes_and_paths_their_state_needs() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    let drifted = insert_item(connection, "drifted");
    connection
        .execute(
            "INSERT INTO deployments \
             (item_id, agent_id, scope, state, mode, target_path, store_hash, target_hash, baseline_hash) \
             VALUES (?1, 'claude_code', 'user', 'drifted', 'copy', '/home/me/.claude/skills/d', 'store', 'external', 'baseline')",
            [&drifted],
        )
        .expect("drifted row inserts");

    let row: (String, String, String) = connection
        .query_row(
            "SELECT store_hash, target_hash, baseline_hash FROM deployments \
             WHERE item_id = ?1 AND baseline_hash <> target_hash",
            [&drifted],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("a drift row has mismatching baseline and target hashes");
    assert_eq!(
        row,
        (
            "store".to_owned(),
            "external".to_owned(),
            "baseline".to_owned()
        )
    );

    // An `off` row remembers where the item was disabled, even though the file
    // is gone; whether a file exists is not a database fact.
    let off = insert_item(connection, "off");
    insert_cell(connection, &off, "codex", "off", Some("none"));
    let target: String = connection
        .query_row(
            "SELECT target_path FROM deployments WHERE item_id = ?1",
            [&off],
            |row| row.get(0),
        )
        .expect("off row keeps its target path");
    assert_eq!(target, "/home/me/.claude/skills/x");
}

#[test]
fn deleting_an_item_cascades_its_deployment_cells() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "cascade");
    insert_cell(connection, &item, "claude_code", "linked", Some("symlink"));
    insert_cell(connection, &item, "codex", "off", Some("none"));

    connection
        .execute("DELETE FROM items WHERE id = ?1", [&item])
        .expect("delete item");
    let cells: u32 = connection
        .query_row("SELECT COUNT(*) FROM deployments", [], |row| row.get(0))
        .expect("count cells");
    assert_eq!(cells, 0);
}

#[test]
fn the_skills_screen_matrix_query_uses_the_agent_scope_index_and_stays_under_50_ms() {
    let mut database = open_with(ALL_VERSIONS);
    let transaction = database
        .connection_mut()
        .transaction()
        .expect("begin seeding");
    let items: Vec<String> = (0..3_000_u32)
        .map(|index| {
            let id = ItemId::new().to_string();
            transaction
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) \
                     VALUES (?1, 'skill', 'user', ?2, ?2)",
                    params![id, format!("skill-{index}")],
                )
                .expect("seed item");
            id
        })
        .collect();
    {
        let mut cell = transaction
            .prepare(
                "INSERT INTO deployments (item_id, agent_id, scope, state, mode, target_path) \
                 VALUES (?1, ?2, 'user', 'linked', 'symlink', ?3)",
            )
            .expect("prepare cell insert");
        for agent in ["claude_code", "codex", "omp"] {
            for item in &items {
                cell.execute(params![item, agent, format!("/home/me/{agent}/skills/x")])
                    .expect("seed cell");
            }
        }
    }
    transaction.commit().expect("commit seeding");

    let sql = "SELECT item_id, state, target_path FROM deployments \
               WHERE agent_id = ?1 AND scope = ?2";
    let connection = database.connection();
    assert!(
        plan(connection, sql, &[&"codex", &"user"])
            .iter()
            .any(|detail| detail.contains("idx_deployments_agent_scope")),
        "the matrix query uses the agent/scope index: {:?}",
        plan(connection, sql, &[&"codex", &"user"])
    );

    let start = Instant::now();
    let rows: Vec<(String, String, String)> = connection
        .prepare(sql)
        .expect("prepare matrix query")
        .query_map(params!["codex", "user"], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("run matrix query")
        .collect::<Result<Vec<_>, _>>()
        .expect("cells read back");
    let elapsed = start.elapsed();
    assert_eq!(rows.len(), 3_000);
    assert!(
        elapsed < Duration::from_millis(50),
        "one agent's 3k-cell matrix slice took {elapsed:?}, above the 50 ms target"
    );
}

#[test]
fn deployments_columns_and_indexes_match_the_story() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    assert_eq!(
        columns(connection, "deployments"),
        vec![
            "id",
            "item_id",
            "agent_id",
            "scope",
            "project_id",
            "state",
            "mode",
            "target_path",
            "link_target",
            "store_hash",
            "target_hash",
            "baseline_hash",
            "sync_label",
            "updated_at",
            "last_verified_at",
            "operation_id",
        ]
    );

    let mut indexes = index_names(connection, "deployments");
    indexes.sort();
    assert_eq!(
        indexes,
        vec![
            "idx_deployments_agent_scope",
            "idx_deployments_matrix",
            "idx_deployments_project_id",
            "idx_deployments_state",
            "idx_deployments_sync_label",
            "idx_deployments_target_path",
        ]
    );
}
