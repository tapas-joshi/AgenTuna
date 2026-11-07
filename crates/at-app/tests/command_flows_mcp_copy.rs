//! GUI-path MCP copy flows through the real at-app command bodies.
//!
//! A copy is planned by the shared engine planner, applied as one journaled at-fs operation and
//! indexed like a saved server, so drift stays quiet and one undo restores every file.

mod support;

use std::collections::BTreeMap;

use at_app::app::flow_harness;
use at_app::app::mcp_copy::{McpCopyApplyRequest, McpCopyPreviewRequest};
use at_app::app::overwrite::operation_undo_inner;
use at_db::Database;
use at_model::{AgentId, ItemId, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_mcp_entry};
use at_store::mcp_store::McpStore;
use support::{Sandbox, TestResult};

/// A stdio definition with no secret, so every destination agent can render it.
fn definition(display_name: &str) -> McpServerDef {
    McpServerDef {
        display_name: display_name.to_owned(),
        description: None,
        transport: McpTransport::Stdio {
            command: "web-search-server".to_owned(),
            args: vec!["--safe".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    }
}

/// Writes one canonical server and its manifest record into a store.
fn seed_server(layout: &StoreLayout, slug: &str, definition: &McpServerDef) -> TestResult<ItemId> {
    let mut store = McpStore::load(layout)?.store;
    store.set(slug, definition.clone());
    std::fs::write(layout.mcp_file(), store.to_bytes()?)?;
    let mut manifest = StoreManifest::load(layout)?.manifest;
    let item_id = ItemId::new();
    let value = serde_json::to_value(definition)?;
    manifest.insert_item(
        ManifestItem::new(
            item_id,
            at_model::ItemKind::McpServer,
            Slug::parse(slug)?,
            StorePath::parse("mcp.json")?,
            hash_mcp_entry(&value)?,
        )
        .with_name(definition.display_name.clone()),
    )?;
    std::fs::write(layout.manifest_path(), manifest.to_bytes()?)?;
    Ok(item_id)
}

/// Creates one empty scope store.
fn init_scope(sandbox: &Sandbox, scope: Scope) -> TestResult<StoreLayout> {
    let layout = sandbox.layout(scope)?;
    at_store::layout::init_store(scope, layout.root())?;
    Ok(layout)
}

fn deployment_count(database: &Database, item_id: &str) -> i64 {
    database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM deployments WHERE item_id = ?1 AND scope = 'project'",
            [item_id],
            |row| row.get(0),
        )
        .expect("deployment count")
}

#[test]
fn copying_a_server_into_the_project_is_one_journaled_undoable_operation() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    sandbox.register_agent(&database, AgentId::Omp)?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    let user_layout = init_scope(&sandbox, Scope::User)?;
    let project_layout = init_scope(&sandbox, Scope::Project)?;
    let source_id = seed_server(&user_layout, "web-search", &definition("Web Search"))?;
    let before = sandbox.snapshot()?;

    let preview = flow_harness::mcp_server_copy_preview(
        &state,
        McpCopyPreviewRequest {
            item_id: source_id.to_string(),
            scope: "user".to_owned(),
            project_id: Some(project_id.clone()),
            target_scope: "project".to_owned(),
            on_collision: None,
            new_slug: None,
        },
    )?;
    let preview_id = preview
        .preview_id
        .clone()
        .expect("a free destination plans");
    assert_eq!(preview.source_scope, "user");
    assert_eq!(preview.target_scope, "project");
    assert_eq!(preview.target_slug, "web-search");
    assert!(preview.collision.is_none());
    assert!(!preview.replaces_existing);
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.ends_with("mcp.json") && file.contains(".agentuna")),
        "the destination store is in the reviewed file list: {:?}",
        preview.files
    );
    assert!(
        preview.agents.iter().any(|agent| agent.agent == "omp"),
        "the project agents are in the reviewed plan: {:?}",
        preview.agents
    );
    assert!(
        preview.refusals.is_empty(),
        "no agent refuses a secret-free definition: {:?}",
        preview.refusals
    );
    before.assert_unchanged(&sandbox.snapshot()?, "planning a copy");

    let applied = flow_harness::mcp_server_copy_apply(&state, McpCopyApplyRequest { preview_id })?;
    assert_ne!(
        applied.target_item_id,
        *source_id.to_string(),
        "a copy is a new item"
    );
    let copied = McpStore::load(&project_layout)?.store;
    assert_eq!(copied.get("web-search"), Some(&definition("Web Search")));
    let native = std::fs::read_to_string(sandbox.project.join("mcp.json"))?;
    assert!(
        native.contains("web-search"),
        "the project agent document holds the copy: {native}"
    );
    assert_eq!(
        deployment_count(&database, &applied.target_item_id),
        preview.agents.len() as i64,
        "one deployment row per projected agent document"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the copy refreshed its drift baselines"
    );

    operation_undo_inner(&state, &applied.operation_id, false)?;
    assert!(
        McpStore::load(&project_layout)?
            .store
            .get("web-search")
            .is_none(),
        "undo removes the copied definition"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "undo of the copy");
    Ok(())
}

