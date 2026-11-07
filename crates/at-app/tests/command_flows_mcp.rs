//! GUI-path MCP server flows through the real at-app command bodies.
//!
//! The add/edit path plans through the shared MCP planner, renders the same native files the
//! adapters write, and applies the reviewed ChangeSet. These tests assert on the native file
//! bytes (format-preserving JSON/TOML edits), the index rows, refusals and drift.

mod support;

use at_app::app::flow_harness;
use at_app::app::mcp_form::{McpFormValue, McpPlanPreviewRequest, McpSaveRequest};
use at_app::app::mcp_servers::McpServerActionRequest;
use at_model::{AgentId, ItemKind, Scope};
use support::{Sandbox, TestResult};

/// Counts the rows of one table.
fn count(database: &at_db::Database, table: &str) -> i64 {
    database
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count query")
}

/// A stdio add request for one agent.
fn stdio_request(name: &str, agent: &str) -> McpPlanPreviewRequest {
    McpPlanPreviewRequest {
        name: name.to_owned(),
        transport: "stdio".to_owned(),
        command: "npx".to_owned(),
        args: vec!["-y".to_owned(), "demo-server".to_owned()],
        url: String::new(),
        headers: Vec::new(),
        env: vec![McpFormValue {
            name: "DEMO_TOKEN".to_owned(),
            value: "${DEMO_TOKEN}".to_owned(),
        }],
        scope: "user".to_owned(),
        agents: vec![agent.to_owned()],
        project_id: None,
        item_id: None,
    }
}

/// The single `mcp_server` item id in the index.
fn mcp_item_id(database: &at_db::Database) -> String {
    database
        .connection()
        .query_row(
            "SELECT id FROM items WHERE kind = 'mcp_server'",
            [],
            |row| row.get(0),
        )
        .expect("mcp item row")
}

#[test]
fn adding_a_server_preserves_unrelated_json_and_writes_no_literal_secret() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let native = sandbox.agent_path(&AgentId::ClaudeCode, ItemKind::McpServer, Scope::User)?;
    if let Some(parent) = native.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A user-owned comment, an unrelated key and an unrelated server must all survive.
    std::fs::write(
        &native,
        b"{\n  // kept by hand\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other\" }\n  }\n}\n",
    )?;

    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.path == native.display().to_string()),
        "the native config is in the reviewed plan: {:?}",
        preview.files
    );
    let operations_before = count(&database, "operations");
    let added = flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id.clone(),
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;
    assert!(added.files_changed >= 1);

    let text = std::fs::read_to_string(&native)?;
    assert!(
        text.contains("// kept by hand"),
        "the comment survives: {text}"
    );
    assert!(
        text.contains("\"theme\""),
        "the unrelated key survives: {text}"
    );
    assert!(
        text.contains("other"),
        "the unrelated server survives: {text}"
    );
    assert!(
        text.contains("demo-server"),
        "the new server is present: {text}"
    );
    // A `${VAR}` reference is never expanded into a literal. An agent whose launch policy for
    // secret injection is unverified gets the field dropped, and the preview must say so.
    if !text.contains("${DEMO_TOKEN}") {
        assert!(
            preview
                .warnings
                .iter()
                .any(|warning| warning.contains("DEMO_TOKEN")),
            "a refused secret must be disclosed in the reviewed plan: {:?}",
            preview.warnings
        );
    }
    assert!(
        !text.contains("sk-live") && !text.contains("hunter2"),
        "no plaintext token reaches the config: {text}"
    );

    assert_eq!(count(&database, "items"), 1, "one canonical server item");
    assert_eq!(count(&database, "deployments"), 1, "one native cell");
    // Two ChangeSets are journaled per save (canonical store, then native documents) on top of
    // the single operation the reviewed scope setup recorded.
    assert_eq!(
        count(&database, "operations"),
        operations_before + 2,
        "the store and native phases are journaled"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the add refreshed the deployment baseline"
    );

    // ---- edit and rename ---------------------------------------------------------------
    let store = sandbox.layout(Scope::User)?.mcp_file();
    let before_edit = std::fs::read(&store)?;
    let item_id = mcp_item_id(&database);
    let mut rename = stdio_request("Renamed Server", "claude_code");
    rename.item_id = Some(item_id.clone());
    let preview = flow_harness::mcp_plan_preview(&state, rename)?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;

    let renamed = std::fs::read_to_string(&store)?;
    let parsed: serde_json::Value = serde_json::from_str(&renamed)?;
    let keys = parsed["servers"]
        .as_object()
        .map(|servers| servers.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert_eq!(
        keys,
        vec!["renamed-server".to_owned()],
        "the renamed entry replaces the old key: {renamed}"
    );
    // A rename rewrites the renamed entry; every unrelated line of the store document stays.
    let diff = at_fs::diff::unified("store", Some(&before_edit), renamed.as_bytes());
    assert!(
        diff.lines()
            .all(|line| !line.starts_with('-') || !line.contains("schema_version")),
        "unrelated store keys must not be rewritten: {diff}"
    );
    let native_text = std::fs::read_to_string(&native)?;
    assert!(native_text.contains("// kept by hand"));
    assert!(native_text.contains("\"theme\""));
    assert!(
        native_text.contains("\"renamed-server\":"),
        "the renamed key is present: {native_text}"
    );
    assert!(
        !native_text.contains("\"demo-server\":"),
        "the old server key is gone (the package name in args is not a key): {native_text}"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the rename refreshed the baseline"
    );
    Ok(())
}

