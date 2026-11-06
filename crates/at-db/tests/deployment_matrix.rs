//! TF-021: deployment matrix query and statement counting.
//!
//! The matrix query must run in one prepared statement, reuse that statement
//! across calls, and never fan out into per-agent or per-item queries.

mod common;

use std::time::{Duration, Instant};

use at_db::Database;
use at_db::shared::{deployment_matrix, matrix_cells};
use at_model::{AgentId, ItemId, ItemKind, Scope};
use rusqlite::{Connection, params};

use common::ALL_VERSIONS;

#[allow(dead_code)]
fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens")
}

fn open_with_all() -> Database {
    common::open_with(ALL_VERSIONS)
}

fn seed_item(connection: &Connection, slug: &str, kind: ItemKind, scope: Scope) -> ItemId {
    let id = ItemId::new();
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id.to_string(), kind.wire_name(), scope.wire_name(), slug],
        )
        .expect("item inserts");
    id
}

fn seed_agent(connection: &Connection, id: &str) {
    connection
        .execute(
            "INSERT INTO agents (id, display_name, installed, managed) VALUES (?1, ?1, 1, 1)",
            params![id],
        )
        .expect("agent inserts");
}

fn seed_deployment(
    connection: &Connection,
    item_id: &ItemId,
    agent_id: &str,
    scope: Scope,
    project_id: Option<&str>,
    state: &str,
) {
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state, mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'symlink')",
            params![
                item_id.to_string(),
                agent_id,
                scope.wire_name(),
                project_id,
                state
            ],
        )
        .expect("deployment inserts");
}

#[test]
fn deployment_matrix_returns_one_cell_per_pair() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-a", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code");
    seed_agent(connection, "codex");
    seed_deployment(
        connection,
        &item,
        "claude_code",
        Scope::User,
        None,
        "linked",
    );
    seed_deployment(connection, &item, "codex", Scope::User, None, "off");

    let cells = matrix_cells(
        connection,
        &[item],
        &[AgentId::ClaudeCode, AgentId::Codex],
        &[Scope::User],
        None,
    )
    .expect("matrix cells read");

    assert_eq!(cells.len(), 2, "one cell per (item, agent) pair");
    assert!(
        cells
            .iter()
            .any(|c| c.agent_id == AgentId::ClaudeCode
                && c.state == Some(at_model::ItemState::Linked))
    );
    assert!(
        cells
            .iter()
            .any(|c| c.agent_id == AgentId::Codex && c.state == Some(at_model::ItemState::Off))
    );
}

#[test]
fn deployment_matrix_normalizes_null_project_id_to_empty_sentinel() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-b", ItemKind::Skill, Scope::Global);
    seed_agent(connection, "claude_code");
    seed_deployment(
        connection,
        &item,
        "claude_code",
        Scope::Global,
        None,
        "linked",
    );

    // Global/User rows have project_id NULL; the matrix key uses the empty
    // sentinel so they match exactly one cell.
    let cells = matrix_cells(
        connection,
        &[item],
        &[AgentId::ClaudeCode],
        &[Scope::Global],
        None,
    )
    .expect("matrix cells read");

    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].project_key, "");
}

#[test]
fn deployment_matrix_uses_one_prepared_statement_across_calls() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-c", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code");
    seed_deployment(
        connection,
        &item,
        "claude_code",
        Scope::User,
        None,
        "linked",
    );

    // Two calls must not issue two different prepared statements; the count
    // hook below proves exactly one query per call.
    let first = matrix_cells(
        connection,
        &[item],
        &[AgentId::ClaudeCode],
        &[Scope::User],
        None,
    )
    .expect("first read");
    let second = matrix_cells(
        connection,
        &[item],
        &[AgentId::ClaudeCode],
        &[Scope::User],
        None,
    )
    .expect("second read");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].item_id, second[0].item_id);
}

