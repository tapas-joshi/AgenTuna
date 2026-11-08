//! First-run flows: a scope with no store yet gets its skeleton inside the reviewed plan.
//!
//! Every test starts from a sandbox with no store directories, so the preview must show the
//! store creation, the apply must create the store and the item as one operation, and undo must
//! remove what that operation created.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

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
use at_model::{AgentId, LinkMode, Scope};
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
fn shared_project_skill_root_is_written_and_backed_up_once_then_undo_restores_it() -> TestResult<()>
{
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    sandbox.register_agent(&database, AgentId::Omp)?;
    let project_id = sandbox.register_project(&mut database)?;
    at_db::settings::set_default_link_mode(
        database.connection_mut(),
        LinkMode::Copy,
        at_db::ops::OperationActor::Ui,
    )?;
    let target_dir = sandbox.project.join(".agents/skills/shared-skill");
    fs::create_dir_all(&target_dir)?;
    let target_file = target_dir.join("SKILL.md");
    let original = b"---\nname: Shared Skill\ndescription: old copy\n---\n";
    fs::write(&target_file, original)?;
    let before = sandbox.snapshot()?;
    let state = sandbox.project_state()?;
    let network = NetworkState::new(at_model::network::NetworkPolicy::default());

    let preview = flow_harness::skill_add_preview_inner(
        &state,
        &network,
        SkillAddPreviewRequest {
            wizard: AddSkillWizard {
                step: WizardStep::Review,
                source: Some(SourceKind::New),
                name: "Shared Skill".to_owned(),
                scope: Some("project".to_owned()),
                agents: vec!["codex".to_owned(), "omp".to_owned()],
                details: at_engine::skill_wizard::SourceDetails {
                    description: "A single physical destination shared by Codex and omp."
                        .to_owned(),
                    template: Some("blank".to_owned()),
                    ..Default::default()
                },
                ..AddSkillWizard::default()
            },
            project_id: Some(project_id),
            import_id: None,
            stage_requested: false,
        },
    )?;
    let target_path = target_file.display().to_string();
    assert_eq!(
        preview
            .files
            .iter()
            .filter(|file| file.path == target_path)
            .count(),
        1,
        "one shared destination appears once in the reviewed plan"
    );
    assert_eq!(
        preview.deployments.len(),
        2,
        "both agents remain visible in review"
    );
    assert!(preview.requires_overwrite_confirmation);

    let applied = flow_harness::skill_add_apply_inner(
        &state,
        SkillAddApplyRequest {
            preview_id: preview.preview_id,
            confirm_overwrites: true,
        },
    )?;
    assert!(
        fs::read(&target_file)?
            .windows(12)
            .any(|bytes| bytes == b"Shared Skill")
    );
    let operation_target_count = database.connection().query_row(
        "SELECT COUNT(*) FROM operation_files WHERE operation_id = ?1 AND path = ?2",
        rusqlite::params![applied.operation_id, target_path],
        |row| row.get::<_, i64>(0),
    )?;
    assert_eq!(
        operation_target_count, 1,
        "the shared file is journaled once"
    );
    let backup_count = database.connection().query_row(
        "SELECT COUNT(*) FROM backups WHERE operation_id = ?1 AND original_path = ?2",
        rusqlite::params![applied.operation_id, target_path],
        |row| row.get::<_, i64>(0),
    )?;
    assert_eq!(backup_count, 1, "the shared destination has one backup");

    operation_undo_inner(&state, &applied.operation_id, false)?;
    assert_eq!(fs::read(&target_file)?, original);
    before.assert_unchanged(&sandbox.snapshot()?, "undo of shared-root skill add");
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
