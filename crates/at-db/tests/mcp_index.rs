//! Behaviour tests for the MCP list projection.

mod common;

use at_db::mcp_index::{McpCellState, mcp_list_counts, mcp_list_rows};
use at_model::{ItemId, Scope};

use rusqlite::params;
#[test]
fn list_projects_latest_health_and_distinguishes_uninstalled_from_absent() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection().execute("INSERT INTO agents(id,display_name,installed) VALUES ('codex','Codex',1),('cursor','Cursor',0)", []).expect("insert agents");
    let item = ItemId::new();
    db.connection().execute(
        "INSERT INTO items(id,kind,scope,slug,name,state,managed) VALUES (?1,'mcp_server','global','svc','Service','active',1)",
        [item.to_string()],
    ).expect("insert server");
    db.connection().execute(
        "INSERT INTO health_checks(item_id,scope,server_name,checked_at,status,trigger) VALUES (?1,'global','Service',1,'failed','manual'),(?1,'global','Service',2,'ok','manual')",
        [item.to_string()],
    ).expect("insert health history");
    let rows = mcp_list_rows(db.connection(), Some(Scope::Global), None).expect("list servers");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].latest_health.as_deref(), Some("ok"));
    assert!(matches!(rows[0].agents[0].state, McpCellState::None));
    assert!(matches!(
        rows[0].agents[1].state,
        McpCellState::NotInstalled
    ));
}

#[test]
fn list_includes_native_only_and_uses_same_set_for_counts() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('omp','omp',1)",
            [],
        )
        .expect("insert agent");
    db.connection().execute(
        "INSERT INTO native_items(id,agent_id,scope,kind,name,path,status) VALUES ('native-mcp','omp','user','mcp_server','Native Service','/sandbox/mcp/native.json','new')",
        [],
    ).expect("insert native server");
    let rows =
        mcp_list_rows(db.connection(), Some(Scope::User), None).expect("list native servers");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].native_only);
    assert_eq!(
        rows[0].source_path.as_deref(),
        Some("/sandbox/mcp/native.json")
    );
    assert_eq!(rows[0].agents.len(), 1);
    assert!(matches!(
        rows[0].agents[0].state,
        McpCellState::Native { .. }
    ));
    assert_eq!(
        mcp_list_counts(db.connection(), Some(Scope::User), None)
            .expect("count chips")
            .all,
        rows.len()
    );
}

#[test]
fn global_lock_marks_every_agent_cell_and_excludes_failure_chip() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('codex','Codex',1),('cursor','Cursor',1)",
            [],
        )
        .expect("insert agents");
    let locked = ItemId::new();
    let failed = ItemId::new();
    for (id, slug, name) in [(&locked, "locked", "Locked"), (&failed, "failed", "Failed")] {
        db.connection()
            .execute(
                "INSERT INTO items(id,kind,scope,slug,name,state,managed) VALUES (?1,'mcp_server','global',?2,?3,'active',1)",
                params![id.to_string(), slug, name],
            )
            .expect("insert server");
        db.connection()
            .execute(
                "INSERT INTO health_checks(item_id,scope,server_name,checked_at,status,trigger) VALUES (?1,'global',?2,1,'failed','manual')",
                params![id.to_string(), name],
            )
            .expect("insert failed health");
    }
    db.connection()
        .execute(
            "INSERT INTO locks(item_id,scope,locked_by) VALUES (?1,'global','Global')",
            [locked.to_string()],
        )
        .expect("lock global server");
    let rows = mcp_list_rows(db.connection(), Some(Scope::Global), None).expect("list servers");
    let locked_row = rows
        .iter()
        .find(|row| row.display_name == "Locked")
        .expect("locked row");
    assert_eq!(locked_row.locked_by, Some(Scope::Global));
    assert!(
        locked_row
            .agents
            .iter()
            .all(|cell| matches!(cell.state, McpCellState::Locked))
    );
    let counts = mcp_list_counts(db.connection(), Some(Scope::Global), None).expect("list counts");
    assert_eq!(counts.all, 2);
    assert_eq!(counts.failing, 1);
}
