//! TA-041: deployments and locks repositories over TF-013/TF-014 tables.
//!
//! No DDL: every column and constraint comes from the shipped migrations.

mod common;

use at_db::Database;
use at_db::deployments::{
    DeploymentSpec, effective_rows, find_deployment, health_counts, list_deployments_for_agent,
    list_deployments_for_item, upsert_deployment,
};
use at_db::locks::{
    LockSpec, count_locks_for_scope, find_lock, list_locks, locks_for_item, locks_for_scope,
    remove_lock, upsert_lock,
};
use at_db::shared::{DeploymentCell, LockRow};
use at_model::{AgentId, ItemId, ItemKind, ItemState, Scope, SyncLabel};
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

fn seed_agent(connection: &Connection, id: &str, managed: bool) {
    connection
        .execute(
            "INSERT INTO agents (id, display_name, installed, managed) VALUES (?1, ?1, 1, ?2)",
            params![id, managed],
        )
        .expect("agent inserts");
}

#[test]
fn upsert_deployment_inserts_then_updates_in_place() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-a", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true);

    let spec = DeploymentSpec::new(&item, &AgentId::ClaudeCode, Scope::User, ItemState::Linked)
        .with_mode("symlink")
        .with_target_path("/home/.claude/skills/a")
        .with_hashes(Some("sh"), Some("th"), Some("bh"))
        .with_sync_label(SyncLabel::InSync);
    upsert_deployment(connection, &spec).expect("first upsert");

    // Second upsert for the same key updates state/mode/hashes and refreshes
    // updated_at; it does not insert a second row.
    let updated = DeploymentSpec::new(&item, &AgentId::ClaudeCode, Scope::User, ItemState::Off)
        .with_mode("copy")
        .with_target_path("/home/.claude/skills/a")
        .with_hashes(Some("sh2"), Some("th2"), Some("bh2"))
        .with_sync_label(SyncLabel::ProjectNewer);
    upsert_deployment(connection, &updated).expect("second upsert");

    let cells = list_deployments_for_item(connection, &item).expect("list reads");
    assert_eq!(cells.len(), 1, "upsert updates in place, not inserts");
    let cell = &cells[0];
    assert_eq!(cell.state, ItemState::Off);
    assert_eq!(cell.mode.as_deref(), Some("copy"));
    assert_eq!(cell.store_hash.as_deref(), Some("sh2"));
    assert_eq!(cell.target_hash.as_deref(), Some("th2"));
    assert_eq!(cell.baseline_hash.as_deref(), Some("bh2"));
    assert_eq!(cell.sync_label, Some(SyncLabel::ProjectNewer));
}

#[test]
fn find_deployment_returns_none_for_missing_cell() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-b", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true);

    let found = find_deployment(connection, &item, &AgentId::ClaudeCode, Scope::User, None)
        .expect("find reads");
    assert!(found.is_none());
}

#[test]
fn health_counts_returns_managed_in_sync_total() {
    let db = open_with_all();
    let connection = db.connection();

    let item1 = seed_item(connection, "skill-c", ItemKind::Skill, Scope::User);
    let item2 = seed_item(connection, "skill-d", ItemKind::Skill, Scope::User);
    let item3 = seed_item(connection, "skill-e", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true);
    seed_agent(connection, "codex", true);

    // claude_code: 2 managed (linked+off), 1 in_sync, 1 native = 4 total.
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item1, &AgentId::ClaudeCode, Scope::User, ItemState::Linked)
            .with_sync_label(SyncLabel::InSync),
    )
    .expect("upsert");
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item2, &AgentId::ClaudeCode, Scope::User, ItemState::Off),
    )
    .expect("upsert");
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item3, &AgentId::ClaudeCode, Scope::User, ItemState::Native),
    )
    .expect("upsert");

    // codex: 1 managed, 0 in_sync, 1 total.
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item1, &AgentId::Codex, Scope::User, ItemState::Linked),
    )
    .expect("upsert");

    let claude = health_counts(connection, &AgentId::ClaudeCode).expect("health reads");
    assert_eq!(claude.managed, 2, "native excluded from managed");
    assert_eq!(claude.in_sync, 1, "only linked+in_sync counts");
    assert_eq!(claude.total, 3);

    let codex = health_counts(connection, &AgentId::Codex).expect("health reads");
    assert_eq!(codex.managed, 1);
    assert_eq!(codex.in_sync, 0);
    assert_eq!(codex.total, 1);
}