#[test]
fn a_destination_collision_needs_replace_or_rename_and_stale_previews_are_refused() -> TestResult<()>
{
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Omp)?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    let user_layout = init_scope(&sandbox, Scope::User)?;
    let project_layout = init_scope(&sandbox, Scope::Project)?;
    let source_id = seed_server(&user_layout, "web-search", &definition("Web Search"))?;
    let mut other = definition("Web Search (project)");
    other.description = Some("already in the project".to_owned());
    seed_server(&project_layout, "web-search", &other)?;
    let before = sandbox.snapshot()?;

    let collision = flow_harness::mcp_server_copy_preview(
        &state,
        McpCopyPreviewRequest {
            item_id: source_id.to_string(),
            scope: "user".to_owned(),
            project_id: Some(project_id.clone()),
            target_scope: "project".to_owned(),
            on_collision: None,
            new_slug: None,
        },
    )?;
    assert!(collision.preview_id.is_none());
    let hit = collision
        .collision
        .as_ref()
        .expect("a collision is reported");
    assert_eq!(hit.slug, "web-search");
    assert_eq!(hit.scope, "project");
    before.assert_unchanged(&sandbox.snapshot()?, "a collision prompt");

    let renamed = flow_harness::mcp_server_copy_preview(
        &state,
        McpCopyPreviewRequest {
            item_id: source_id.to_string(),
            scope: "user".to_owned(),
            project_id: Some(project_id.clone()),
            target_scope: "project".to_owned(),
            on_collision: Some("rename".to_owned()),
            new_slug: Some("web-search-copy".to_owned()),
        },
    )?;
    assert_eq!(renamed.target_slug, "web-search-copy");
    let preview_id = renamed.preview_id.expect("rename plans a copy");
    flow_harness::mcp_server_copy_apply(&state, McpCopyApplyRequest { preview_id })?;
    let copied = McpStore::load(&project_layout)?.store;
    assert!(copied.get("web-search-copy").is_some());

    // A reviewed plan is single-use: replaying it after a successful apply is refused.
    let replay = flow_harness::mcp_server_copy_apply(
        &state,
        McpCopyApplyRequest {
            preview_id: "preview-that-was-never-issued".to_owned(),
        },
    )
    .expect_err("an unknown preview is refused");
    assert_eq!(
        replay.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument,
        "{replay:?}"
    );
    Ok(())
}

#[test]
fn a_stale_copy_preview_is_refused_and_changes_nothing() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Omp)?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    let user_layout = init_scope(&sandbox, Scope::User)?;
    let _project_layout = init_scope(&sandbox, Scope::Project)?;
    let source_id = seed_server(&user_layout, "web-search", &definition("Web Search"))?;
    // A project document exists, so the reviewed plan covers an existing file.
    std::fs::write(
        sandbox.project.join("mcp.json"),
        b"{\n  \"mcpServers\": {}\n}\n",
    )?;

    let preview = flow_harness::mcp_server_copy_preview(
        &state,
        McpCopyPreviewRequest {
            item_id: source_id.to_string(),
            scope: "user".to_owned(),
            project_id: Some(project_id),
            target_scope: "project".to_owned(),
            on_collision: None,
            new_slug: None,
        },
    )?;
    let preview_id = preview.preview_id.expect("a free destination plans");
    // An external writer touches the reviewed file after the user reviewed the plan.
    std::fs::write(
        sandbox.project.join("mcp.json"),
        b"{\n  \"theme\": \"light\"\n}\n",
    )?;
    let before = sandbox.snapshot()?;
    let error = flow_harness::mcp_server_copy_apply(&state, McpCopyApplyRequest { preview_id })
        .expect_err("a file changed after review is refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::ExternalWriteConflict,
        "{error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a stale copy preview");
    Ok(())
}
