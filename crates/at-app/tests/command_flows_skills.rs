//! GUI-path skill flows through the real at-app command bodies.
//!
//! Each test drives the same planner/apply/undo functions the webview calls and asserts on the
//! filesystem tree and the index rows, not on the view structs alone.

mod support;

use at_app::app::flow_harness::{self, skill_agent_apply, skill_agent_preview};
use at_app::app::migrate::operation_undo_check_inner;
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::skill_agent::SkillAgentPreviewRequest;
use at_app::app::skill_delete::{SkillDeleteApplyRequest, SkillDeletePreviewRequest};
use at_app::app::skill_transfer::{SkillTransferApplyRequest, SkillTransferPreviewRequest};
use std::str::FromStr;

use at_db::Database;
use at_model::{Agent, AgentId, ItemState, Scope};
use support::{Sandbox, TestResult};

const SKILL_MD: &[u8] = b"---\nname: Demo Skill\ndescription: sandbox only\n---\n\nBody.\n";

/// Counts the rows of one table.
fn count(database: &Database, table: &str) -> i64 {
    database
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count query")
}

/// The recorded deployment state for one item and agent, if any row exists.
fn deployment_state(
    database: &Database,
    item_id: &str,
    agent: &AgentId,
    scope: Scope,
) -> Option<ItemState> {
    database
        .connection()
        .query_row(
            "SELECT state FROM deployments WHERE item_id = ?1 AND agent_id = ?2 \
               AND scope = ?3 AND COALESCE(project_id, '') = ''",
            rusqlite::params![item_id, agent.to_string(), scope.wire_name()],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(|wire| ItemState::from_str(&wire).expect("known deployment state"))
}

#[test]
fn enable_and_disable_are_reviewed_and_undo_restores_the_exact_tree() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let paths = sandbox.paths()?;
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "demo-skill",
        "Demo Skill",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;
    let target = sandbox
        .agent_skills_dir(&AgentId::ClaudeCode, Scope::User)?
        .join(&skill.slug);
    let before = sandbox.snapshot()?;

    // ---- review and enable -------------------------------------------------------------
    let preview = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "claude_code".to_owned(),
            enabled: true,
            project_id: None,
        },
    )?;
    assert!(!preview.no_op, "an absent deployment is not a no-op");
    assert_eq!(preview.mode.as_deref(), Some("link"));
    assert_eq!(preview.paths, vec![target.display().to_string()]);
    assert!(
        preview.coupled_agents.is_empty(),
        "claude_code owns its own user skills root"
    );
    assert!(
        !target.symlink_metadata().is_ok(),
        "a preview writes nothing"
    );
    assert_eq!(before.difference(&sandbox.snapshot()?), "");

    let applied = skill_agent_apply(&state, &preview.preview_id)?;
    assert!(applied.changed);
    let enable_operation = applied
        .operation_id
        .clone()
        .expect("enable records an operation");
    assert!(
        target.symlink_metadata()?.file_type().is_symlink(),
        "enable creates a symlink"
    );
    assert_eq!(
        deployment_state(
            &database,
            &skill.item_id.to_string(),
            &AgentId::ClaudeCode,
            Scope::User
        ),
        Some(ItemState::Linked)
    );
    assert_eq!(count(&database, "operations"), 1);
    assert_eq!(
        count(&database, "operation_files"),
        1,
        "the deployment wrote exactly one file"
    );

    // ---- undo the enable ---------------------------------------------------------------
    let check = operation_undo_check_inner(&state, &enable_operation)?;
    assert!(check.known, "the enable is in history");
    assert!(!check.already_undone);
    operation_undo_inner(&state, &enable_operation, false)?;
    assert!(!target.symlink_metadata().is_ok(), "undo removed the link");
    before.assert_unchanged(&sandbox.snapshot()?, "undo of the enable");
    assert_eq!(
        deployment_state(
            &database,
            &skill.item_id.to_string(),
            &AgentId::ClaudeCode,
            Scope::User
        ),
        Some(ItemState::Off),
        "undo must not leave the row claiming a link that no longer exists"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "an undone deployment is not external drift"
    );

    // ---- review and disable ------------------------------------------------------------
    let enable_again = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "claude_code".to_owned(),
            enabled: true,
            project_id: None,
        },
    )?;
    assert!(
        !enable_again.no_op,
        "the app agrees the deployment is gone after undo"
    );
    let relinked = skill_agent_apply(&state, &enable_again.preview_id)?;
    assert!(relinked.changed);

    let disable = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "claude_code".to_owned(),
            enabled: false,
            project_id: None,
        },
    )?;
    assert_eq!(disable.paths, vec![target.display().to_string()]);
    let disabled = skill_agent_apply(&state, &disable.preview_id)?;
    assert!(disabled.changed);
    assert!(
        !target.symlink_metadata().is_ok(),
        "disable removed the link"
    );
    assert!(
        skill.dir.join("SKILL.md").exists(),
        "disable never touches the store package"
    );
    assert_eq!(
        deployment_state(
            &database,
            &skill.item_id.to_string(),
            &AgentId::ClaudeCode,
            Scope::User
        ),
        Some(ItemState::Off)
    );

    // ---- undo the disable --------------------------------------------------------------
    let disable_operation = disabled.operation_id.expect("disable records an operation");
    operation_undo_inner(&state, &disable_operation, false)?;
    assert!(
        target.symlink_metadata()?.file_type().is_symlink(),
        "undo of a disable restores the link"
    );
    assert_eq!(
        deployment_state(
            &database,
            &skill.item_id.to_string(),
            &AgentId::ClaudeCode,
            Scope::User
        ),
        Some(ItemState::Linked),
        "the restored link must be recorded as deployed again"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "undo of a disable is not external drift"
    );
    Ok(())
}

