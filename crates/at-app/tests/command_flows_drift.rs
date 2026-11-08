//! Sync & drift flows through the real at-app command bodies.
//!
//! A genuine external edit is detected once, previewed (the dry run the dialog shows), then
//! resolved by restoring the canonical bytes or by adopting the on-disk bytes. Every command
//! leaves a reconcile with no open events.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use at_app::app::flow_harness::{self, skill_agent_apply, skill_agent_preview};
use at_app::app::skill_agent::SkillAgentPreviewRequest;
use at_model::{AgentId, Scope};
use support::{Sandbox, TestResult};

const SKILL_MD: &[u8] = b"---\nname: Drift Skill\ndescription: sandbox\n---\n\nCanonical body.\n";

/// Enables one skill for Claude Code and returns its deployed path together with the sandbox.
fn enabled_skill() -> TestResult<(Sandbox, String, std::path::PathBuf)> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let skill = sandbox.install_skill(
        &database,
        Scope::User,
        "drift-skill",
        "Drift Skill",
        SKILL_MD,
        None,
    )?;
    let paths = sandbox.paths()?;
    let target = sandbox
        .agent_skills_dir(&AgentId::ClaudeCode, Scope::User)?
        .join(&skill.slug);
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
    skill_agent_apply(&state, &preview.preview_id)?;
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the enable left no drift"
    );
    Ok((sandbox, skill.item_id.to_string(), target))
}

/// Replaces a deployed link with a real folder holding `bytes`, as an external tool would.
fn replace_link_with_folder(target: &std::path::Path, bytes: &[u8]) -> TestResult<()> {
    std::fs::remove_file(target)?;
    std::fs::create_dir_all(target)?;
    std::fs::write(target.join("SKILL.md"), bytes)?;
    Ok(())
}

#[test]
fn an_external_edit_is_previewed_then_restored_with_zero_drift_left() -> TestResult<()> {
    let (sandbox, _, target) = enabled_skill()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;

    let external = b"---\nname: Drift Skill\n---\n\nEdited outside AgenTuna.\n";
    replace_link_with_folder(&target, external)?;

    // ---- detection ---------------------------------------------------------------------
    let notices = flow_harness::reconcile_drift(&database)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].path, target);
    let open = flow_harness::open_drift_events(&database)?;
    assert_eq!(open.len(), 1, "one actionable row");
    let event_id = open[0].id;

    // ---- dry run -----------------------------------------------------------------------
    let before_preview = sandbox.snapshot()?;
    let preview = flow_harness::drift_preview(&state, event_id)?;
    assert_eq!(preview.event_id, event_id);
    assert_eq!(preview.path, target.display().to_string());
    assert!(
        preview.previous_version_available,
        "the canonical bytes are still available: {preview:?}"
    );
    assert_eq!(preview.baseline_unavailable_reason, None);
    assert!(
        preview.diff.contains("Edited outside AgenTuna"),
        "{}",
        preview.diff
    );
    let on_disk = preview.on_disk_content.as_deref().unwrap_or_default();
    assert!(
        on_disk.contains("Edited outside AgenTuna"),
        "the preview shows the on-disk bytes: {on_disk}"
    );
    before_preview.assert_unchanged(&sandbox.snapshot()?, "a drift preview (dry run)");

    // ---- restore -----------------------------------------------------------------------
    flow_harness::drift_restore(&state, event_id)?;
    // The store package is unchanged; the deployed path holds the canonical bytes again.
    let store_file = sandbox
        .layout(Scope::User)?
        .skill_dir(&at_store::layout::Slug::parse("drift-skill")?)
        .join("SKILL.md");
    assert_eq!(std::fs::read(&store_file)?, SKILL_MD);
    assert_eq!(std::fs::read(target.join("SKILL.md"))?, SKILL_MD);
    assert!(
        flow_harness::open_drift_events(&database)?.is_empty(),
        "the resolved event is closed"
    );
    let leftover = flow_harness::reconcile_drift(&database)?;
    assert!(
        leftover.is_empty(),
        "restore left no new drift: {leftover:?}"
    );

    // ---- the same event cannot be resolved twice ----------------------------------------
    let error = flow_harness::drift_restore(&state, event_id)
        .expect_err("a closed event cannot be resolved again");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::NotFound,
        "{error:?}"
    );
    Ok(())
}

