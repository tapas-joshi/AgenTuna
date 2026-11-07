//! First-run flows: a scope with no store yet gets its skeleton inside the reviewed plan.
//!
//! Every test starts from a sandbox with no store directories, so the preview must show the
//! store creation, the apply must create the store and the item as one operation, and undo must
//! remove what that operation created.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use at_app::app::flow_harness;
use at_app::app::mcp_copy::McpCopyPreviewRequest;
use at_app::app::mcp_form::{McpPlanPreviewRequest, McpSaveRequest};
use at_app::app::network::NetworkState;
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::skill_wizard::{SkillAddApplyRequest, SkillAddPreviewRequest};
use at_engine::skill_wizard::{AddSkillWizard, SourceKind, WizardStep};
use at_model::{AgentId, Scope};
use support::{Sandbox, TestResult};

fn stdio_request(name: &str, agent: &str) -> McpPlanPreviewRequest {
    McpPlanPreviewRequest {
        name: name.to_owned(),
        transport: "stdio".to_owned(),
        command: "npx".to_owned(),
        args: vec!["-y".to_owned(), "demo-server".to_owned()],
        url: String::new(),
        headers: Vec::new(),
        env: Vec::new(),
        scope: "user".to_owned(),
        agents: vec![agent.to_owned()],
        project_id: None,
        item_id: None,
    }
}

fn save_request(preview_id: &str) -> McpSaveRequest {
    save_request_for(preview_id, "codex")
}

fn save_request_for(preview_id: &str, agent: &str) -> McpSaveRequest {
    McpSaveRequest {
        preview_id: preview_id.to_owned(),
        selected_agents: vec![agent.to_owned()],
        acknowledge_unsupported: false,
    }
}

fn new_skill_request() -> SkillAddPreviewRequest {
    SkillAddPreviewRequest {
        wizard: AddSkillWizard {
            step: WizardStep::Review,
            source: Some(SourceKind::New),
            name: "Fresh Skill".to_owned(),
            scope: Some("user".to_owned()),
            agents: vec!["codex".to_owned()],
            ..AddSkillWizard::default()
        },
        project_id: None,
        import_id: None,
        stage_requested: false,
    }
}

#[test]
fn mcp_save_creates_the_user_store_inside_the_reviewed_plan_and_undo_removes_it() -> TestResult<()>
{
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    let state = sandbox.state()?;
    let layout = sandbox.layout(Scope::User)?;
    assert!(
        !layout.is_ready(),
        "the sandbox starts without a user store"
    );

    let preview = flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "codex"))?;
    let init = preview
        .store_init
        .as_ref()
        .expect("the review includes the store creation");
    assert_eq!(init.path, layout.root().display().to_string());
    assert!(
        init.files_created > 0,
        "the layout entries are reviewed, not just the store root"
    );
    assert_eq!(
        preview.files.first().map(|file| file.agent.as_str()),
        Some("AgenTuna"),
        "the store creation is listed before the native documents: {:?}",
        preview.files
    );

    let saved = flow_harness::mcp_save(&state, save_request(&preview.preview_id))?;
    assert!(layout.is_ready(), "the store manifest is written");
    assert!(
        layout.mcp_file().is_file(),
        "the server document is written"
    );
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.path == layout.mcp_file().display().to_string()),
        "the preview listed the store document"
    );

    // A second save finds a ready store and must not plan the skeleton again.
    let again = flow_harness::mcp_plan_preview(&state, stdio_request("Second Server", "codex"))?;
    assert!(
        again.store_init.is_none(),
        "a ready store is left alone: {:?}",
        again.store_init
    );
    let second = flow_harness::mcp_save(&state, save_request(&again.preview_id))?;

    // Undo newest first: the second save restores the store document the first save wrote, then
    // the first save's undo removes the store files and the empty layout directories it created.
    operation_undo_inner(&state, &second.operation_id, false)?;
    operation_undo_inner(&state, &saved.operation_id, false)?;
    assert!(
        !layout.is_ready(),
        "undo removed the store manifest the operation created"
    );
    assert!(
        !layout.skills_dir().exists(),
        "an empty layout directory created by the operation is removed"
    );
    Ok(())
}

