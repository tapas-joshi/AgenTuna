//! TF-043: Move aside is reversible through the move journal, byte for byte.
//!
//! The sandbox registers a fake Claude Code skills folder in the index, moves an unmanaged
//! skill out of it and undoes the move. Nothing here touches a real agent folder.

mod support;

use std::fs;
use std::path::Path;

use at_db::agents::{AgentPathFormat, AgentPathKind, AgentPathSpec};
use at_model::{AgentId, Scope};
use support::{Sandbox, TestResult};

#[test]
fn moving_aside_and_undoing_restores_the_original_path_and_hash() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let skills_dir = sandbox.home.join(".claude/skills");
    fs::create_dir_all(&skills_dir)?;
    at_db::agents::upsert_agent_path(
        database.connection(),
        &AgentPathSpec {
            agent: &AgentId::ClaudeCode,
            scope: Scope::User,
            kind: AgentPathKind::SkillsDir,
            path: &skills_dir,
            format: Some(AgentPathFormat::Dir),
            is_primary: true,
            exists: true,
        },
    )?;

    let target = skills_dir.join("hand-written");
    fs::create_dir_all(&target)?;
    fs::write(target.join("SKILL.md"), b"---\nname: hand-written\n---\n")?;
    let before_hash = at_fs::sha256_hex(&fs::read(target.join("SKILL.md"))?);

    let state = sandbox.state()?;
    let moved = at_app::app::overwrite::overwrite_move_aside_inner(
        &state,
        target.to_str().expect("utf-8 target"),
    )?;
    assert!(!target.exists(), "Move aside removes the original path");
    assert_eq!(moved.original, target.display().to_string());
    let stored = Path::new(&moved.moved_to).join("SKILL.md");
    assert!(stored.exists(), "the whole tree is preserved under support");
    assert_eq!(at_fs::sha256_hex(&fs::read(&stored)?), before_hash);

    let undone = at_app::app::overwrite::overwrite_undo_move_inner(&state, &moved.operation_id)?;
    assert_eq!(undone.original, target.display().to_string());
    assert_eq!(undone.moved_back_from, moved.moved_to);
    assert_eq!(
        at_fs::sha256_hex(&fs::read(target.join("SKILL.md"))?),
        before_hash,
        "Undo restores the original path and hash"
    );
    assert!(!Path::new(&moved.moved_to).exists());

    let database = sandbox.database()?;
    let detail = at_db::ops::operation_detail(database.connection(), &moved.operation_id)?
        .expect("the move is in the operations log");
    assert_eq!(
        detail.operation.status,
        Some(at_db::ops::OperationStatus::Undone)
    );

    let repeat = at_app::app::overwrite::overwrite_undo_move_inner(&state, &moved.operation_id);
    assert!(repeat.is_err(), "an undone move cannot be undone twice");
    Ok(())
}

#[test]
fn undo_refuses_to_clobber_a_new_item_at_the_original_path() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let skills_dir = sandbox.home.join(".claude/skills");
    fs::create_dir_all(&skills_dir)?;
    at_db::agents::upsert_agent_path(
        database.connection(),
        &AgentPathSpec {
            agent: &AgentId::ClaudeCode,
            scope: Scope::User,
            kind: AgentPathKind::SkillsDir,
            path: &skills_dir,
            format: Some(AgentPathFormat::Dir),
            is_primary: true,
            exists: true,
        },
    )?;
    let target = skills_dir.join("unmanaged");
    fs::create_dir_all(&target)?;
    fs::write(target.join("SKILL.md"), b"original\n")?;

    let state = sandbox.state()?;
    let moved = at_app::app::overwrite::overwrite_move_aside_inner(
        &state,
        target.to_str().expect("utf-8 target"),
    )?;
    fs::create_dir_all(&target)?;
    fs::write(target.join("SKILL.md"), b"newer external write\n")?;

    let undo = at_app::app::overwrite::overwrite_undo_move_inner(&state, &moved.operation_id);
    assert!(undo.is_err(), "a newer external item is never clobbered");
    assert_eq!(
        fs::read(target.join("SKILL.md"))?,
        b"newer external write\n"
    );
    assert_eq!(
        fs::read(Path::new(&moved.moved_to).join("SKILL.md"))?,
        b"original\n",
        "the moved-aside copy is still preserved"
    );
    Ok(())
}