#[test]
fn health_counts_unknown_agent_returns_zero() {
    let db = open_with_all();
    let connection = db.connection();

    let counts = health_counts(connection, &AgentId::Omp).expect("health reads");
    assert_eq!(counts.managed, 0);
    assert_eq!(counts.in_sync, 0);
    assert_eq!(counts.total, 0);
}

#[test]
fn effective_rows_joins_item_and_agent_in_one_statement() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-f", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true);
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item, &AgentId::ClaudeCode, Scope::User, ItemState::Linked)
            .with_sync_label(SyncLabel::InSync),
    )
    .expect("upsert");

    let rows = effective_rows(connection, &AgentId::ClaudeCode, Scope::User, None)
        .expect("effective rows read");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.item.id, item);
    assert_eq!(row.cell.state, ItemState::Linked);
    assert_eq!(row.agent_display_name.as_deref(), Some("claude_code"));
}

#[test]
fn effective_rows_filters_by_scope_and_project() {
    let db = open_with_all();
    let connection = db.connection();

    let item_user = seed_item(connection, "skill-g", ItemKind::Skill, Scope::User);
    let item_project = seed_item(connection, "skill-h", ItemKind::Skill, Scope::Project);
    seed_agent(connection, "claude_code", true);

    upsert_deployment(
        connection,
        &DeploymentSpec::new(
            &item_user,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
        ),
    )
    .expect("upsert");
    upsert_deployment(
        connection,
        &DeploymentSpec::new(
            &item_project,
            &AgentId::ClaudeCode,
            Scope::Project,
            ItemState::Linked,
        )
        .with_project("proj_1"),
    )
    .expect("upsert");

    let user_rows = effective_rows(connection, &AgentId::ClaudeCode, Scope::User, None)
        .expect("effective rows read");
    assert_eq!(user_rows.len(), 1);
    assert_eq!(user_rows[0].item.id, item_user);

    let project_rows = effective_rows(
        connection,
        &AgentId::ClaudeCode,
        Scope::Project,
        Some("proj_1"),
    )
    .expect("effective rows read");
    assert_eq!(project_rows.len(), 1);
    assert_eq!(project_rows[0].item.id, item_project);
}

#[test]
fn list_deployments_for_agent_returns_all_scopes() {
    let db = open_with_all();
    let connection = db.connection();

    let item1 = seed_item(connection, "skill-i", ItemKind::Skill, Scope::User);
    let item2 = seed_item(connection, "skill-j", ItemKind::Skill, Scope::Project);
    seed_agent(connection, "claude_code", true);

    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item1, &AgentId::ClaudeCode, Scope::User, ItemState::Linked),
    )
    .expect("upsert");
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item2, &AgentId::ClaudeCode, Scope::Project, ItemState::Off)
            .with_project("proj_1"),
    )
    .expect("upsert");

    let cells = list_deployments_for_agent(connection, &AgentId::ClaudeCode).expect("list reads");
    assert_eq!(cells.len(), 2);
    assert!(cells.iter().any(|c| c.scope == Scope::User));
    assert!(cells.iter().any(|c| c.scope == Scope::Project));
}

// ---- locks ----

#[test]
fn upsert_lock_inserts_then_updates_in_place() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-k", ItemKind::Skill, Scope::Global);

    let spec = LockSpec {
        item_id: &item,
        scope: Scope::Global,
        reason: Some("first"),
        locked_by: Some("user"),
    };
    upsert_lock(connection, &spec).expect("first upsert");

    let updated = LockSpec {
        item_id: &item,
        scope: Scope::Global,
        reason: Some("second"),
        locked_by: Some("admin"),
    };
    upsert_lock(connection, &updated).expect("second upsert");

    let locks = locks_for_item(connection, &item).expect("locks read");
    assert_eq!(locks.len(), 1, "upsert updates in place");
    assert_eq!(locks[0].reason.as_deref(), Some("second"));
    assert_eq!(locks[0].locked_by.as_deref(), Some("admin"));
}

