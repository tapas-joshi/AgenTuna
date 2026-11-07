//! Snapshot tests for the shared row structs (TF-021).
//!
//! Every row struct serializes to stable snake_case JSON. A renamed field
//! breaks the snapshot and fails this test — that is the point: the wire
//! contract is the field names.

use at_db::shared::{
    AgentRef, BackupRow, DeploymentCell, DeploymentMatrixRow, DriftEventRow, ItemRow, LockRow,
    NativeItemRow, OperationFileRow, OperationRow, ProjectRef,
};
use at_model::{AgentId, ItemId, ItemKind, ItemState, LinkMode, Scope, SyncLabel};
use serde_json::json;

fn item_id() -> ItemId {
    "itm_0198f3a2-4c5d-7e8f-9a0b-1c2d3e4f5a6b"
        .parse()
        .expect("valid item id")
}

fn agent_id() -> AgentId {
    AgentId::ClaudeCode
}

#[test]
fn item_row_serializes_to_stable_snake_case() {
    let row = ItemRow {
        id: item_id(),
        kind: ItemKind::Skill,
        scope: Scope::User,
        project_id: None,
        slug: "my-skill".to_owned(),
        name: "My Skill".to_owned(),
        source_path: Some("/store/skills/my-skill".to_owned()),
        link_target: None,
        content_hash: Some("abc123".to_owned()),
        state: Some("active".to_owned()),
        managed: true,
        source_id: None,
        enabled_default: Some(true),
        tags_json: Some("[\"rust\"]".to_owned()),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-02T00:00:00Z".to_owned(),
        last_scanned_at: Some("2026-01-02T00:00:00Z".to_owned()),
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(
        json,
        json!({
            "id": "itm_0198f3a2-4c5d-7e8f-9a0b-1c2d3e4f5a6b",
            "kind": "skill",
            "scope": "user",
            "project_id": null,
            "slug": "my-skill",
            "name": "My Skill",
            "source_path": "/store/skills/my-skill",
            "link_target": null,
            "content_hash": "abc123",
            "state": "active",
            "managed": true,
            "source_id": null,
            "enabled_default": true,
            "tags_json": "[\"rust\"]",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "last_scanned_at": "2026-01-02T00:00:00Z"
        })
    );
}

#[test]
fn project_ref_serializes_to_stable_snake_case() {
    let row = ProjectRef {
        id: "proj_1".to_owned(),
        name: Some("My Project".to_owned()),
        path: "/repo".to_owned(),
        source_root: Some("/repos".to_owned()),
        config_hash: Some("hash".to_owned()),
        last_opened_at: Some("2026-01-01T00:00:00Z".to_owned()),
        last_used_at: Some("2026-01-01T00:00:00Z".to_owned()),
        hidden: false,
        is_global: false,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["id"], "proj_1");
    assert_eq!(json["name"], "My Project");
    assert_eq!(json["is_global"], false);
}

#[test]
fn agent_ref_serializes_to_stable_snake_case() {
    let row = AgentRef {
        id: agent_id(),
        display_name: Some("Claude Code".to_owned()),
        kind: Some("claude".to_owned()),
        version: Some("1.0".to_owned()),
        installed: true,
        managed: true,
        detected_at: Some("2026-01-01T00:00:00Z".to_owned()),
        settings_json: None,
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["id"], "claude_code");
    assert_eq!(json["display_name"], "Claude Code");
    assert_eq!(json["managed"], true);
}

#[test]
fn deployment_cell_serializes_to_stable_snake_case() {
    let cell = DeploymentCell {
        item_id: item_id(),
        agent_id: agent_id(),
        scope: Scope::User,
        project_id: None,
        state: ItemState::Linked,
        mode: Some("symlink".to_owned()),
        link_mode: Some(LinkMode::Link),
        target_path: Some("/home/.claude/skills/my-skill".to_owned()),
        link_target: Some("/store/skills/my-skill".to_owned()),
        store_hash: Some("store".to_owned()),
        target_hash: Some("target".to_owned()),
        baseline_hash: Some("baseline".to_owned()),
        sync_label: Some(SyncLabel::InSync),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        last_verified_at: Some("2026-01-01T00:00:00Z".to_owned()),
        operation_id: Some("op_1".to_owned()),
    };
    let json = serde_json::to_value(&cell).expect("serializes");
    assert_eq!(json["state"], "linked");
    assert_eq!(json["mode"], "symlink");
    assert_eq!(json["link_mode"], "link");
    assert_eq!(json["sync_label"], "in_sync");
    assert_eq!(json["store_hash"], "store");
    assert_eq!(json["target_hash"], "target");
    assert_eq!(json["baseline_hash"], "baseline");
}

#[test]
fn deployment_matrix_row_serializes_to_stable_snake_case() {
    let row = DeploymentMatrixRow {
        item: ItemRow {
            id: item_id(),
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: "my-skill".to_owned(),
            name: "My Skill".to_owned(),
            source_path: None,
            link_target: None,
            content_hash: None,
            state: None,
            managed: true,
            source_id: None,
            enabled_default: None,
            tags_json: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            last_scanned_at: None,
        },
        agent: AgentRef {
            id: agent_id(),
            display_name: None,
            kind: None,
            version: None,
            installed: true,
            managed: true,
            detected_at: None,
            settings_json: None,
        },
        scope: Scope::User,
        project_id: None,
        cell: None,
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["scope"], "user");
    assert_eq!(json["cell"], serde_json::Value::Null);
}

#[test]
fn backup_row_serializes_to_stable_snake_case() {
    let row = BackupRow {
        id: "bak_1".to_owned(),
        operation_id: Some("op_1".to_owned()),
        original_path: Some("/path".to_owned()),
        backup_path: Some("/backup".to_owned()),
        content_hash: Some("hash".to_owned()),
        size: Some(1024),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        pinned: false,
        expires_at: None,
        restore_count: 0,
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["id"], "bak_1");
    assert_eq!(json["pinned"], false);
    assert_eq!(json["restore_count"], 0);
}

#[test]
fn operation_row_serializes_to_stable_snake_case() {
    let row = OperationRow {
        id: "op_1".to_owned(),
        kind: None,
        scope: Some(Scope::User),
        actor: None,
        status: None,
        summary: Some("Deploy skill".to_owned()),
        error_kind: None,
        started_at: "2026-01-01T00:00:00Z".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        finished_at: None,
        correlation_id: None,
        undo_of: None,
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["id"], "op_1");
    assert_eq!(json["scope"], "user");
    assert_eq!(json["summary"], "Deploy skill");
}

#[test]
fn operation_file_row_serializes_to_stable_snake_case() {
    let row = OperationFileRow {
        id: 1,
        operation_id: "op_1".to_owned(),
        path: Some("/path".to_owned()),
        action: None,
        before_hash: None,
        after_hash: Some("hash".to_owned()),
        backup_id: None,
        bytes_before: None,
        bytes_after: Some(100),
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["operation_id"], "op_1");
    assert_eq!(json["bytes_after"], 100);
}

#[test]
fn lock_row_serializes_to_stable_snake_case() {
    let row = LockRow {
        id: 1,
        item_id: item_id(),
        scope: Scope::Global,
        reason: Some("reason".to_owned()),
        locked_by: Some("user".to_owned()),
        locked_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["scope"], "global");
    assert_eq!(json["reason"], "reason");
}

#[test]
fn native_item_row_serializes_to_stable_snake_case() {
    let row = NativeItemRow {
        id: "nat_1".to_owned(),
        agent_id: agent_id(),
        scope: Scope::User,
        kind: ItemKind::Skill,
        name: "Native Skill".to_owned(),
        path: "/path".to_owned(),
        content_hash: None,
        first_seen_at: "2026-01-01T00:00:00Z".to_owned(),
        last_seen_at: "2026-01-01T00:00:00Z".to_owned(),
        adopted_item_id: None,
        status: "new".to_owned(),
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["status"], "new");
    assert_eq!(json["kind"], "skill");
}

#[test]
fn drift_event_row_serializes_to_stable_snake_case() {
    let row = DriftEventRow {
        id: 1,
        item_id: Some(item_id()),
        agent_id: agent_id(),
        scope: Scope::User,
        path: "/path".to_owned(),
        expected_hash: None,
        actual_hash: Some("hash".to_owned()),
        detected_at: "2026-01-01T00:00:00Z".to_owned(),
        resolved_at: None,
        resolution: None,
        backup_id: None,
    };
    let json = serde_json::to_value(&row).expect("serializes");
    assert_eq!(json["path"], "/path");
    assert_eq!(json["resolved_at"], serde_json::Value::Null);
}

/// Repository roots one workspace level above this crate.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a workspace parent")
        .to_path_buf()
}