#[test]
fn native_skill_list_projects_all_readers_of_a_shared_root() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    for (agent, display_name) in [(AgentId::Codex, "Codex"), (AgentId::Omp, "omp")] {
        sandbox.register_agent(&database, agent.clone())?;
        at_db::agents::upsert_agent(
            database.connection(),
            &Agent {
                id: agent,
                display_name: display_name.to_owned(),
                detected: true,
                version: Some("1.0".to_owned()),
            },
        )?;
    }
    let root = sandbox.agent_skills_dir(&AgentId::Codex, Scope::Project)?;
    std::fs::create_dir_all(root.join("release-notes"))?;
    let skill_path = root.join("release-notes");
    let skill_path_text = skill_path.to_string_lossy().into_owned();
    for agent in [AgentId::Codex, AgentId::Omp] {
        at_db::agents::upsert_agent_path(
            database.connection(),
            &at_db::agents::AgentPathSpec {
                agent: &agent,
                scope: Scope::Project,
                kind: at_db::agents::AgentPathKind::SkillsDir,
                path: &root,
                format: Some(at_db::agents::AgentPathFormat::Dir),
                is_primary: true,
                exists: true,
            },
        )?;
    }
    at_db::native::replace_scan(
        database.connection(),
        &AgentId::Codex,
        Scope::Project,
        &[at_db::native::NativeScanEntry::new(
            at_model::ItemKind::Skill,
            "release-notes",
            &skill_path_text,
        )],
    )?;
    let state = flow_harness::test_state(database, sandbox.project_paths()?);

    let view = flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: Some("project".to_owned()),
            project_id: None,
        },
    )?;
    let row = view
        .rows
        .iter()
        .find(|row| row.native && row.name == "release-notes")
        .ok_or("shared native skill row is missing")?;
    assert_eq!(row.agents.len(), 2);
    assert!(row.agents.iter().all(|cell| cell.state == "native"));
    assert!(
        row.agents
            .iter()
            .all(|cell| cell.config_path.as_deref() == Some(skill_path_text.as_str()))
    );
    assert!(
        view.agents
            .iter()
            .any(|agent| agent.id == "codex" && agent.label == "Codex")
    );
    assert!(
        view.agents
            .iter()
            .any(|agent| agent.id == "omp" && agent.label == "omp")
    );
    Ok(())
}