#[test]
fn adopting_an_external_edit_updates_the_canonical_copy() -> TestResult<()> {
    let (sandbox, _, target) = enabled_skill()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;

    let external = b"---\nname: Drift Skill\n---\n\nAdopt this body.\n";
    replace_link_with_folder(&target, external)?;
    let notices = flow_harness::reconcile_drift(&database)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let event_id = flow_harness::open_drift_events(&database)?[0].id;

    flow_harness::drift_adopt(&state, event_id)?;
    let store_file = sandbox
        .layout(Scope::User)?
        .skill_dir(&at_store::layout::Slug::parse("drift-skill")?)
        .join("SKILL.md");
    assert_eq!(
        std::fs::read(&store_file)?,
        external,
        "adopt copies the on-disk bytes into the canonical store"
    );
    assert!(flow_harness::open_drift_events(&database)?.is_empty());
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the adopted bytes became the new baseline"
    );

    // A later genuine edit is still detected, so adopt did not disable detection.
    std::fs::write(
        target.join("SKILL.md"),
        b"---\nname: Drift Skill\n---\n\nThird body.\n",
    )?;
    assert_eq!(
        flow_harness::reconcile_drift(&database)?.len(),
        1,
        "a new external edit opens a new event"
    );
    Ok(())
}

/// Builds a relative link target from `from_dir` to `destination`, using `..` components.
fn relative_link(from_dir: &std::path::Path, destination: &std::path::Path) -> std::path::PathBuf {
    let from = from_dir.components().collect::<Vec<_>>();
    let to = destination.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = std::path::PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

/// Replaces the deployed symlink with an equivalent relative link to the same destination.
fn replace_link_with_relative(
    target: &std::path::Path,
    destination: &std::path::Path,
) -> TestResult<()> {
    std::fs::remove_file(target)?;
    let parent = target.parent().ok_or("the deployment link has no parent")?;
    std::os::unix::fs::symlink(relative_link(parent, destination), target)?;
    Ok(())
}

/// Runs one external edit through preview, adopt and restore for the deployed link shape.
fn drift_cycle(relative: bool) -> TestResult<()> {
    let (sandbox, _, target) = enabled_skill()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;
    let store_dir = sandbox
        .layout(Scope::User)?
        .skill_dir(&at_store::layout::Slug::parse("drift-skill")?);
    if relative {
        replace_link_with_relative(&target, &store_dir)?;
        let observed = std::fs::read_link(&target)?;
        assert!(
            !observed.is_absolute(),
            "the managed link is stored relative: {}",
            observed.display()
        );
        assert!(
            observed
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
            "the relative link climbs with ..: {}",
            observed.display()
        );
    } else {
        // The app stores new links relatively; rewrite this one absolutely to cover that shape.
        std::fs::remove_file(&target)?;
        std::os::unix::fs::symlink(&store_dir, &target)?;
        assert!(
            std::fs::read_link(&target)?.is_absolute(),
            "the managed link is stored absolute"
        );
    }
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "a link that resolves to the store is in sync"
    );

    // ---- external edit, preview, adopt --------------------------------------------------
    let first = b"---\nname: Drift Skill\n---\n\nFirst external edit.\n";
    std::fs::write(target.join("SKILL.md"), first)?;
    let notices = flow_harness::reconcile_drift(&database)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let event_id = flow_harness::open_drift_events(&database)?[0].id;

    let preview = flow_harness::drift_preview(&state, event_id)?;
    assert_eq!(preview.path, target.display().to_string());
    assert!(
        preview
            .on_disk_content
            .as_deref()
            .unwrap_or_default()
            .contains("First external edit"),
        "the preview shows the bytes the link resolves to: {preview:?}"
    );
    flow_harness::drift_adopt(&state, event_id)?;
    assert_eq!(
        std::fs::read(store_dir.join("SKILL.md"))?,
        first,
        "adopt copies the edited bytes into the canonical store"
    );
    assert!(
        flow_harness::open_drift_events(&database)?.is_empty(),
        "adopt closes the row"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the adopted bytes became the new baseline"
    );

    // ---- second edit, preview, restore --------------------------------------------------
    let second = b"---\nname: Drift Skill\n---\n\nSecond external edit.\n";
    std::fs::write(target.join("SKILL.md"), second)?;
    let notices = flow_harness::reconcile_drift(&database)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let event_id = flow_harness::open_drift_events(&database)?[0].id;

    let preview = flow_harness::drift_preview(&state, event_id)?;
    assert!(
        preview
            .on_disk_content
            .as_deref()
            .unwrap_or_default()
            .contains("Second external edit"),
        "{preview:?}"
    );
    flow_harness::drift_restore(&state, event_id)?;
    assert_eq!(
        std::fs::read(target.join("SKILL.md"))?,
        std::fs::read(store_dir.join("SKILL.md"))?,
        "the deployed bytes are the canonical bytes"
    );
    assert!(
        flow_harness::open_drift_events(&database)?.is_empty(),
        "restore closes the row"
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "restore left no new drift"
    );
    Ok(())
}

