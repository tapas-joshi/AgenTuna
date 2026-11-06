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

#[test]
fn nine_server_fixture_matches_row_and_chip_counts() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection().execute(
        "INSERT INTO agents(id,display_name,installed) VALUES ('claude_code','Claude Code',1),('codex','Codex',1),('omp','omp',1),('cursor','Cursor',1)",
        [],
    ).expect("insert agents");
    for index in 0..6 {
        let id = ItemId::new();
        db.connection().execute(
            "INSERT INTO items(id,kind,scope,slug,name,state,managed) VALUES (?1,'mcp_server','global',?2,?3,'active',1)",
            params![id.to_string(), format!("managed-{index}"), format!("Managed {index}")],
        ).expect("insert managed server");
    }
    // One failing managed server: latest check failed.
    let failing = ItemId::new();
    db.connection().execute(
        "INSERT INTO items(id,kind,scope,slug,name,state,managed) VALUES (?1,'mcp_server','global','failing','Failing','active',1)",
        [failing.to_string()],
    ).expect("insert failing server");
    db.connection().execute(
        "INSERT INTO health_checks(item_id,scope,server_name,checked_at,status,trigger) VALUES (?1,'global','Failing',1,'ok','manual'),(?1,'global','Failing',2,'failed','manual')",
        [failing.to_string()],
    ).expect("insert failing health");
    // Two native-only discoveries.
    db.connection().execute(
        "INSERT INTO native_items(id,agent_id,scope,kind,name,path,status) VALUES ('native-a','codex','user','mcp_server','Native A','/sandbox/codex/native-a.json','new'),('native-b','omp','user','mcp_server','Native B','/sandbox/omp/native-b.json','new')",
        [],
    ).expect("insert native servers");

    let rows = mcp_list_rows(db.connection(), None, None).expect("list rows");
    assert_eq!(rows.len(), 9, "7 managed + 2 native-only rows");
    assert_eq!(rows.iter().filter(|row| row.native_only).count(), 2);
    let counts = mcp_list_counts(db.connection(), None, None).expect("chip counts");
    assert_eq!(counts.all, 9);
    assert_eq!(counts.not_in_agentuna, 2);
    assert_eq!(counts.failing, 1);
    // Native-only rows sort last, name ascending within the same group.
    assert!(rows.iter().rev().take(2).all(|row| row.native_only));
}

#[test]
fn transport_subtitle_comes_from_the_stored_command_or_url() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('claude_code','Claude Code',1)",
            [],
        )
        .expect("insert agent");
    let stdio = ItemId::new();
    let http = ItemId::new();
    db.connection().execute(
        "INSERT INTO items(id,kind,scope,slug,name,state,managed,tags_json) VALUES (?1,'mcp_server','user','github','GitHub','active',1,?2)",
        params![stdio.to_string(), r#"{"mcp_command":"/usr/local/bin/npx"}"#],
    ).expect("insert stdio server");
    db.connection().execute(
        "INSERT INTO items(id,kind,scope,slug,name,state,managed,tags_json) VALUES (?1,'mcp_server','user','docs','Docs','active',1,?2)",
        params![http.to_string(), r#"{"mcp_url":"https://user@mcp.example.com:8443/docs?x=1"}"#],
    ).expect("insert http server");
    let rows = mcp_list_rows(db.connection(), Some(Scope::User), None).expect("list rows");
    let by_name = |name: &str| {
        rows.iter()
            .find(|row| row.display_name == name)
            .expect("row")
    };
    assert_eq!(by_name("GitHub").transport_subtitle.as_deref(), Some("npx"));
    assert_eq!(
        by_name("Docs").transport_subtitle.as_deref(),
        Some("mcp.example.com")
    );
}

#[test]
fn three_hundred_health_rows_still_yield_one_row_through_the_index() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection()
        .execute(
            "INSERT INTO agents(id,display_name,installed) VALUES ('claude_code','Claude Code',1)",
            [],
        )
        .expect("insert agent");
    let item = ItemId::new();
    db.connection().execute(
        "INSERT INTO items(id,kind,scope,slug,name,state,managed) VALUES (?1,'mcp_server','user','svc','Service','active',1)",
        [item.to_string()],
    ).expect("insert server");
    let mut batch = String::from("BEGIN;\n");
    for tick in 0..300 {
        batch.push_str(&format!(
            "INSERT INTO health_checks(item_id,scope,server_name,checked_at,status,trigger) VALUES ('{item}','user','Service',{tick},'ok','scheduled');\n"
        ));
    }
    batch.push_str("COMMIT;");
    db.connection()
        .execute_batch(&batch)
        .expect("insert history");

    let rows = mcp_list_rows(db.connection(), Some(Scope::User), None).expect("list rows");
    assert_eq!(rows.len(), 1, "history never fans out the projection");
    assert_eq!(rows[0].latest_health.as_deref(), Some("ok"));

    let plan: Vec<String> = db.connection()
        .prepare("EXPLAIN QUERY PLAN SELECT h.status FROM health_checks h WHERE h.item_id=?1 ORDER BY h.checked_at DESC, h.id DESC LIMIT 1")
        .expect("prepare plan")
        .query_map([item.to_string()], |row| row.get::<_, String>(3))
        .expect("run plan")
        .collect::<Result<_, _>>()
        .expect("collect plan");
    let detail = plan.join(" ");
    assert!(
        detail.contains("idx_health_checks_item_checked"),
        "plan must use the (item_id, checked_at) index, got: {detail}"
    );
}

#[test]
fn five_hundred_servers_by_four_agents_return_within_the_ci_bound() {
    let db = common::open_with(common::ALL_VERSIONS);
    db.connection().execute(
        "INSERT INTO agents(id,display_name,installed) VALUES ('claude_code','Claude Code',1),('codex','Codex',1),('omp','omp',1),('cursor','Cursor',0)",
        [],
    ).expect("insert agents");
    let mut batch = String::from("BEGIN;\n");
    for index in 0..500 {
        let id = ItemId::new();
        batch.push_str(&format!(
            "INSERT INTO items(id,kind,scope,slug,name,state,managed,tags_json) VALUES ('{id}','mcp_server','global','svc-{index:03}','Service {index:03}','active',1,'{{\"mcp_command\":\"uvx\"}}');\n"
        ));
    }
    batch.push_str("COMMIT;");
    db.connection().execute_batch(&batch).expect("seed servers");

    let started = std::time::Instant::now();
    let rows = mcp_list_rows(db.connection(), None, None).expect("list 500 servers");
    let elapsed = started.elapsed();
    assert_eq!(rows.len(), 500);
    assert_eq!(rows[0].agents.len(), 4, "one cell per detected agent");
    assert_eq!(
        rows[0].transport_subtitle.as_deref(),
        Some("uvx"),
        "the fixture stores a command subtitle"
    );
    // Generous CI bound: the dev Mac measures in tens of milliseconds.
    assert!(
        elapsed < std::time::Duration::from_millis(2_000),
        "mcp_list_rows over 500 servers x 4 agents took {elapsed:?}"
    );
}