#[test]
fn a_stale_preview_is_refused_and_changes_nothing() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let native = sandbox.agent_path(&AgentId::ClaudeCode, ItemKind::McpServer, Scope::User)?;
    if let Some(parent) = native.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&native, b"{\n  \"theme\": \"dark\"\n}\n")?;

    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    // An external writer touches the reviewed file after the user reviewed the plan.
    std::fs::write(&native, b"{\n  \"theme\": \"light\"\n}\n")?;
    let operations_before = count(&database, "operations");
    let before = sandbox.snapshot()?;

    let error = flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id.clone(),
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )
    .expect_err("a file changed after review is refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::ExternalWriteConflict,
        "{error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a stale MCP preview");
    assert_eq!(count(&database, "items"), 0, "nothing was indexed");
    assert_eq!(
        count(&database, "operations"),
        operations_before,
        "nothing was journaled"
    );

    // The reviewed plan is single-use: replaying it after a successful save is refused too.
    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id.clone(),
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;
    let after = sandbox.snapshot()?;
    let replay = flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )
    .expect_err("an applied preview is single-use");
    assert_eq!(
        replay.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument,
        "{replay:?}"
    );
    after.assert_unchanged(&sandbox.snapshot()?, "a replayed MCP preview");
    Ok(())
}

#[test]
fn an_unsupported_field_must_be_acknowledged_before_saving() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let mut request = McpPlanPreviewRequest {
        name: "Http Server".to_owned(),
        transport: "http".to_owned(),
        command: String::new(),
        args: Vec::new(),
        url: "https://example.invalid/mcp".to_owned(),
        headers: vec![McpFormValue {
            name: "X-Api-Key".to_owned(),
            value: "${DEMO_TOKEN}".to_owned(),
        }],
        env: Vec::new(),
        scope: "user".to_owned(),
        agents: vec!["codex".to_owned()],
        project_id: None,
        item_id: None,
    };
    request.headers.push(McpFormValue {
        name: "X-Trace".to_owned(),
        value: "on".to_owned(),
    });
    let preview = flow_harness::mcp_plan_preview(&state, request)?;
    assert!(
        preview
            .unsupported_fields
            .iter()
            .any(|field| field.agent == "codex" && field.fields.iter().any(|f| f == "headers")),
        "codex drops HTTP headers: {:?}",
        preview.unsupported_fields
    );
    let before = sandbox.snapshot()?;
    let error = flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id.clone(),
            selected_agents: vec!["codex".to_owned()],
            acknowledge_unsupported: false,
        },
    )
    .expect_err("an unacknowledged drop is refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument,
        "{error:?}"
    );
    assert!(
        error
            .message
            .contains("confirm the fields this plan will drop"),
        "{error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "an unacknowledged field drop");

    // After the explicit acknowledgement the same reviewed plan applies.
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["codex".to_owned()],
            acknowledge_unsupported: true,
        },
    )?;
    let native = sandbox.agent_path(&AgentId::Codex, ItemKind::McpServer, Scope::User)?;
    let text = std::fs::read_to_string(&native)?;
    assert!(text.contains("http-server"), "{text}");
    assert!(
        !text.contains("X-Api-Key"),
        "the dropped header is not rendered as if supported: {text}"
    );
    assert!(!text.contains("sk-"));
    Ok(())
}

