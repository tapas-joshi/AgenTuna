//! Tests that every fixed deployment write site records the TA-005 sync_label
//! and evidence hashes so that project sync status reports the item in sync (not untracked).

use at_db::Database;
use at_db::deployments::{
    DeploymentHashes, DeploymentSpec, list_deployments_for_item, upsert_deployment,
};
use at_engine::project_sync::sync_record_from_rows;
use at_model::{AgentId, ItemId, ItemState, Scope, SyncLabel};

fn open_test_db() -> Database {
    let db = Database::open_in_memory().expect("open database");
    db.connection()
        .execute(
            "INSERT OR IGNORE INTO agents (id, display_name, installed, managed) \
             VALUES ('claude_code', 'Claude Code', 1, 1)",
            [],
        )
        .expect("seed agent");
    db
}

fn seed_project_item(db: &Database, slug: &str, project_id: &str) -> ItemId {
    let item_id = ItemId::new();
    db.connection()
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
             VALUES (?1, 'skill', 'project', ?2, ?3, ?3, 'present', 1)",
            rusqlite::params![item_id.to_string(), project_id, slug],
        )
        .expect("seed project item");
    item_id
}

#[test]
fn bulk_skill_deployment_reports_in_sync_status() {
    let db = open_test_db();
    let project_id = "project-bulk";
    let item_id = seed_project_item(&db, "bulk-skill", project_id);

    // Fixed site: bulk_skill::update_deployments writes using DeploymentSpec::applied
    let hashes = DeploymentHashes::in_sync("hash-bulk-1");
    let mut spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Linked,
        Some("symlink"),
        Some("/sandbox/project/skills/bulk-skill"),
        hashes,
    )
    .with_operation("op_bulk_1");
    spec = spec.with_project(project_id);
    upsert_deployment(db.connection(), &spec).expect("bulk skill deployment write");

    let rows = list_deployments_for_item(db.connection(), &item_id).expect("list deployments");
    let record = sync_record_from_rows(&rows, project_id).expect("must not be untracked");
    assert_eq!(record.label, SyncLabel::InSync);
    assert_eq!(record.evidence.project_hash.as_deref(), Some("hash-bulk-1"));
    assert_eq!(record.evidence.library_hash.as_deref(), Some("hash-bulk-1"));
    assert_eq!(record.evidence.base_hash.as_deref(), Some("hash-bulk-1"));
}

#[test]
fn mcp_server_deployment_reports_in_sync_status() {
    let db = open_test_db();
    let project_id = "project-mcp";
    let item_id = ItemId::new();
    db.connection()
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
             VALUES (?1, 'mcp_server', 'project', ?2, 'github', 'github', 'present', 1)",
            rusqlite::params![item_id.to_string(), project_id],
        )
        .expect("seed mcp item");

    // Fixed site: mcp_servers::apply_cached writes using DeploymentSpec::applied
    let hashes = DeploymentHashes::new(
        Some("store-mcp-hash"),
        Some("target-mcp-hash"),
        Some("target-mcp-hash"),
    );
    let mut spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Linked,
        Some("native"),
        Some("/sandbox/project/.claude.json"),
        hashes,
    )
    .with_operation("op_mcp_1");
    spec = spec.with_project(project_id);
    upsert_deployment(db.connection(), &spec).expect("mcp server deployment write");

    let rows = list_deployments_for_item(db.connection(), &item_id).expect("list deployments");
    let record = sync_record_from_rows(&rows, project_id).expect("must not be untracked");
    assert_eq!(record.label, SyncLabel::InSync);
    assert_eq!(
        record.evidence.project_hash.as_deref(),
        Some("store-mcp-hash")
    );
    assert_eq!(
        record.evidence.library_hash.as_deref(),
        Some("target-mcp-hash")
    );
    assert_eq!(
        record.evidence.base_hash.as_deref(),
        Some("target-mcp-hash")
    );
}

