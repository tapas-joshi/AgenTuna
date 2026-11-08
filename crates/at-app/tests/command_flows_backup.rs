//! Backups, scope setup and read-only refusals through the real at-app command bodies.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;

use at_app::app::drift_commands::BackupRestoreRequest;
use at_app::app::flow_harness;
use at_app::app::overview::{overview_setup_apply_inner, overview_setup_preview_inner};
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::rule_create::InstructionCreateRequest;
use at_app::app::workflows::InstructionSaveRequest;
use at_model::{AgentId, ItemKind, Scope};
use support::{Sandbox, TestResult};

/// Creates one user rule file and returns its path and indexed id.
fn created_rule(
    state: &at_app::app::state::AppState,
    sandbox: &Sandbox,
) -> TestResult<(String, String)> {
    let path = sandbox
        .agent_path(&AgentId::ClaudeCode, ItemKind::Rule, Scope::User)?
        .display()
        .to_string();
    let created = flow_harness::instruction_create(
        state,
        InstructionCreateRequest {
            file_type: "CLAUDE.md".to_owned(),
            scope: "user".to_owned(),
        },
        None,
    )?;
    assert_eq!(created, path);
    let database = sandbox.database()?;
    let item_id: String = database.connection().query_row(
        "SELECT id FROM items WHERE kind = 'rule' LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    Ok((path, item_id))
}

#[test]
fn a_saved_overwrite_is_listed_previewed_restored_and_undone() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let (path, item_id) = created_rule(&state, &sandbox)?;
    let original = std::fs::read(&path)?;

    // The second save replaces the managed body, so the pipeline backs the first version up.
    let replacement = String::from_utf8(at_fs::managed_block::render_managed_block(
        std::path::Path::new(&path),
        Some(&original),
        b"# CLAUDE.md\n\nA reviewed addition.",
    )?)?;
    assert_ne!(
        replacement,
        String::from_utf8_lossy(&original),
        "the body changes"
    );
    let saved = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id,
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: at_fs::sha256_hex(&original),
            content: replacement.clone(),
            project_id: None,
            force: false,
        },
    )?;
    assert_eq!(saved.status, "saved");
    let backup_path = saved
        .backup_path
        .clone()
        .expect("the overwrite is backed up");
    assert!(std::path::Path::new(&backup_path).exists());

    // ---- list ----------------------------------------------------------------------------
    let rows = flow_harness::backups_list(&state)?;
    let row = rows
        .iter()
        .find(|row| row.original_path.as_deref() == Some(path.as_str()))
        .expect("the overwritten file is catalogued");
    assert!(row.restorable, "{row:?}");
    assert_eq!(row.unavailable_reason, None);

    // ---- preview --------------------------------------------------------------------------
    let preview = flow_harness::backup_preview(&state, &row.id)?;
    assert!(preview.destination_exists);
    assert!(
        preview.diff.contains("A reviewed addition."),
        "the diff shows what the restore removes: {}",
        preview.diff
    );
    assert!(!preview.redacted, "nothing secret in this fixture");

    // ---- restore ---------------------------------------------------------------------------
    let restored = flow_harness::backup_restore(
        &state,
        &BackupRestoreRequest {
            backup_id: row.id.clone(),
            preview_id: preview.preview_id.clone(),
            destination_hash: preview.destination_hash.clone(),
        },
    )?;
    assert_eq!(restored.path, path);
    assert_eq!(std::fs::read(&path)?, original, "the saved bytes are back");
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "a restore is not reported as drift"
    );

    // ---- undo the restore --------------------------------------------------------------------
    operation_undo_inner(&state, &restored.operation_id, false)?;
    assert_eq!(
        std::fs::read_to_string(&path)?,
        replacement,
        "undo puts the reviewed save back"
    );
    Ok(())
}