#[test]
fn a_server_can_be_disabled_re_enabled_and_deleted() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let native = sandbox.agent_path(&AgentId::ClaudeCode, ItemKind::McpServer, Scope::User)?;
    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;
    let item_id = mcp_item_id(&database);
    let request = |apply: bool, preview_id: Option<String>| McpServerActionRequest {
        item_id: item_id.clone(),
        scope: "user".to_owned(),
        project_id: None,
        agent: Some("claude_code".to_owned()),
        in_repo: false,
        apply,
        preview_id,
    };

    // Disable: reviewed, then applied.
    let preview = flow_harness::mcp_server_disable(&state, request(false, None))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Preview { preview } = preview else {
        panic!("disable must preview first");
    };
    let disabled =
        flow_harness::mcp_server_disable(&state, request(true, Some(preview.preview_id.clone())))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Applied { .. } = disabled else {
        panic!("disable must apply the reviewed plan");
    };
    let text = std::fs::read_to_string(&native)?;
    assert!(
        !text.contains("demo-server"),
        "disable removes the native entry: {text}"
    );
    let cell_state: String = database.connection().query_row(
        "SELECT state FROM deployments WHERE item_id = ?1",
        [item_id.clone()],
        |row| row.get(0),
    )?;
    assert_eq!(cell_state, "off");

    // Enable: the reviewed plan puts the entry back.
    let preview = flow_harness::mcp_server_enable(&state, request(false, None))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Preview { preview } = preview else {
        panic!("enable must preview first");
    };
    flow_harness::mcp_server_enable(&state, request(true, Some(preview.preview_id)))?;
    let text = std::fs::read_to_string(&native)?;
    assert!(
        text.contains("demo-server"),
        "enable restores the entry: {text}"
    );

    // Delete: the item, its store definition and the native entry all go.
    let parsed_id = item_id.parse::<at_model::ItemId>()?;
    let mut db_handle = sandbox.database()?;
    let preset_id = at_db::presets::create_preset(
        db_handle.connection_mut(),
        &at_db::presets::NewPreset {
            id: None,
            name: "Server preset",
            description: None,
            scope: Scope::User,
            is_builtin: false,
        },
    )?;
    at_db::presets::set_member(
        db_handle.connection(),
        &preset_id,
        &parsed_id,
        ItemKind::McpServer,
        true,
    )?;
    assert_eq!(
        at_db::presets::preset_members(db_handle.connection(), &preset_id)?.len(),
        1
    );

    let preview = flow_harness::mcp_server_delete(&state, request(false, None))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Preview { preview } = preview else {
        panic!("delete must preview first");
    };
    flow_harness::mcp_server_delete(&state, request(true, Some(preview.preview_id)))?;
    assert_eq!(count(&database, "items"), 0, "the item row is removed");
    assert_eq!(
        at_db::presets::preset_members(db_handle.connection(), &preset_id)?.len(),
        0,
        "preset_members row cascades on server delete"
    );
    let text = std::fs::read_to_string(&native)?;
    assert!(
        !text.contains("demo-server"),
        "the native entry is gone: {text}"
    );
    Ok(())
}