#[test]
fn skill_transfer_deployment_reports_in_sync_status() {
    let db = open_test_db();
    let project_id = "project-transfer";
    let item_id = seed_project_item(&db, "transferred-skill", project_id);

    // Fixed site: skill_transfer target row writes using DeploymentSpec::applied
    let hashes = DeploymentHashes::new(
        Some("store-transfer"),
        Some("target-transfer"),
        Some("base-transfer"),
    );
    let mut spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Linked,
        Some("symlink"),
        Some("/sandbox/project/skills/transferred-skill"),
        hashes,
    )
    .with_operation("op_transfer_1");
    spec = spec.with_project(project_id);
    upsert_deployment(db.connection(), &spec).expect("skill transfer deployment write");

    let rows = list_deployments_for_item(db.connection(), &item_id).expect("list deployments");
    let record = sync_record_from_rows(&rows, project_id).expect("must not be untracked");
    assert_eq!(record.label, SyncLabel::InSync);
    assert_eq!(
        record.evidence.project_hash.as_deref(),
        Some("store-transfer")
    );
    assert_eq!(
        record.evidence.library_hash.as_deref(),
        Some("target-transfer")
    );
    assert_eq!(record.evidence.base_hash.as_deref(), Some("base-transfer"));
}

#[test]
fn skill_disable_in_repo_deployment_reports_in_sync_status() {
    let db = open_test_db();
    let project_id = "project-disable";
    let item_id = seed_project_item(&db, "disabled-skill", project_id);

    // Fixed site: skill_transfer::skill_disable_in_repo_impl writes using DeploymentSpec::applied
    let hashes = DeploymentHashes::new(Some("store-disabled"), None, Some("base-disabled"));
    let mut spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Off,
        Some("none"),
        Some("/sandbox/project/skills/disabled-skill"),
        hashes,
    )
    .with_operation("op_disable_1");
    spec = spec.with_project(project_id);
    upsert_deployment(db.connection(), &spec).expect("skill disable deployment write");

    let rows = list_deployments_for_item(db.connection(), &item_id).expect("list deployments");
    let record = sync_record_from_rows(&rows, project_id).expect("must not be untracked");
    assert_eq!(record.label, SyncLabel::InSync);
    assert_eq!(
        record.evidence.project_hash.as_deref(),
        Some("store-disabled")
    );
    assert_eq!(record.evidence.library_hash.as_deref(), None);
    assert_eq!(record.evidence.base_hash.as_deref(), Some("base-disabled"));
}

#[test]
fn cli_mcp_toggle_deployment_reports_in_sync_status() {
    let db = open_test_db();
    let project_id = "project-cli-mcp";
    let item_id = ItemId::new();
    db.connection()
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
             VALUES (?1, 'mcp_server', 'project', ?2, 'fetch', 'fetch', 'present', 1)",
            rusqlite::params![item_id.to_string(), project_id],
        )
        .expect("seed cli mcp item");

    // Fixed site: cli::mcp::run_toggle writes using DeploymentSpec::applied
    let hashes = DeploymentHashes::new(
        Some("store-fetch"),
        Some("target-fetch"),
        Some("target-fetch"),
    );
    let mut spec = DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Linked,
        Some("native"),
        Some("/sandbox/project/.claude.json"),
        hashes,
    )
    .with_operation("op_cli_mcp_1");
    spec = spec.with_project(project_id);
    upsert_deployment(db.connection(), &spec).expect("cli mcp toggle deployment write");

    let rows = list_deployments_for_item(db.connection(), &item_id).expect("list deployments");
    let record = sync_record_from_rows(&rows, project_id).expect("must not be untracked");
    assert_eq!(record.label, SyncLabel::InSync);
    assert_eq!(record.evidence.project_hash.as_deref(), Some("store-fetch"));
    assert_eq!(
        record.evidence.library_hash.as_deref(),
        Some("target-fetch")
    );
    assert_eq!(record.evidence.base_hash.as_deref(), Some("target-fetch"));
}