#[test]
fn a_shared_skills_root_reports_the_coupled_agent_and_bulk_apply_keeps_the_baseline()
-> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    sandbox.register_agent(&database, AgentId::Omp)?;
    let project_id = sandbox.register_project(&mut database)?;
    let paths = sandbox.project_paths()?;
    let skill = sandbox.install_skill(
        &database,
        Scope::Project,
        "shared-skill",
        "Shared Skill",
        SKILL_MD,
        Some(&project_id),
    )?;
    let state = sandbox.project_state()?;
    let codex_dir = sandbox.agent_skills_dir(&AgentId::Codex, Scope::Project)?;
    let omp_dir = sandbox.agent_skills_dir(&AgentId::Omp, Scope::Project)?;
    assert_eq!(
        codex_dir, omp_dir,
        "codex and omp share <project>/.agents/skills"
    );
    assert!(codex_dir.ends_with(".agents/skills"), "{codex_dir:?}");
    let target = codex_dir.join(&skill.slug);

    let preview = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "project".to_owned(),
            agent_id: "codex".to_owned(),
            enabled: true,
            project_id: Some(project_id.clone()),
        },
    )?;
    assert_eq!(
        preview.coupled_agents,
        vec!["omp".to_owned()],
        "the preview names the other agent loading the same folder"
    );
    assert!(
        preview
            .warnings
            .iter()
            .any(|warning| warning.contains("omp")),
        "the warning explains the coupled effect: {:?}",
        preview.warnings
    );

    let batch = flow_harness::bulk_skill_apply(
        &state,
        at_app::app::bulk_skill::BulkSkillApplyRequest {
            preview_ids: vec![preview.preview_id],
            label: "Enable shared-skill".to_owned(),
        },
    )?;
    assert!(batch.applied, "{batch:?}");
    assert!(target.symlink_metadata().is_ok());
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the bulk write refreshed the shared baseline"
    );

    // A genuine external edit inside the shared folder is still reported once.
    std::fs::write(
        target.join("SKILL.md"),
        b"---\nname: Shared Skill\n---\n\nEdited by hand.\n",
    )?;
    let notices = flow_harness::reconcile_drift(&database)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].path, target);
    Ok(())
}

#[test]
fn a_lock_and_a_stale_preview_refuse_without_touching_the_tree() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let paths = sandbox.paths()?;
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "locked-skill",
        "Locked Skill",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;
    let before = sandbox.snapshot()?;

    at_db::locks::upsert_lock(
        database.connection(),
        &at_db::locks::LockSpec {
            item_id: &skill.item_id,
            scope: Scope::Global,
            reason: Some("pinned by policy"),
            locked_by: Some("sandbox"),
        },
    )?;
    let error = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "claude_code".to_owned(),
            enabled: true,
            project_id: None,
        },
    )
    .expect_err("a Global lock refuses the deployment");
    assert_eq!(error.code, at_app::app::commands::CommandErrorCode::Locked);
    before.assert_unchanged(&sandbox.snapshot()?, "a locked deployment");

    at_db::locks::remove_lock(database.connection(), &skill.item_id, Scope::Global)?;
    let preview = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "claude_code".to_owned(),
            enabled: true,
            project_id: None,
        },
    )?;

    // The store package changes after review: apply must refuse and write nothing.
    std::fs::write(
        skill.dir.join("SKILL.md"),
        b"---\nname: Demo Skill\n---\n\nChanged after review.\n",
    )?;
    let before_apply = sandbox.snapshot()?;
    let error = skill_agent_apply(&state, &preview.preview_id)
        .expect_err("a package changed after review is refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::Conflict
    );
    before_apply.assert_unchanged(&sandbox.snapshot()?, "a stale skill preview");
    assert_eq!(count(&database, "operations"), 0, "nothing was journaled");
    Ok(())
}