#[test]
fn skill_add_creates_the_store_in_the_same_operation() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    let state = sandbox.state()?;
    let layout = sandbox.layout(Scope::User)?;
    let network = NetworkState::new(at_model::network::NetworkPolicy::default());

    let preview = flow_harness::skill_add_preview_inner(&state, &network, new_skill_request())?;
    let init = preview
        .store_init
        .as_ref()
        .expect("the review includes the store creation");
    assert_eq!(init.path, layout.root().display().to_string());
    assert!(!layout.is_ready());

    flow_harness::skill_add_apply_inner(
        &state,
        SkillAddApplyRequest {
            preview_id: preview.preview_id.clone(),
            confirm_overwrites: false,
        },
    )?;
    assert!(layout.is_ready(), "the store manifest is written");
    assert!(
        layout.skills_dir().join("fresh-skill/SKILL.md").is_file(),
        "the skill package is written"
    );
    Ok(())
}

#[test]
fn copy_into_a_scope_without_a_store_creates_it_in_the_same_operation() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let project_id = sandbox.register_project(&mut database)?;
    let state = sandbox.state()?;
    let project_state = sandbox.project_state()?;
    let source =
        flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "claude_code"))?;
    flow_harness::mcp_save(&state, save_request_for(&source.preview_id, "claude_code"))?;
    let target = sandbox.layout(Scope::Project)?;
    assert!(!target.is_ready(), "the project has no store yet");

    let preview = flow_harness::mcp_server_copy_preview(
        &project_state,
        McpCopyPreviewRequest {
            item_id: item_id(&database)?,
            scope: "user".to_owned(),
            project_id: Some(project_id),
            target_scope: "project".to_owned(),
            on_collision: None,
            new_slug: None,
        },
    )?;
    assert!(
        preview.store_init.is_some(),
        "the copy review includes the destination store"
    );
    flow_harness::mcp_server_copy_apply(
        &project_state,
        at_app::app::mcp_copy::McpCopyApplyRequest {
            preview_id: preview.preview_id.clone().expect("reviewed identity"),
        },
    )?;
    assert!(target.is_ready(), "the destination store is created");
    Ok(())
}

#[test]
fn a_failed_native_phase_rolls_back_the_store_it_created() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    let state = sandbox.state()?;
    let layout = sandbox.layout(Scope::User)?;
    // Codex's document lives in `~/.codex/config.toml`; a read-only directory makes the native
    // phase fail after the store phase succeeded.
    let codex_dir = sandbox.home.join(".codex");
    fs::create_dir_all(&codex_dir)?;
    fs::set_permissions(&codex_dir, fs::Permissions::from_mode(0o555))?;

    let preview = flow_harness::mcp_plan_preview(&state, stdio_request("Demo Server", "codex"))?;
    let error = flow_harness::mcp_save(&state, save_request(&preview.preview_id))
        .expect_err("the native write is refused");
    assert!(!error.to_string().is_empty());
    fs::set_permissions(&codex_dir, fs::Permissions::from_mode(0o755))?;
    assert!(!layout.is_ready(), "the store phase was rolled back");
    assert!(!layout.mcp_file().exists(), "no server document survives");
    assert!(
        !layout.skills_dir().exists(),
        "directories the rolled-back operation created are removed"
    );
    Ok(())
}

fn item_id(database: &at_db::Database) -> TestResult<String> {
    Ok(database.connection().query_row(
        "SELECT id FROM items WHERE kind = 'mcp_server' ORDER BY rowid LIMIT 1",
        [],
        |row| row.get(0),
    )?)
}