#[test]
fn a_read_only_native_config_is_refused_without_writing() -> TestResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let native = sandbox.agent_path(&AgentId::ClaudeCode, ItemKind::McpServer, Scope::User)?;
    if let Some(parent) = native.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&native, b"{\n  \"theme\": \"dark\"\n}\n")?;
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o444))?;

    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.cannot_write_reason.is_some()),
        "a read-only target is reported before apply: {:?}",
        preview.files
    );
    let before = sandbox.snapshot()?;
    let error = flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )
    .expect_err("a read-only native config is refused");
    assert!(
        matches!(
            error.code,
            at_app::app::commands::CommandErrorCode::InvalidArgument
                | at_app::app::commands::CommandErrorCode::RefusedOverwrite
                | at_app::app::commands::CommandErrorCode::Conflict
        ),
        "a refusal is a typed error, not an internal failure: {error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a read-only MCP target");
    assert_eq!(count(&database, "items"), 0);
    Ok(())
}

/// One deployment row rendered without the volatile bookkeeping columns.
fn deployment_row_text(database: &at_db::Database, item_id: &str) -> Option<String> {
    database
        .connection()
        .query_row(
            "SELECT state, mode, target_path, link_target, store_hash, target_hash, \
                    baseline_hash, sync_label FROM deployments WHERE item_id = ?1",
            [item_id],
            |row| {
                let mut cells = Vec::with_capacity(8);
                for index in 0..8 {
                    let value: rusqlite::types::Value = row.get(index)?;
                    cells.push(format!("{value:?}"));
                }
                Ok(cells.join("|"))
            },
        )
        .ok()
}

/// Regression: a delete captured the row a later/earlier disable had left `off`, and undo
/// restored it without its owning operation, so the disable's own undo could not flip it back
/// to `linked`. The enable-change undo must re-align exactly the row it turned off.
#[test]
fn undoing_a_disable_after_undoing_a_delete_restores_the_linked_row() -> TestResult<()> {
    use at_app::app::mcp_servers::McpServerActionOutcome;
    use at_app::app::overwrite::operation_undo_inner;

    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let preview =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;
    let item_id = mcp_item_id(&database);
    let before = deployment_row_text(&database, &item_id).expect("the saved server is deployed");
    let request = |apply: bool, preview_id: Option<String>| McpServerActionRequest {
        item_id: item_id.clone(),
        scope: "user".to_owned(),
        project_id: None,
        agent: Some("claude_code".to_owned()),
        in_repo: false,
        apply,
        preview_id,
    };

    let preview = flow_harness::mcp_server_disable(&state, request(false, None))?;
    let McpServerActionOutcome::Preview { preview } = preview else {
        return Err("disable must preview first".into());
    };
    let disabled =
        flow_harness::mcp_server_disable(&state, request(true, Some(preview.preview_id)))?;
    let McpServerActionOutcome::Applied { applied } = disabled else {
        return Err("disable must apply the reviewed plan".into());
    };
    let disable_operation = applied.operation_id;

    let preview = flow_harness::mcp_server_delete(&state, request(false, None))?;
    let McpServerActionOutcome::Preview { preview } = preview else {
        return Err("delete must preview first".into());
    };
    let deleted = flow_harness::mcp_server_delete(&state, request(true, Some(preview.preview_id)))?;
    let McpServerActionOutcome::Applied { applied } = deleted else {
        return Err("delete must apply the reviewed plan".into());
    };
    assert!(
        deployment_row_text(&database, &item_id).is_none(),
        "the delete removes the row it captured"
    );

    // Undo newest-first, exactly as the undo screen and the sequence property do.
    operation_undo_inner(&state, &applied.operation_id, false)?;
    operation_undo_inner(&state, &disable_operation, false)?;
    assert_eq!(
        deployment_row_text(&database, &item_id).as_deref(),
        Some(before.as_str()),
        "undoing the disable re-links the row the delete's undo restored"
    );
    Ok(())
}

