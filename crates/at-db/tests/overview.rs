//! TA-042: overview queries over the operations, items, deployments, locks,
//! health_checks, drift_events and agents tables.
//!
//! Every query is read-only and uses one prepared statement; the UI can render
//! while a scan runs elsewhere.

mod common;

use std::time::{Duration, Instant};

use at_db::Database;
use at_db::overview::{
    AttentionKind, ScopeCounts, counts_for_scope, drift_candidates, failing_health_checks,
    last_edited, missing_agent_candidates, native_deployment_candidates, needs_attention,
    recent_changes,
};
use at_model::{AgentId, ItemId, ItemKind, Scope};
use rusqlite::{Connection, params};

use common::ALL_VERSIONS;

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

fn seed_agent(connection: &Connection, id: &str, installed: bool, managed: bool) {
    connection
        .execute(
            "INSERT INTO agents (id, display_name, installed, managed) VALUES (?1, ?1, ?2, ?3)",
            params![id, installed, managed],
        )
        .expect("agent inserts");
}

fn seed_lock(connection: &Connection, item_id: &ItemId, scope: Scope) {
    connection
        .execute(
            "INSERT INTO locks (item_id, scope) VALUES (?1, ?2)",
            params![item_id.to_string(), scope.wire_name()],
        )
        .expect("lock inserts");
}

fn seed_operation(
    connection: &Connection,
    id: &str,
    kind: &str,
    scope: Option<Scope>,
    created_at: &str,
) {
    connection
        .execute(
            "INSERT INTO operations (id, kind, scope, actor, status, created_at) \
             VALUES (?1, ?2, ?3, 'ui', 'succeeded', ?4)",
            params![id, kind, scope.map(Scope::wire_name), created_at,],
        )
        .expect("operation inserts");
}

fn seed_operation_file(connection: &Connection, operation_id: &str, path: &str) {
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) VALUES (?1, ?2, 'modified')",
            params![operation_id, path],
        )
        .expect("file inserts");
}

#[test]
fn counts_for_scope_returns_skills_mcp_rules_locks_in_one_query() {
    let db = open_with_all();
    let connection = db.connection();

    // 12 items across 3 scopes: 4 per scope, mixed kinds.
    for scope in [Scope::Global, Scope::User, Scope::Project] {
        for index in 0..4 {
            let kind = match index % 3 {
                0 => ItemKind::Skill,
                1 => ItemKind::McpServer,
                _ => ItemKind::Rule,
            };
            seed_item(connection, &format!("item-{index}"), kind, scope);
        }
    }

    // Locks: 2 global, 1 user. Lock items are separate from the 12 counted
    // items so they do not inflate the kind counts.
    let lock_items: Vec<ItemId> = (0..3)
        .map(|i| {
            let id = ItemId::new();
            // Lock items use 'local' scope so they do not inflate the per-scope
            // kind counts the test asserts on.
            connection
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'local', ?2, ?2)",
                    params![id.to_string(), format!("lock-{i}")],
                )
                .expect("lock item inserts");
            id
        })
        .collect();
    seed_lock(connection, &lock_items[0], Scope::Global);
    seed_lock(connection, &lock_items[1], Scope::Global);
    seed_lock(connection, &lock_items[2], Scope::User);

    let counts = counts_for_scope(connection).expect("counts read");
    let by_scope: std::collections::BTreeMap<Scope, ScopeCounts> = counts.into_iter().collect();

    // Global: 4 items (1 skill, 1 mcp, 2 rules... wait, 4 items with index%3 pattern)
    // index 0=skill, 1=mcp, 2=rule, 3=skill -> 2 skills, 1 mcp, 1 rule
    let global = by_scope.get(&Scope::Global).expect("global counts");
    assert_eq!(global.skills, 2);
    assert_eq!(global.mcp, 1);
    assert_eq!(global.rules, 1);
    assert_eq!(global.locks, 2);

    let user = by_scope.get(&Scope::User).expect("user counts");
    assert_eq!(user.skills, 2);
    assert_eq!(user.mcp, 1);
    assert_eq!(user.rules, 1);
    assert_eq!(user.locks, 1);

    let project = by_scope.get(&Scope::Project).expect("project counts");
    assert_eq!(project.skills, 2);
    assert_eq!(project.mcp, 1);
    assert_eq!(project.rules, 1);
    assert_eq!(project.locks, 0);
}

#[test]
fn last_edited_returns_max_created_at_per_scope_and_null_when_empty() {
    let db = open_with_all();
    let connection = db.connection();

    // No operations: last_edited returns empty.
    let empty = last_edited(connection).expect("last_edited reads");
    assert!(empty.is_empty());

    seed_operation(
        connection,
        "op1",
        "deploy",
        Some(Scope::User),
        "2026-01-01T00:00:00Z",
    );
    seed_operation(
        connection,
        "op2",
        "edit_skill",
        Some(Scope::User),
        "2026-01-02T00:00:00Z",
    );
    seed_operation(
        connection,
        "op3",
        "deploy",
        Some(Scope::Project),
        "2026-01-03T00:00:00Z",
    );

    let edited = last_edited(connection).expect("last_edited reads");
    let by_scope: std::collections::BTreeMap<Scope, Option<String>> = edited
        .into_iter()
        .map(|o| (o.scope, o.last_edited))
        .collect();

    assert_eq!(
        by_scope.get(&Scope::User).and_then(|s| s.as_deref()),
        Some("2026-01-02T00:00:00Z")
    );
    assert_eq!(
        by_scope.get(&Scope::Project).and_then(|s| s.as_deref()),
        Some("2026-01-03T00:00:00Z")
    );
    assert!(!by_scope.contains_key(&Scope::Global));
    assert!(!by_scope.contains_key(&Scope::Local));
}