#[test]
fn adopting_refuses_a_source_that_changed_after_the_preview() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let skills_dir = sandbox.home.join(".claude/skills");
    fs::create_dir_all(&skills_dir)?;
    at_db::agents::upsert_agent_path(
        database.connection(),
        &AgentPathSpec {
            agent: &AgentId::ClaudeCode,
            scope: Scope::User,
            kind: AgentPathKind::SkillsDir,
            path: &skills_dir,
            format: Some(AgentPathFormat::Dir),
            is_primary: true,
            exists: true,
        },
    )?;
    let target = skills_dir.join("reviewed-once");
    fs::create_dir_all(&target)?;
    let skill_md = target.join("SKILL.md");
    fs::write(
        &skill_md,
        b"---\nname: reviewed-once\ndescription: demo\n---\n\nReviewed body.\n",
    )?;
    let path = target.to_str().expect("utf-8 target").to_owned();

    let state = sandbox.state()?;
    let preview = at_app::app::overwrite::adopt_preview_inner(
        &state,
        &path,
        None,
        Some("link".to_owned()),
        None,
    )?;
    assert_eq!(preview.current, "present");
    assert_eq!(
        preview.hash.len(),
        64,
        "the preview carries the digest it verified: {:?}",
        preview.hash
    );

    // The agent's file changes between the review and the click.
    fs::write(
        &skill_md,
        b"---\nname: reviewed-once\ndescription: demo\n---\n\nUnreviewed body.\n",
    )?;

    let applied = at_app::app::overwrite::adopt_apply_inner(
        &state,
        &path,
        Some(preview.hash.clone()),
        Some("link".to_owned()),
        None,
    );
    match applied {
        Ok(outcome) => panic!("a changed source must not be adopted: {outcome:?}"),
        Err(error) => assert_eq!(
            error.code,
            at_app::app::commands::CommandErrorCode::Conflict,
            "the apply reports the stale preview as a conflict: {error:?}"
        ),
    }
    assert!(
        target.is_dir() && !target.is_symlink(),
        "the refused apply leaves the native item untouched"
    );
    assert_eq!(
        fs::read(&skill_md)?,
        b"---\nname: reviewed-once\ndescription: demo\n---\n\nUnreviewed body.\n"
    );
    Ok(())
}
#[test]
fn codex_native_skill_can_be_previewed_for_adoption() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::Codex)?;
    let skills_dir = sandbox.home.join(".codex/skills");
    let target = skills_dir.join("codex-active");
    fs::create_dir_all(&target)?;
    fs::write(
        target.join("SKILL.md"),
        b"---\nname: codex-active\ndescription: native Codex skill\n---\n\nNative skill.\n",
    )?;
    let state = sandbox.state()?;

    let preview = at_app::app::overwrite::adopt_preview_inner(
        &state,
        target.to_str().expect("utf-8 target"),
        None,
        None,
        None,
    )?;

    assert_eq!(preview.native_name, "codex-active");
    assert_eq!(preview.current, "present");
    assert!(!preview.hash.is_empty());
    Ok(())
}
#[test]
fn shared_project_skill_can_be_adopted_backed_up_and_undone() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_project(&mut database)?;
    for agent in [AgentId::Codex, AgentId::Omp] {
        sandbox.register_agent(&database, agent.clone())?;
    }
    let shared_root = sandbox.project.join(".agents/skills");
    fs::create_dir_all(&shared_root)?;
    for agent in [AgentId::Codex, AgentId::Omp] {
        at_db::agents::upsert_agent_path(
            database.connection(),
            &AgentPathSpec {
                agent: &agent,
                scope: Scope::Project,
                kind: AgentPathKind::SkillsDir,
                path: &shared_root,
                format: Some(AgentPathFormat::Dir),
                is_primary: true,
                exists: true,
            },
        )?;
    }
    let target = shared_root.join("release-notes");
    fs::create_dir_all(&target)?;
    let original =
        b"---\nname: release-notes\ndescription: shared native skill\n---\n\nRelease notes.\n";
    fs::write(target.join("SKILL.md"), original)?;
    let path = target.to_string_lossy().into_owned();
    let state = sandbox.project_state()?;

    let preview = at_app::app::overwrite::adopt_preview_inner(
        &state,
        &path,
        None,
        Some("link".to_owned()),
        None,
    )?;
    assert_eq!(preview.native_name, "release-notes");
    assert!(preview.backup_dir.contains("backup"));
    assert_eq!(preview.agents.len(), 2);
    let applied = at_app::app::overwrite::adopt_apply_inner(
        &state,
        &path,
        Some(preview.hash),
        Some("link".to_owned()),
        None,
    )?;
    let backup = sandbox.support.join("backups");
    assert!(backup.exists(), "adoption creates its backup root");
    at_app::app::overwrite::operation_undo_inner(&state, &applied.operation_id, false)?;
    assert_eq!(fs::read(target.join("SKILL.md"))?, original);
    Ok(())
}