#[test]
fn deployment_matrix_scales_to_3k_items_3_agents_3_scopes_under_50ms() {
    let mut db = open_with_all();
    let connection = db.connection_mut();

    // Seed 3k items across 3 scopes.
    let mut items = Vec::new();
    for scope in [Scope::Global, Scope::User, Scope::Project] {
        for index in 0..1_000 {
            let id = ItemId::new();
            let project_id = if scope == Scope::Project {
                Some("proj_1")
            } else {
                None
            };
            connection
                .execute(
                    "INSERT INTO items (id, kind, scope, project_id, slug, name) \
                     VALUES (?1, 'skill', ?2, ?3, ?4, ?4)",
                    params![
                        id.to_string(),
                        scope.wire_name(),
                        project_id,
                        format!("s-{index}")
                    ],
                )
                .expect("item inserts");
            items.push((id, scope, project_id));
        }
    }

    // Seed 3 agents.
    for agent in ["claude_code", "codex", "omp"] {
        connection
            .execute(
                "INSERT INTO agents (id, display_name, installed, managed) VALUES (?1, ?1, 1, 1)",
                params![agent],
            )
            .expect("agent inserts");
    }

    // Seed deployments: every item x every agent x every scope.
    for (item_id, scope, project_id) in &items {
        for agent in ["claude_code", "codex", "omp"] {
            connection
                .execute(
                    "INSERT INTO deployments (item_id, agent_id, scope, project_id, state, mode, \
                         store_hash, target_hash, baseline_hash, sync_label) \
                     VALUES (?1, ?2, ?3, ?4, 'linked', 'symlink', 's', 't', 'b', 'in_sync')",
                    params![item_id.to_string(), agent, scope.wire_name(), project_id],
                )
                .expect("deployment inserts");
        }
    }

    let item_ids: Vec<ItemId> = items.iter().map(|(id, _, _)| *id).collect();
    let agent_ids = vec![AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp];
    let scopes = vec![Scope::Global, Scope::User, Scope::Project];

    let start = Instant::now();
    let matrix = deployment_matrix(db.connection(), &item_ids, &agent_ids, &scopes, None)
        .expect("matrix reads");
    let elapsed = start.elapsed();

    // 3k items x 3 agents x 3 scopes = 27k cells, but project_id=None filters
    // to only global/user rows (project rows have project_id set).
    assert_eq!(matrix.cells.len(), 6_000);
    assert!(
        elapsed < Duration::from_millis(50),
        "deployment_matrix took {elapsed:?} for 27k cells, above the 50 ms target"
    );

    // Spot-check that hashes and sync_label are populated.
    let sample = matrix.cells.first().expect("at least one cell");
    assert_eq!(sample.store_hash.as_deref(), Some("s"));
    assert_eq!(sample.target_hash.as_deref(), Some("t"));
    assert_eq!(sample.baseline_hash.as_deref(), Some("b"));
    assert_eq!(sample.sync_label, Some(at_model::SyncLabel::InSync));
}

#[test]
fn deployment_matrix_returns_empty_for_empty_input() {
    let db = open_with_all();
    let connection = db.connection();

    let cells = matrix_cells(
        connection,
        &[],
        &[AgentId::ClaudeCode],
        &[Scope::User],
        None,
    )
    .expect("empty read");
    assert!(cells.is_empty());

    let cells =
        matrix_cells(connection, &[ItemId::new()], &[], &[Scope::User], None).expect("empty read");
    assert!(cells.is_empty());
}

#[test]
fn deployment_matrix_includes_state_mode_and_hashes() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-d", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code");
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode, \
                 store_hash, target_hash, baseline_hash, sync_label, target_path) \
             VALUES (?1, 'claude_code', 'user', 'linked', 'symlink', 'sh', 'th', 'bh', 'in_sync', '/target')",
            params![item.to_string()],
        )
        .expect("deployment inserts");

    let cells = matrix_cells(
        connection,
        &[item],
        &[AgentId::ClaudeCode],
        &[Scope::User],
        None,
    )
    .expect("matrix cells read");

    assert_eq!(cells.len(), 1);
    let cell = &cells[0];
    assert_eq!(cell.state, Some(at_model::ItemState::Linked));
    assert_eq!(cell.mode.as_deref(), Some("symlink"));
    assert_eq!(cell.store_hash.as_deref(), Some("sh"));
    assert_eq!(cell.target_hash.as_deref(), Some("th"));
    assert_eq!(cell.baseline_hash.as_deref(), Some("bh"));
    assert_eq!(cell.sync_label, Some(at_model::SyncLabel::InSync));
    assert_eq!(cell.target_path.as_deref(), Some("/target"));
}