#[test]
fn recent_changes_returns_at_most_limit_ordered_created_at_desc_id_desc() {
    let db = open_with_all();
    let connection = db.connection();

    // 25 operations with distinct created_at.
    for i in 0..25 {
        seed_operation(
            connection,
            &format!("op{i:02}"),
            "deploy",
            Some(Scope::User),
            &format!("2026-01-{i:02}T00:00:00Z"),
        );
    }

    let changes = recent_changes(connection, 20).expect("recent_changes reads");
    assert_eq!(changes.len(), 20, "limit=20 returns at most 20 rows");

    // Verify ordering: created_at desc, id desc for ties.
    for window in changes.windows(2) {
        let a = &window[0].operation;
        let b = &window[1].operation;
        assert!(
            a.created_at > b.created_at || (a.created_at == b.created_at && a.id > b.id),
            "ordering violated: {} ({}) before {} ({})",
            a.id,
            a.created_at,
            b.id,
            b.created_at
        );
    }
}

#[test]
fn recent_changes_includes_file_count() {
    let db = open_with_all();
    let connection = db.connection();

    seed_operation(
        connection,
        "op1",
        "deploy",
        Some(Scope::User),
        "2026-01-01T00:00:00Z",
    );
    seed_operation_file(connection, "op1", "/path/a");
    seed_operation_file(connection, "op1", "/path/b");
    seed_operation(
        connection,
        "op2",
        "scan",
        Some(Scope::User),
        "2026-01-02T00:00:00Z",
    );

    let changes = recent_changes(connection, 10).expect("recent_changes reads");
    assert_eq!(changes.len(), 2);
    let op1 = changes
        .iter()
        .find(|c| c.operation.id == "op1")
        .expect("op1 present");
    assert_eq!(op1.file_count, 2);
    let op2 = changes
        .iter()
        .find(|c| c.operation.id == "op2")
        .expect("op2 present");
    assert_eq!(op2.file_count, 0);
}

#[test]
fn overview_queries_run_under_50ms_on_10k_operations() {
    let mut db = open_with_all();
    let connection = db.connection_mut();

    // Seed 10k operations with files.
    for i in 0..10_000 {
        let id = format!("op{i:05}");
        connection
            .execute(
                "INSERT INTO operations (id, kind, scope, actor, status, created_at) \
                 VALUES (?1, 'deploy', 'user', 'ui', 'succeeded', ?2)",
                params![
                    id,
                    format!("2026-01-{:02}T00:00:{:02}Z", i % 28 + 1, i % 60)
                ],
            )
            .expect("operation inserts");
        connection
            .execute(
                "INSERT INTO operation_files (operation_id, path, action) VALUES (?1, ?2, 'modified')",
                params![id, format!("/path/{i}")],
            )
            .expect("file inserts");
    }

    let conn = db.connection();
    let start = Instant::now();
    let _ = counts_for_scope(conn).expect("counts");
    let _ = last_edited(conn).expect("last_edited");
    let _ = recent_changes(conn, 20).expect("recent_changes");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(50),
        "overview queries took {elapsed:?} on 10k operations, above 50 ms"
    );
}

#[test]
fn overview_queries_are_read_only() {
    let db = open_with_all();
    let connection = db.connection();

    seed_item(connection, "skill-x", ItemKind::Skill, Scope::User);
    seed_operation(
        connection,
        "op1",
        "deploy",
        Some(Scope::User),
        "2026-01-01T00:00:00Z",
    );

    // All queries must succeed on a read-only connection.
    let _ = counts_for_scope(connection).expect("counts");
    let _ = last_edited(connection).expect("last_edited");
    let _ = recent_changes(connection, 10).expect("recent_changes");
    let _ = failing_health_checks(connection, 10).expect("failing_health");
    let _ = drift_candidates(connection, 10).expect("drift");
    let _ = native_deployment_candidates(connection, 10).expect("native");
    let _ = missing_agent_candidates(connection, 10).expect("missing_agents");
    let _ = needs_attention(connection, 10).expect("needs_attention");
}

// ---- needs-attention candidates ----