/// Reads the `items` row a saved server produced, with its project identity.
fn indexed_project(database: &at_db::Database, item_id: &str) -> Option<String> {
    database
        .connection()
        .query_row(
            "SELECT project_id FROM items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .expect("item row")
}

/// Reads the project identity and sync label of one deployment row.
fn deployment_project_and_label(
    database: &at_db::Database,
    item_id: &str,
) -> (Option<String>, Option<String>) {
    database
        .connection()
        .query_row(
            "SELECT project_id, sync_label FROM deployments WHERE item_id = ?1",
            [item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("deployment row")
}

#[test]
fn a_user_scope_save_while_a_project_is_active_stays_indexed_and_listed() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;

    // The screen passes the active project with every request, including a User-scope save.
    let mut request = stdio_request("Demo Server", "claude_code");
    request.project_id = Some(project_id.clone());
    let preview = flow_harness::mcp_plan_preview(&state, request)?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;

    let item_id = mcp_item_id(&database);
    assert_eq!(
        indexed_project(&database, &item_id),
        None,
        "a User item must not carry the active project"
    );
    let (deployment_project, sync_label) = deployment_project_and_label(&database, &item_id);
    assert_eq!(
        deployment_project, None,
        "the User deployment is project-less"
    );
    assert!(
        sync_label.is_some(),
        "the deployment row carries a sync label"
    );

    // The MCP screen's own projection must show it as managed rather than a native discovery.
    let list = flow_harness::mcp_server_list(
        &state,
        at_app::app::mcp_servers::McpServerListRequest {
            scope: "user".to_owned(),
            project_id: Some(project_id.clone()),
        },
    )?;
    let row = list
        .servers
        .iter()
        .find(|row| row.name == "Demo Server")
        .ok_or("the saved server must appear in the MCP list")?;
    assert_eq!(row.id, item_id);
    assert!(
        !row.native,
        "the saved server is managed, not native-only: {row:?}"
    );
    assert!(
        row.agents
            .iter()
            .any(|cell| cell.agent == "claude_code" && cell.enabled),
        "the saved server is deployed to Claude Code: {row:?}"
    );

    // Disable and re-enable exercise the same index writer with the same active project.
    let action = |apply: bool, preview_id: Option<String>| McpServerActionRequest {
        item_id: item_id.clone(),
        scope: "user".to_owned(),
        project_id: Some(project_id.clone()),
        agent: Some("claude_code".to_owned()),
        in_repo: false,
        apply,
        preview_id,
    };
    let preview = flow_harness::mcp_server_disable(&state, action(false, None))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Preview { preview } = preview else {
        return Err("disable must preview first".into());
    };
    flow_harness::mcp_server_disable(&state, action(true, Some(preview.preview_id)))?;
    let (deployment_project, _) = deployment_project_and_label(&database, &item_id);
    assert_eq!(
        deployment_project, None,
        "disable keeps the User deployment project-less"
    );

    let preview = flow_harness::mcp_server_enable(&state, action(false, None))?;
    let at_app::app::mcp_servers::McpServerActionOutcome::Preview { preview } = preview else {
        return Err("enable must preview first".into());
    };
    flow_harness::mcp_server_enable(&state, action(true, Some(preview.preview_id)))?;
    let (deployment_project, sync_label) = deployment_project_and_label(&database, &item_id);
    assert_eq!(
        deployment_project, None,
        "enable keeps the User deployment project-less"
    );
    assert_eq!(sync_label.as_deref(), Some("in_sync"));
    Ok(())
}

#[test]
fn a_project_scope_save_records_its_project() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_secret(&database, "DEMO_TOKEN")?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "project", Some(&project_id))?;

    let mut request = stdio_request("Project Server", "claude_code");
    request.scope = "project".to_owned();
    request.project_id = Some(project_id.clone());
    let preview = flow_harness::mcp_plan_preview(&state, request)?;
    flow_harness::mcp_save(
        &state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec!["claude_code".to_owned()],
            acknowledge_unsupported: false,
        },
    )?;

    let item_id = mcp_item_id(&database);
    assert_eq!(
        indexed_project(&database, &item_id).as_deref(),
        Some(project_id.as_str()),
        "a Project item belongs to the active project"
    );
    let (deployment_project, sync_label) = deployment_project_and_label(&database, &item_id);
    assert_eq!(
        deployment_project.as_deref(),
        Some(project_id.as_str()),
        "the Project deployment belongs to the same project"
    );
    assert!(sync_label.is_some());
    Ok(())
}