/// Every `.rs` file below `root`, recursively (sorted for a stable failure message).
fn rust_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort();
    found
}

/// TF-021 AC3: `at-db` owns the shared row structs and the deployment matrix only.
///
/// The domain list queries named by the story live in the repositories that own them:
/// TS-001's `list_skills` in `skills_index`, TM-033's `mcp_list_rows` behind the MCP list
/// projection, TA-006's `at_engine::effective::query` for effective config, and the
/// app-facing `skills_list` command. `at-db` must not grow a same-named copy of any of them.
#[test]
fn domain_list_queries_do_not_live_in_at_db() {
    let root = workspace_root();
    let forbidden = ["skills_list", "mcp_list", "effective_rows"];
    let mut offenders = Vec::new();
    for path in rust_sources(&root.join("at-db").join("src")) {
        let source = std::fs::read_to_string(&path).expect("at-db source is UTF-8");
        for name in forbidden {
            // Only a definition counts: `fn <name>(` or `fn <name><…>`.
            if source.contains(&format!("fn {name}(")) || source.contains(&format!("fn {name}<")) {
                offenders.push(format!("{} defines fn {name}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "domain list queries belong to their repositories, not at-db: {offenders:#?}"
    );

    let owners = [
        ("at-db/src/skills_index.rs", "fn list_skills("),
        ("at-db/src/mcp_index.rs", "fn mcp_list_rows("),
        ("at-engine/src/effective.rs", "fn query("),
        ("at-app/src/app/skills_list.rs", "fn skills_list("),
    ];
    for (relative, symbol) in owners {
        let path = root.join(relative);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            source.contains(symbol),
            "{} must own {symbol}",
            path.display()
        );
    }
}