#[test]
fn failing_health_checks_returns_failed_newest_per_item_agent() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-y", ItemKind::McpServer, Scope::User);
    seed_agent(connection, "claude_code", true, true);

    // Old failed check, then a newer ok check: the item should NOT appear.
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, agent_id, server_name, checked_at, status, trigger) \
             VALUES (?1, 'user', 'claude_code', 'srv', 1000, 'failed', 'manual')",
            params![item.to_string()],
        )
        .expect("health insert");
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, agent_id, server_name, checked_at, status, trigger) \
             VALUES (?1, 'user', 'claude_code', 'srv', 2000, 'ok', 'manual')",
            params![item.to_string()],
        )
        .expect("health insert");

    let candidates = failing_health_checks(connection, 10).expect("candidates read");
    assert!(candidates.is_empty(), "newest check is ok, not failed");

    // Now add a newer failed check.
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, agent_id, server_name, checked_at, status, trigger, error_code) \
             VALUES (?1, 'user', 'claude_code', 'srv', 3000, 'failed', 'manual', 'timeout')",
            params![item.to_string()],
        )
        .expect("health insert");

    let candidates = failing_health_checks(connection, 10).expect("candidates read");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].kind, AttentionKind::FailingHealthCheck);
    assert_eq!(candidates[0].item_id, Some(item));
    assert_eq!(candidates[0].agent_id, Some(AgentId::ClaudeCode));
}

#[test]
fn drift_candidates_returns_unresolved_events() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-z", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true, true);

    connection
        .execute(
            "INSERT INTO drift_events (item_id, agent_id, scope, path, detected_at) \
             VALUES (?1, 'claude_code', 'user', '/path', '2026-01-01T00:00:00Z')",
            params![item.to_string()],
        )
        .expect("drift insert");
    connection
        .execute(
            "INSERT INTO drift_events (item_id, agent_id, scope, path, detected_at, resolved_at, resolution) \
             VALUES (?1, 'claude_code', 'user', '/path2', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z', 'adopt')",
            params![item.to_string()],
        )
        .expect("drift insert");

    let candidates = drift_candidates(connection, 10).expect("candidates read");
    assert_eq!(candidates.len(), 1, "only unresolved drift events");
    assert_eq!(candidates[0].kind, AttentionKind::DriftEvent);
    assert_eq!(candidates[0].item_id, Some(item));
}

#[test]
fn native_deployment_candidates_returns_native_state_rows() {
    let db = open_with_all();
    let connection = db.connection();

    let item1 = seed_item(connection, "skill-aa", ItemKind::Skill, Scope::User);
    let item2 = seed_item(connection, "skill-ab", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true, true);

    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode) \
             VALUES (?1, 'claude_code', 'user', 'native', 'native')",
            params![item1.to_string()],
        )
        .expect("deployment insert");
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, mode) \
             VALUES (?1, 'claude_code', 'user', 'linked', 'symlink')",
            params![item2.to_string()],
        )
        .expect("deployment insert");

    let candidates = native_deployment_candidates(connection, 10).expect("candidates read");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].kind, AttentionKind::NativeDeployment);
    assert_eq!(candidates[0].item_id, Some(item1));
}

#[test]
fn missing_agent_candidates_returns_installed_unmanaged() {
    let db = open_with_all();
    let connection = db.connection();

    seed_agent(connection, "claude_code", true, true);
    seed_agent(connection, "codex", true, false); // installed but unmanaged
    seed_agent(connection, "omp", false, true); // not installed

    let candidates = missing_agent_candidates(connection, 10).expect("candidates read");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].kind, AttentionKind::MissingAgent);
    assert_eq!(candidates[0].agent_id, Some(AgentId::Codex));
}

#[test]
fn needs_attention_merges_all_candidate_kinds() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-ab", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true, true);
    seed_agent(connection, "codex", true, false);

    connection
        .execute(
            "INSERT INTO drift_events (item_id, agent_id, scope, path, detected_at) \
             VALUES (?1, 'claude_code', 'user', '/path', '2026-01-01T00:00:00Z')",
            params![item.to_string()],
        )
        .expect("drift insert");

    let candidates = needs_attention(connection, 50).expect("needs_attention reads");
    assert!(candidates.len() >= 2, "drift + missing agent at minimum");

    // Severity ordering: Warning before Info.
    let warning_count = candidates
        .iter()
        .filter(|c| c.severity == at_db::overview::AttentionSeverity::Warning)
        .count();
    let info_count = candidates
        .iter()
        .filter(|c| c.severity == at_db::overview::AttentionSeverity::Info)
        .count();
    assert!(warning_count > 0);
    assert!(info_count > 0);
}

#[test]
fn shadowed_skill_rows_report_effective_winner_and_navigation() {
    let db = open_with_all();
    let connection = db.connection();
    let user = seed_item(connection, "same-skill", ItemKind::Skill, Scope::User);
    let project = seed_item(connection, "same-skill", ItemKind::Skill, Scope::Project);
    let rows = at_db::overview::shadowed_skills(connection, 20).expect("shadow query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item_id, user);
    assert_eq!(rows[0].winner_scope, Scope::Project);
    let candidates = at_db::overview::shadowed_skill_candidates(connection, 20).expect("attention");
    assert_eq!(candidates[0].item_id, Some(user));
    assert_eq!(
        candidates[0]
            .navigation
            .as_ref()
            .and_then(|target| target.row_id.as_deref()),
        Some(user.to_string().as_str())
    );
    assert_ne!(user, project);
}