#[test]
fn a_project_skill_root_authorizes_adoption_without_any_agent_path_row() -> TestResult<()> {
    // Codex and omp both read `<project>/.agents/skills`; the adapters declare that root, so a
    // scan finds the item and the refusal dialog offers Adopt before any agent row records it.
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_project(&mut database)?;
    let shared_root = sandbox.project.join(".agents/skills");
    let target = shared_root.join("shared-notes");
    fs::create_dir_all(&target)?;
    fs::write(
        target.join("SKILL.md"),
        b"---\nname: shared-notes\ndescription: shared native skill\n---\n\nBody.\n",
    )?;
    let state = sandbox.project_state()?;

    let preview = at_app::app::overwrite::adopt_preview_inner(
        &state,
        target.to_str().expect("utf-8 target"),
        None,
        None,
        None,
    )?;

    assert_eq!(preview.native_name, "shared-notes");
    assert_eq!(preview.current, "present");
    assert_eq!(
        Some(preview.hash.as_str()),
        at_fs::changeset::hash_node(&target)?.as_deref(),
        "the preview digest is the representation the apply re-checks"
    );
    Ok(())
}

#[test]
fn adopt_refuses_a_project_path_outside_the_shared_skill_root() -> TestResult<()> {
    // The shared root is `.agents/skills`, not `.agents` or the project root: a neighbouring
    // folder in the same project stays refused even though the scan can read it.
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_project(&mut database)?;
    let sibling = sandbox.project.join(".agents/notes/loose");
    fs::create_dir_all(&sibling)?;
    fs::write(sibling.join("SKILL.md"), b"---\nname: loose\n---\n")?;
    let state = sandbox.project_state()?;

    let error = at_app::app::overwrite::adopt_preview_inner(
        &state,
        sibling.to_str().expect("utf-8 target"),
        None,
        None,
        None,
    )
    .expect_err("a path outside every registered root is refused");

    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument
    );
    assert!(
        error.message.contains("outside registered agent paths"),
        "the refusal names the rule: {error:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_symlinked_project_skill_that_escapes_its_root_is_refused() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let mut database = sandbox.database()?;
    sandbox.register_project(&mut database)?;
    let shared_root = sandbox.project.join(".agents/skills");
    let outside = sandbox.project.join("outside");
    fs::create_dir_all(&shared_root)?;
    fs::create_dir_all(&outside)?;
    fs::write(
        outside.join("SKILL.md"),
        b"---\nname: escaped\n---\n\nOutside the root.\n",
    )?;
    let target = shared_root.join("escaped");
    std::os::unix::fs::symlink(&outside, &target)?;
    let state = sandbox.project_state()?;

    let error = at_app::app::overwrite::adopt_preview_inner(
        &state,
        target.to_str().expect("utf-8 target"),
        None,
        None,
        None,
    )
    .expect_err("a symlink that leaves the root is refused");

    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::InvalidArgument
    );
    Ok(())
}

#[test]
fn adopt_authorized_roots_authorizes_backup_dir_outside_home() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("fake_home");
    let support = temp.path().join("isolated_support");
    let user_store = temp.path().join("isolated_user_store");
    let global_store = temp.path().join("isolated_global");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&support)?;
    fs::create_dir_all(&user_store)?;
    fs::create_dir_all(&global_store)?;

    let paths = at_store::paths::AppPaths::resolve(&at_store::paths::PathEnvironment {
        home: at_store::paths::HomeSource::Injected(home),
        overrides: at_store::paths::PathOverrides {
            home: Some(support.clone()),
            user_store: Some(user_store),
            global_store: Some(global_store),
            project_store: None,
        },
        project_root: None,
    })?;

    let roots = at_app::app::overwrite::adopt_authorized_roots(&paths)?;
    let canonical_backup = fs::canonicalize(paths.backup_dir())?;
    assert!(
        roots
            .roots()
            .iter()
            .any(|r| canonical_backup.starts_with(r)),
        "backup_dir outside home must be included in authorized roots: roots={:?}, backup_dir={:?}",
        roots.roots(),
        canonical_backup
    );
    Ok(())
}