#[test]
fn deleting_a_skill_requires_the_typed_name_and_undo_restores_bytes() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "delete-me",
        "Delete Me",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;
    let before = sandbox.snapshot()?;
    let package_bytes = std::fs::read(skill.dir.join("SKILL.md"))?;

    let preview = flow_harness::skill_delete_preview(
        &state,
        SkillDeletePreviewRequest {
            item_id: skill.item_id.to_string(),
            project_id: None,
        },
    )?;
    assert_eq!(preview.name, "Delete Me");
    assert_eq!(preview.scope, "user");
    assert_eq!(preview.files, vec!["SKILL.md"]);
    before.assert_unchanged(&sandbox.snapshot()?, "a delete preview");

    let error = flow_harness::skill_delete_apply(
        &state,
        SkillDeleteApplyRequest {
            preview_id: preview.preview_id.clone(),
            confirmed_name: "Not The Name".to_owned(),
        },
    )
    .expect_err("a wrong confirmation refuses");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a refused confirmation");

    let preview = flow_harness::skill_delete_preview(
        &state,
        SkillDeletePreviewRequest {
            item_id: skill.item_id.to_string(),
            project_id: None,
        },
    )?;
    let result = flow_harness::skill_delete_apply(
        &state,
        SkillDeleteApplyRequest {
            preview_id: preview.preview_id,
            confirmed_name: "Delete Me".to_owned(),
        },
    )?;
    assert!(!skill.dir.exists(), "the package is gone");
    assert!(
        database.connection().query_row(
            "SELECT COUNT(*) FROM items WHERE id = ?1",
            [skill.item_id.to_string()],
            |row| row.get::<_, i64>(0)
        )? == 0
    );

    operation_undo_inner(&state, &result.operation_id, false)?;
    assert_eq!(
        std::fs::read(skill.dir.join("SKILL.md"))?,
        package_bytes,
        "undo restores the exact package bytes"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "undo of the delete");
    Ok(())
}

#[test]
fn a_promote_and_a_move_are_reviewed_and_undoable() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "promote-me",
        "Promote Me",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;

    // A destination store is created through the reviewed scope-setup action, exactly as the
    // Overview screen does before a cross-scope transfer is offered.
    let setup = at_app::app::overview::overview_setup_preview_inner(&state, "global", None)?;
    assert!(!setup.already_ready);
    let setup_id = setup.preview_id.expect("a new store needs a reviewed plan");
    at_app::app::overview::overview_setup_apply_inner(&state, &setup_id, true)?;

    let options = flow_harness::skill_transfer_options(
        &state,
        at_app::app::skill_transfer::SkillTransferOptionsRequest {
            item_id: skill.item_id.to_string(),
            transfer: "promote".to_owned(),
            project_id: None,
        },
    )?;
    assert_eq!(options.source_scope, "user");
    assert!(
        options
            .targets
            .iter()
            .any(|target| target.scope == "global"),
        "User can be promoted to Global: {:?}",
        options.targets
    );

    let preview = flow_harness::skill_transfer_preview(
        &state,
        SkillTransferPreviewRequest {
            item_id: skill.item_id.to_string(),
            target_scope: "global".to_owned(),
            transfer: "promote".to_owned(),
            resolution: None,
            new_slug: None,
            project_id: None,
        },
    )?;
    let preview_id = preview
        .preview_id
        .clone()
        .expect("a free destination plans");
    assert_eq!(preview.target_scope, "global");
    assert!(preview.target_path.contains("promote-me"));
    assert!(preview.overwrite.is_none());

    let applied =
        flow_harness::skill_transfer_apply(&state, SkillTransferApplyRequest { preview_id })?;
    let global = sandbox.layout(Scope::Global)?;
    let promoted = global.skill_dir(&at_store::layout::Slug::parse("promote-me")?);
    assert_eq!(
        std::fs::read(promoted.join("SKILL.md"))?,
        std::fs::read(skill.dir.join("SKILL.md"))?
    );
    assert!(
        skill.dir.join("SKILL.md").exists(),
        "promote keeps the source copy"
    );

    operation_undo_inner(&state, &applied.operation_id, false)?;
    assert!(!promoted.exists(), "undo removes the promoted copy");
    Ok(())
}

