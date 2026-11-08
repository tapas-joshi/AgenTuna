//! GUI-path rule (instruction file) flows through the real at-app command bodies.
//!
//! Creating, previewing and saving native rule files, the typed read-only refusal for the
//! Global scope, and the concurrent-external-edit refusal the editor dialog renders.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;

use at_app::app::flow_harness;
use at_app::app::rule_create::InstructionCreateRequest;
use at_app::app::workflows::InstructionSaveRequest;
use at_model::{AgentId, ItemKind, Scope};
use support::{Sandbox, TestResult};

/// The indexed item id for the single rule file.
fn rule_item_id(database: &at_db::Database) -> String {
    database
        .connection()
        .query_row(
            "SELECT id FROM items WHERE kind = 'rule' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("rule item row")
}

#[test]
fn creating_and_saving_a_rule_file_is_reviewed_and_conflict_safe() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    sandbox.register_agent(&database, AgentId::ClaudeCode)?;
    let state = sandbox.state()?;
    sandbox.setup_scope(&state, "user", None)?;
    let before = sandbox.snapshot()?;

    // ---- options and preview write nothing ---------------------------------------------
    let options = flow_harness::instruction_create_options(&state, None)?;
    assert!(
        options.global_writable,
        "the sandbox Global store is writable"
    );
    assert!(!options.has_project, "no project is registered yet");
    assert!(!options.local_available);
    assert_eq!(
        options.local_reason.as_deref(),
        Some("select a project to use the Local scope")
    );

    let preview = flow_harness::instruction_create_preview(&state, "CLAUDE.md", "user", None)?;
    assert!(!preview.exists, "the file does not exist yet");
    let path = preview.path.clone().expect("a user rule path is known");
    let expected_path = sandbox.agent_path(&AgentId::ClaudeCode, ItemKind::Rule, Scope::User)?;
    assert_eq!(path, expected_path.display().to_string());
    before.assert_unchanged(&sandbox.snapshot()?, "a rule create preview");

    // ---- create ------------------------------------------------------------------------
    let created = flow_harness::instruction_create(
        &state,
        InstructionCreateRequest {
            file_type: "CLAUDE.md".to_owned(),
            scope: "user".to_owned(),
        },
        None,
    )?;
    assert_eq!(created, path, "create returns the reviewed path");
    let first = std::fs::read(&expected_path)?;
    assert!(!first.is_empty(), "the template is written");
    let item_id = rule_item_id(&database);
    assert_eq!(
        database.connection().query_row(
            "SELECT source_path FROM items WHERE id = ?1",
            [item_id.clone()],
            |row| row.get::<_, String>(0)
        )?,
        path
    );

    // ---- save with the current hash ----------------------------------------------------
    // A created rule file is AgenTuna-managed from birth, and the editor buffer must keep the
    // markers so only the managed body changes.
    let template = String::from_utf8_lossy(&first).into_owned();
    assert!(
        template.starts_with("<!-- agentuna:begin -->"),
        "{template}"
    );
    assert!(
        template.trim_end().ends_with("<!-- agentuna:end -->"),
        "{template}"
    );
    let render = |body: &[u8]| -> TestResult<String> {
        Ok(String::from_utf8(
            at_fs::managed_block::render_managed_block(
                std::path::Path::new(&path),
                Some(&first),
                body,
            )?,
        )?)
    };
    let buffer = render(b"# CLAUDE.md\n\nExtra line from the editor.")?;
    assert_ne!(buffer, template, "the buffer really changes the body");
    let saved = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id: item_id.clone(),
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: at_fs::sha256_hex(&first),
            content: buffer.clone(),
            project_id: None,
            force: false,
        },
    )?;
    assert_eq!(saved.status, "saved");
    assert_eq!(std::fs::read_to_string(&expected_path)?, buffer);
    assert_eq!(
        saved.hash.as_deref(),
        Some(at_fs::sha256_hex(buffer.as_bytes()).as_str())
    );
    assert!(
        flow_harness::reconcile_drift(&database)?.is_empty(),
        "the save refreshed the baseline"
    );

    // ---- a concurrent external edit is reported, not overwritten -----------------------
    let external = render(b"# CLAUDE.md\n\nExternal edit made while the editor stayed open.")?;
    std::fs::write(&expected_path, &external)?;
    let conflict = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id: item_id.clone(),
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: at_fs::sha256_hex(buffer.as_bytes()),
            content: render(b"# CLAUDE.md\n\nEditor buffer that must not win.")?,
            project_id: None,
            force: false,
        },
    )?;
    assert_eq!(conflict.status, "conflict");
    let details = conflict.conflict.expect("the dialog needs the diff");
    assert_eq!(details.disk_content, external);
    assert_eq!(details.path, path);
    assert_eq!(std::fs::read_to_string(&expected_path)?, external);

    // ---- explicit Overwrite after the user reviewed the conflict ------------------------
    let kept = render(b"# CLAUDE.md\n\nEditor buffer the user chose to keep.")?;
    let forced = flow_harness::instruction_save(
        &state,
        InstructionSaveRequest {
            item_id,
            path: path.clone(),
            scope: "user".to_owned(),
            expected_hash: details.disk_hash,
            content: kept.clone(),
            project_id: None,
            force: true,
        },
    )?;
    assert_eq!(forced.status, "saved");
    assert_eq!(std::fs::read_to_string(&expected_path)?, kept);
    assert!(
        kept.contains("<!-- agentuna:begin -->") && kept.contains("<!-- agentuna:end -->"),
        "the markers survive the forced save"
    );
    Ok(())
}

#[test]
fn a_read_only_global_store_refuses_rule_creation_with_a_typed_error() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let database = sandbox.database()?;
    let state = sandbox.state()?;

    // Global writes are gated by the store's own writability, exactly as the privilege screen
    // reports it.
    std::fs::set_permissions(&sandbox.global, std::fs::Permissions::from_mode(0o555))?;
    let options = flow_harness::instruction_create_options(&state, None)?;
    assert!(!options.global_writable, "the probe reports read-only");
    assert!(options.global_reason.is_some());

    let before = sandbox.snapshot()?;
    let error = flow_harness::instruction_create(
        &state,
        InstructionCreateRequest {
            file_type: "AGENTS.md".to_owned(),
            scope: "global".to_owned(),
        },
        None,
    )
    .expect_err("a read-only Global store refuses creation");
    assert_eq!(
        error.code,
        at_app::app::commands::CommandErrorCode::Locked,
        "{error:?}"
    );
    before.assert_unchanged(&sandbox.snapshot()?, "a read-only Global create");
    let items: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
    assert_eq!(items, 0, "no item row is written for a refused create");

    // The preview only resolves the path; the writability signal the dialog disables the card
    // with is `global_writable`, asserted above.
    let preview = flow_harness::instruction_create_preview(&state, "AGENTS.md", "global", None)?;
    assert!(!preview.exists);
    std::fs::set_permissions(&sandbox.global, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}