#[test]
fn scope_setup_is_previewed_applied_and_undone() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let state = sandbox.state()?;
    let before = sandbox.snapshot()?;

    let preview = overview_setup_preview_inner(&state, "user", None)?;
    assert!(!preview.already_ready);
    assert!(!preview.files.is_empty(), "the plan lists what it creates");
    before.assert_unchanged(&sandbox.snapshot()?, "a scope setup preview");

    let preview_id = preview
        .preview_id
        .expect("a new store needs a reviewed plan");
    let applied = overview_setup_apply_inner(&state, &preview_id, true)?;
    assert!(sandbox.user_store.join("store.json").exists());

    // The same action on a ready scope is a no-op, not a second operation.
    let again = overview_setup_preview_inner(&state, "user", None)?;
    assert!(again.already_ready);
    assert_eq!(again.preview_id, None);

    operation_undo_inner(&state, &applied.operation_id, false)?;
    assert!(!sandbox.user_store.join("store.json").exists());
    before.assert_unchanged(&sandbox.snapshot()?, "undo of a scope setup");
    Ok(())
}

#[test]
fn a_restore_into_a_read_only_folder_is_refused_and_changes_nothing() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let (path, item_id) = created_rule(&state, &sandbox)?;
    let original = std::fs::read(&path)?;
    let saved = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id,
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: at_fs::sha256_hex(&original),
            content: String::from_utf8(at_fs::managed_block::render_managed_block(
                std::path::Path::new(&path),
                Some(&original),
                b"# CLAUDE.md\n\nchanged",
            )?)?,
            project_id: None,
            force: false,
        },
    )?;
    assert_eq!(saved.status, "saved");
    let row = flow_harness::backups_list(&state)?
        .into_iter()
        .find(|row| row.original_path.as_deref() == Some(path.as_str()))
        .expect("catalogued");

    let parent = std::path::Path::new(&path)
        .parent()
        .expect("rule path has a parent")
        .to_path_buf();
    let preview = flow_harness::backup_preview(&state, &row.id)?;
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555))?;
    let before = sandbox.snapshot()?;
    let result = flow_harness::backup_restore(
        &state,
        &BackupRestoreRequest {
            backup_id: row.id.clone(),
            preview_id: preview.preview_id.clone(),
            destination_hash: preview.destination_hash.clone(),
        },
    );
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))?;

    let error = result.expect_err("a read-only destination folder refuses the restore");
    assert!(
        matches!(
            error.code,
            at_app::app::commands::CommandErrorCode::Unavailable
                | at_app::app::commands::CommandErrorCode::Conflict
                | at_app::app::commands::CommandErrorCode::Internal
        ),
        "the refusal is typed: {error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a refused backup restore");
    Ok(())
}

#[test]
fn a_restore_after_an_external_edit_is_refused_as_stale() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let (path, item_id) = created_rule(&state, &sandbox)?;
    let original = std::fs::read(&path)?;
    let saved = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id,
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: at_fs::sha256_hex(&original),
            content: String::from_utf8(at_fs::managed_block::render_managed_block(
                std::path::Path::new(&path),
                Some(&original),
                b"# CLAUDE.md\n\nfirst reviewed save",
            )?)?,
            project_id: None,
            force: false,
        },
    )?;
    assert_eq!(saved.status, "saved");
    let row = flow_harness::backups_list(&state)?
        .into_iter()
        .find(|row| row.original_path.as_deref() == Some(path.as_str()))
        .expect("catalogued");
    let preview = flow_harness::backup_preview(&state, &row.id)?;
    let external = b"# CLAUDE.md\n\nedited outside AgenTuna\n";
    std::fs::write(&path, external)?;

    let result = flow_harness::backup_restore(
        &state,
        &BackupRestoreRequest {
            backup_id: row.id,
            preview_id: preview.preview_id,
            destination_hash: preview.destination_hash,
        },
    );
    let error = result.expect_err("a destination edited after the preview is stale");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::ExternalWriteConflict
    );
    assert_eq!(
        std::fs::read(&path)?,
        external,
        "the external edit is preserved"
    );
    Ok(())
}