#[test]
fn skills_list_reports_the_lock_that_blocks_a_row() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    let locked = sandbox.install_skill(
        &database,
        Scope::User,
        "locked-skill",
        "Locked Skill",
        SKILL_MD,
        None,
    )?;
    sandbox.install_skill(
        &database,
        Scope::User,
        "free-skill",
        "Free Skill",
        SKILL_MD,
        None,
    )?;
    at_db::locks::upsert_lock(
        database.connection(),
        &at_db::locks::LockSpec {
            item_id: &locked.item_id,
            scope: Scope::Global,
            reason: Some("approved by the team"),
            locked_by: Some("sandbox"),
        },
    )?;
    let state = sandbox.state()?;
    let view = flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: None,
            project_id: None,
        },
    )?;
    let locked_row = view
        .rows
        .iter()
        .find(|row| row.name == "Locked Skill")
        .expect("the locked skill is listed");
    assert!(locked_row.locked);
    assert_eq!(locked_row.lock_scope.as_deref(), Some("global"));
    assert_eq!(
        locked_row.lock_reason.as_deref(),
        Some("approved by the team")
    );
    let free_row = view
        .rows
        .iter()
        .find(|row| row.name == "Free Skill")
        .expect("the unlocked skill is listed");
    assert!(!free_row.locked);
    assert_eq!(free_row.lock_scope, None);
    Ok(())
}