#[test]
fn find_lock_returns_none_for_missing() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-l", ItemKind::Skill, Scope::Global);
    let found = find_lock(connection, &item, Scope::Global).expect("find reads");
    assert!(found.is_none());
}

#[test]
fn remove_lock_deletes_and_returns_true() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-m", ItemKind::Skill, Scope::Global);
    upsert_lock(
        connection,
        &LockSpec {
            item_id: &item,
            scope: Scope::Global,
            reason: None,
            locked_by: None,
        },
    )
    .expect("upsert");

    let removed = remove_lock(connection, &item, Scope::Global).expect("remove");
    assert!(removed);

    let removed_again = remove_lock(connection, &item, Scope::Global).expect("remove");
    assert!(!removed_again, "second remove finds nothing");
}

#[test]
fn locks_for_scope_and_count_locks_for_scope() {
    let db = open_with_all();
    let connection = db.connection();

    let item1 = seed_item(connection, "skill-n", ItemKind::Skill, Scope::Global);
    let item2 = seed_item(connection, "skill-o", ItemKind::Skill, Scope::Global);
    let item3 = seed_item(connection, "skill-p", ItemKind::Skill, Scope::User);

    for (item, scope) in [
        (&item1, Scope::Global),
        (&item2, Scope::Global),
        (&item3, Scope::User),
    ] {
        upsert_lock(
            connection,
            &LockSpec {
                item_id: item,
                scope,
                reason: None,
                locked_by: None,
            },
        )
        .expect("upsert");
    }

    let global_locks = locks_for_scope(connection, Scope::Global).expect("locks read");
    assert_eq!(global_locks.len(), 2);

    let user_locks = locks_for_scope(connection, Scope::User).expect("locks read");
    assert_eq!(user_locks.len(), 1);

    assert_eq!(
        count_locks_for_scope(connection, Scope::Global).expect("count"),
        2
    );
    assert_eq!(
        count_locks_for_scope(connection, Scope::User).expect("count"),
        1
    );
    assert_eq!(
        count_locks_for_scope(connection, Scope::Project).expect("count"),
        0
    );
}

#[test]
fn list_locks_returns_all_rows() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-q", ItemKind::Skill, Scope::Global);
    upsert_lock(
        connection,
        &LockSpec {
            item_id: &item,
            scope: Scope::Global,
            reason: Some("r"),
            locked_by: Some("u"),
        },
    )
    .expect("upsert");

    let all = list_locks(connection).expect("list reads");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].item_id, item);
}

#[test]
fn deployment_cell_constructible_from_db_row() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-r", ItemKind::Skill, Scope::User);
    seed_agent(connection, "claude_code", true);
    upsert_deployment(
        connection,
        &DeploymentSpec::new(&item, &AgentId::ClaudeCode, Scope::User, ItemState::Linked)
            .with_mode("symlink")
            .with_sync_label(SyncLabel::InSync),
    )
    .expect("upsert");

    let cells = list_deployments_for_item(connection, &item).expect("list reads");
    assert_eq!(cells.len(), 1);
    let cell: &DeploymentCell = &cells[0];
    assert_eq!(cell.item_id, item);
    assert_eq!(cell.agent_id, AgentId::ClaudeCode);
    assert_eq!(cell.scope, Scope::User);
    assert_eq!(cell.state, ItemState::Linked);
}

#[test]
fn lock_row_constructible_from_db_row() {
    let db = open_with_all();
    let connection = db.connection();

    let item = seed_item(connection, "skill-s", ItemKind::Skill, Scope::Global);
    upsert_lock(
        connection,
        &LockSpec {
            item_id: &item,
            scope: Scope::Global,
            reason: Some("test"),
            locked_by: Some("user"),
        },
    )
    .expect("upsert");

    let locks = locks_for_item(connection, &item).expect("locks read");
    assert_eq!(locks.len(), 1);
    let lock: &LockRow = &locks[0];
    assert_eq!(lock.item_id, item);
    assert_eq!(lock.scope, Scope::Global);
    assert_eq!(lock.reason.as_deref(), Some("test"));
}