#[test]
fn a_relative_link_with_parent_components_previews_adopts_and_restores() -> TestResult<()> {
    drift_cycle(true)
}

#[test]
fn an_absolute_link_previews_adopts_and_restores() -> TestResult<()> {
    drift_cycle(false)
}

#[test]
fn a_link_replaced_by_a_file_still_reports_drift() -> TestResult<()> {
    let (sandbox, _, target) = enabled_skill()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;

    let external = b"---\nname: Drift Skill\n---\n\nA real folder replaced the link.\n";
    replace_link_with_folder(&target, external)?;
    assert_eq!(
        flow_harness::reconcile_drift(&database)?.len(),
        1,
        "a content difference is drift even when the link is gone"
    );
    let snapshot = flow_harness::drift_snapshot(&state)?;
    let row = snapshot.rows.first().ok_or("one open drift row")?;
    assert_eq!(row.kind, "link_replaced_by_file", "{row:?}");
    assert!(row.can_restore, "{row:?}");
    let event_id = row.event_id;

    let preview = flow_harness::drift_preview(&state, event_id)?;
    assert!(
        preview
            .on_disk_content
            .as_deref()
            .unwrap_or_default()
            .contains("A real folder replaced the link"),
        "{preview:?}"
    );
    flow_harness::drift_restore(&state, event_id)?;
    assert_eq!(std::fs::read(target.join("SKILL.md"))?, SKILL_MD);
    assert!(flow_harness::open_drift_events(&database)?.is_empty());
    assert!(flow_harness::reconcile_drift(&database)?.is_empty());
    Ok(())
}

#[test]
fn a_managed_link_pointing_elsewhere_is_a_conflict_not_accepted() -> TestResult<()> {
    let (sandbox, _, target) = enabled_skill()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;
    let store_dir = sandbox
        .layout(Scope::User)?
        .skill_dir(&at_store::layout::Slug::parse("drift-skill")?);

    let elsewhere = sandbox.home.join("elsewhere-skill");
    std::fs::create_dir_all(&elsewhere)?;
    std::fs::write(
        elsewhere.join("SKILL.md"),
        b"---\nname: Other Skill\n---\n\nNot the managed copy.\n",
    )?;
    replace_link_with_relative(&target, &elsewhere)?;

    assert_eq!(
        flow_harness::reconcile_drift(&database)?.len(),
        1,
        "the relocated link still differs from the baseline"
    );
    let event_id = flow_harness::open_drift_events(&database)?[0].id;
    let error = flow_harness::drift_preview(&state, event_id)
        .expect_err("a link that points somewhere else is refused");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::Conflict,
        "{error:?}"
    );
    assert!(
        error.message.contains("points somewhere unexpected"),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read(store_dir.join("SKILL.md"))?,
        SKILL_MD,
        "the canonical copy is untouched"
    );
    Ok(())
}