/// One deployment cell's state, native path and recorded baseline.
fn deployment_cell(
    database: &Database,
    item_id: &str,
    agent: &AgentId,
    scope: Scope,
) -> Option<(String, Option<String>, Option<String>)> {
    database
        .connection()
        .query_row(
            "SELECT state, target_path, baseline_hash FROM deployments WHERE item_id = ?1 \
               AND agent_id = ?2 AND scope = ?3",
            rusqlite::params![item_id, agent.to_string(), scope.wire_name()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
}

fn max_operation_rowid(database: &Database) -> TestResult<i64> {
    Ok(database.connection().query_row(
        "SELECT COALESCE(MAX(rowid), 0) FROM operations",
        [],
        |row| row.get(0),
    )?)
}

/// Successful, not-yet-undone operation ids created after `baseline`, newest first.
/// Whether a path is a symbolic link (a dangling one counts).
fn is_link(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Whether nothing exists at a path.
fn is_gone(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
}

/// Canonical path of a link entry or directory, naming the path that could not be resolved.
fn resolved(path: &std::path::Path) -> TestResult<std::path::PathBuf> {
    std::fs::canonicalize(path)
        .map_err(|error| format!("{} could not be resolved: {error}", path.display()).into())
}

fn operations_after(database: &Database, baseline: i64) -> TestResult<Vec<String>> {
    let connection = database.connection();
    let mut statement = connection.prepare(
        "SELECT id FROM operations WHERE rowid > ?1 AND status = 'succeeded' \
         AND undo_of IS NULL ORDER BY rowid DESC",
    )?;
    let rows = statement.query_map([baseline], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Moves a User skill into a scope whose enabled agents read a different native root.
///
/// Regression for the relink bug: the move used to re-create the *source* scope link against the
/// target store, so the old agent folder pointed at a package the source baseline did not
/// describe and drift reconciled it as `StoreFileEditedWhileLinked`. After the fix the source
/// entry exists only in backups, both agents share one target-scope link (Codex and omp read
/// `.agents/skills`), the rows name those paths with the new baseline, and undo restores the
/// source links.
#[test]
fn a_move_redeploys_enabled_agents_at_the_target_scope_and_undo_restores_them() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut setup = sandbox.database()?;
    for agent in [AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp] {
        sandbox.register_agent(&setup, agent)?;
    }
    let project_id = sandbox.register_project(&mut setup)?;
    drop(setup);
    let database = sandbox.database()?;
    let paths = sandbox.paths()?;
    for scope in [Scope::User, Scope::Project] {
        let layout = sandbox.layout(scope)?;
        at_store::layout::init_store(scope, layout.root())?;
    }
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "shared-root-skill",
        "Shared Root",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;
    let omp_source = sandbox
        .agent_skills_dir(&AgentId::Omp, Scope::User)?
        .join(&skill.slug);
    let codex_source = sandbox
        .agent_skills_dir(&AgentId::Codex, Scope::User)?
        .join(&skill.slug);
    let omp_target = sandbox
        .agent_skills_dir(&AgentId::Omp, Scope::Project)?
        .join(&skill.slug);
    let codex_target = sandbox
        .agent_skills_dir(&AgentId::Codex, Scope::Project)?
        .join(&skill.slug);
    assert_eq!(
        omp_target, codex_target,
        "Codex and omp share one project skills root"
    );
    let project_package = sandbox
        .layout(Scope::Project)?
        .skill_dir(&at_store::layout::Slug::parse(&skill.slug)?);

    for agent in [AgentId::Omp, AgentId::Codex] {
        let preview = skill_agent_preview(
            database.connection(),
            &paths,
            SkillAgentPreviewRequest {
                item_id: skill.item_id.to_string(),
                scope: "user".to_owned(),
                agent_id: agent.to_string(),
                enabled: true,
                project_id: None,
            },
        )?;
        skill_agent_apply(&state, &preview.preview_id)?;
    }
    assert!(
        is_link(&omp_source),
        "the omp User link exists at {}",
        omp_source.display()
    );
    assert!(
        is_link(&codex_source),
        "the codex User link exists at {}",
        codex_source.display()
    );
    let baseline = max_operation_rowid(&database)?;

    let preview = flow_harness::skill_transfer_preview(
        &state,
        SkillTransferPreviewRequest {
            item_id: skill.item_id.to_string(),
            target_scope: "project".to_owned(),
            transfer: "move".to_owned(),
            resolution: None,
            new_slug: None,
            project_id: Some(project_id),
        },
    )?;
    let preview_id = preview.preview_id.expect("a free destination plans");
    flow_harness::skill_transfer_apply(&state, SkillTransferApplyRequest { preview_id })?;

    assert!(
        is_gone(&omp_source),
        "the source-scope omp link is gone, not relinked"
    );
    assert!(
        is_gone(&codex_source),
        "the source-scope codex link is gone, not relinked"
    );
    assert!(
        is_link(&omp_target),
        "the target-scope link exists at {}",
        omp_target.display()
    );
    assert_eq!(
        resolved(&omp_target)?,
        resolved(&project_package)?,
        "the target-scope link resolves to the Project package"
    );
    let shared = std::fs::read_dir(omp_target.parent().expect("skills root"))?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        shared.len(),
        1,
        "a shared root is materialised once for both agents"
    );
    let package_hash = at_store::manifest::hash_skill_folder(&project_package)?.to_string();
    for agent in [AgentId::Omp, AgentId::Codex] {
        assert_eq!(
            deployment_cell(&database, &skill.item_id.to_string(), &agent, Scope::User)
                .map(|cell| cell.0),
            Some(String::from("off")),
            "the moved source row is a tombstone, not a live link"
        );
        let (state_text, target_path, baseline_hash) = deployment_cell(
            &database,
            &skill.item_id.to_string(),
            &agent,
            Scope::Project,
        )
        .expect("the target row exists");
        assert_eq!(state_text, "linked");
        assert_eq!(
            target_path.as_deref(),
            Some(omp_target.to_string_lossy().as_ref())
        );
        assert_eq!(
            baseline_hash.as_deref(),
            Some(package_hash.as_str()),
            "the target baseline describes the target package, not the source store"
        );
    }
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "a move must not report its own writes as drift"
    );

    for operation in operations_after(&database, baseline)? {
        let check = operation_undo_check_inner(&state, &operation)?;
        if check.already_undone {
            continue;
        }
        operation_undo_inner(&state, &operation, false)?;
    }
    assert!(is_link(&omp_source), "undo restores the omp User link");
    assert!(is_link(&codex_source), "undo restores the codex User link");
    assert!(is_gone(&omp_target), "undo removes the target-scope link");
    assert!(flow_harness::reconcile_drift(&database)?.is_empty());
    Ok(())
}

/// Moves a User skill to Global, where the agent reads the same native directory.
///
/// The link must be re-pointed at the Global store and the row must carry the Global baseline;
/// re-using the source baseline is exactly what drift reported before the fix.
#[test]
fn a_move_to_a_scope_with_the_same_native_root_rebases_the_link() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let setup = sandbox.database()?;
    sandbox.register_agent(&setup, AgentId::Codex)?;
    drop(setup);
    let database = sandbox.database()?;
    let paths = sandbox.paths()?;
    for scope in [Scope::User, Scope::Global] {
        let layout = sandbox.layout(scope)?;
        at_store::layout::init_store(scope, layout.root())?;
    }
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "same-root-skill",
        "Same Root",
        SKILL_MD,
        None,
    )?;
    let state = sandbox.state()?;
    let native = sandbox
        .agent_skills_dir(&AgentId::Codex, Scope::User)?
        .join(&skill.slug);
    assert_eq!(
        native,
        sandbox
            .agent_skills_dir(&AgentId::Codex, Scope::Global)?
            .join(&skill.slug),
        "Codex reads one skills root for Global and User"
    );
    let preview = skill_agent_preview(
        database.connection(),
        &paths,
        SkillAgentPreviewRequest {
            item_id: skill.item_id.to_string(),
            scope: "user".to_owned(),
            agent_id: "codex".to_owned(),
            enabled: true,
            project_id: None,
        },
    )?;
    skill_agent_apply(&state, &preview.preview_id)?;
    let baseline = max_operation_rowid(&database)?;

    let preview = flow_harness::skill_transfer_preview(
        &state,
        SkillTransferPreviewRequest {
            item_id: skill.item_id.to_string(),
            target_scope: "global".to_owned(),
            transfer: "move".to_owned(),
            resolution: None,
            new_slug: None,
            project_id: None,
        },
    )?;
    let preview_id = preview.preview_id.expect("a free destination plans");
    flow_harness::skill_transfer_apply(&state, SkillTransferApplyRequest { preview_id })?;

    let global_package = sandbox
        .layout(Scope::Global)?
        .skill_dir(&at_store::layout::Slug::parse(&skill.slug)?);
    assert!(is_link(&native), "the shared native path is a link");
    let link_text = std::fs::read_link(&native).unwrap_or_default();
    assert!(
        resolved(&native).is_ok(),
        "link at {} -> {} does not resolve; package {} exists={}",
        native.display(),
        link_text.display(),
        global_package.display(),
        global_package.exists()
    );
    assert_eq!(
        resolved(&native)?,
        resolved(&global_package)?,
        "the shared native path now points at the Global package"
    );
    let (state_text, target_path, baseline_hash) = deployment_cell(
        &database,
        &skill.item_id.to_string(),
        &AgentId::Codex,
        Scope::Global,
    )
    .expect("the Global row exists");
    assert_eq!(state_text, "linked");
    assert_eq!(
        target_path.as_deref(),
        Some(native.to_string_lossy().as_ref())
    );
    assert_eq!(
        baseline_hash.as_deref(),
        Some(
            at_store::manifest::hash_skill_folder(&global_package)?
                .to_string()
                .as_str()
        ),
        "the baseline follows the package now linked"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the re-pointed link is not drift"
    );

    for operation in operations_after(&database, baseline)? {
        let check = operation_undo_check_inner(&state, &operation)?;
        if check.already_undone {
            continue;
        }
        operation_undo_inner(&state, &operation, false)?;
    }
    assert_eq!(
        resolved(&native)?,
        resolved(&skill.dir)?,
        "undo restores the link to the source package"
    );
    assert!(flow_harness::reconcile_drift(&database)?.is_empty());
    Ok(())
}
