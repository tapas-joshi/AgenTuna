//! TC-030 end-to-end tests: `migrate` and `adopt` against a sandboxed fake home.
//!
//! Every test runs the real `agentuna` binary with all roots redirected into a
//! temporary directory, so nothing touches the developer's home, store or agent
//! folders.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use support::{Sandbox, agentuna, exit_code, stderr, stdout};

/// A skill document the scanner accepts.
fn skill_doc(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n")
}

/// Writes `contents` creating parents.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// Seeds a fake home with one plain skill and one MCP server, and initializes the
/// user store and index.
fn seed_simple(sandbox: &Sandbox) -> (PathBuf, PathBuf) {
    let home = sandbox.path("home");
    let alpha = home.join(".claude/skills/alpha");
    write(
        &alpha.join("SKILL.md"),
        &skill_doc("alpha", "A plain native skill"),
    );
    let claude_json = home.join(".claude.json");
    write(
        &claude_json,
        "{\n  \"mcpServers\": {\n    \"docs\": {\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"docs-server\"]\n    }\n  }\n}\n",
    );
    init_store(sandbox);
    (alpha, claude_json)
}

/// Seeds the simple home plus two copies of `dup` whose bytes differ, which is a
/// migration content conflict.
fn seed_conflict(sandbox: &Sandbox) {
    let home = sandbox.path("home");
    write(
        &home.join(".claude/skills/dup/SKILL.md"),
        &skill_doc("dup", "Claude copy of the conflicting skill"),
    );
    write(
        &home.join(".codex/skills/dup/SKILL.md"),
        &skill_doc("dup", "Codex copy of the conflicting skill"),
    );
}

/// Initializes the user store the way `doctor` would.
fn init_store(sandbox: &Sandbox) {
    let _ = fs::create_dir_all(sandbox.support());
    at_store::layout::init_store(at_model::Scope::User, sandbox.user_store())
        .expect("initialize the sandbox user store");
}

/// Parses stdout as exactly one JSON envelope.
fn json_envelope(output: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(output)).expect("one JSON envelope on stdout")
}

/// Fails with context when `output` did not exit with `expected`.
fn assert_exit(output: &std::process::Output, expected: i32) -> Value {
    let actual = exit_code(output);
    let envelope = json_envelope(output);
    assert_eq!(
        actual,
        expected,
        "exit {expected} expected, got {actual}; stderr: {}\nstdout: {}",
        stderr(output),
        stdout(output)
    );
    envelope
}

/// Recursive content snapshot: relative path -> kind plus hash or link target.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read_dir").flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("prefix")
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).expect("symlink_metadata");
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("read_link");
                entries.insert(relative, format!("symlink:{}", target.display()));
            } else if metadata.is_dir() {
                entries.insert(relative, "dir".to_owned());
                pending.push(path);
            } else {
                let bytes = fs::read(&path).expect("read");
                let digest = Sha256::digest(&bytes);
                entries.insert(relative, format!("file:{}", hex(&digest)));
            }
        }
    }
    entries
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The store plans directory for the sandbox.
fn plans_dir(sandbox: &Sandbox) -> PathBuf {
    sandbox.user_store().join(".state").join("plans")
}

#[test]
fn scan_reports_native_items_with_kind_scope_path_and_state() {
    let sandbox = Sandbox::new();
    let (alpha, claude_json) = seed_simple(&sandbox);

    let output = agentuna(&sandbox, &["migrate", "scan", "--json"]);
    let envelope = assert_exit(&output, 0);
    let items = envelope["data"]["items"]
        .as_array()
        .expect("items array")
        .clone();
    assert_eq!(items.len(), 2, "one skill and one MCP server: {items:?}");
    for item in &items {
        for field in ["kind", "scope", "path", "state", "name"] {
            assert!(
                item[field].is_string(),
                "every scan record carries {field}: {item}"
            );
        }
    }
    let by_path = |path: &Path| {
        items
            .iter()
            .find(|item| item["path"].as_str() == Some(&path.display().to_string()))
            .unwrap_or_else(|| panic!("{} missing from the scan", path.display()))
            .clone()
    };
    let alpha_record = by_path(&alpha);
    assert_eq!(alpha_record["kind"], "skill");
    assert_eq!(alpha_record["state"], "not_in_agentuna");
    let docs_record = by_path(&claude_json);
    assert_eq!(docs_record["kind"], "mcp_server");
    assert_eq!(docs_record["name"], "docs");
    assert!(!stdout(&output).is_empty());
}

#[test]
fn apply_dry_run_lists_operations_and_creates_no_file_lock_or_journal() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);

    // Persist a plan first; it is the reviewed input the dry run reads.
    let output = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();
    assert_eq!(envelope["data"]["persisted"], Value::Bool(true));
    assert!(plans_dir(&sandbox).join(format!("{plan_id}.json")).exists());

    let before = snapshot(&sandbox.path("home"));
    let before_store = snapshot(&sandbox.user_store());
    let before_support = snapshot(&sandbox.support());
    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--plan",
            &plan_id,
            "--dry-run",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    assert_eq!(envelope["data"]["dry_run"], Value::Bool(true));
    let operations = envelope["data"]["operations"]
        .as_array()
        .expect("operations array")
        .clone();
    assert!(
        !operations.is_empty(),
        "the dry run lists the exact file operations"
    );
    for operation in &operations {
        assert!(operation["seq"].is_i64(), "rows carry seq: {operation}");
        assert!(
            operation["action"].is_string(),
            "rows carry action: {operation}"
        );
    }
    let after = snapshot(&sandbox.path("home"));
    let after_store = snapshot(&sandbox.user_store());
    let after_support = snapshot(&sandbox.support());
    assert_eq!(before, after, "a dry run must not touch the home");
    assert_eq!(
        before_store, after_store,
        "a dry run must not touch the store"
    );
    assert_eq!(
        before_support, after_support,
        "a dry run must create no file, lock or journal"
    );
}

#[test]
fn a_freshly_migrated_file_is_not_reported_as_drifted() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let output = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();
    let output = agentuna(
        &sandbox,
        &["migrate", "apply", "--plan", &plan_id, "--yes", "--json"],
    );
    assert_exit(&output, 0);

    // The deployment baselines must describe the rendered files, not the scan sources.
    let output = agentuna(&sandbox, &["drift", "list", "--json"]);
    let envelope = assert_exit(&output, 0);
    assert_eq!(
        envelope["data"]["events"],
        Value::Array(Vec::new()),
        "a fresh migration must not look drifted: {envelope}"
    );
}

#[test]
fn apply_succeeds_when_the_selected_project_has_no_store_directory() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    let project_arg = project.to_string_lossy().into_owned();
    let added = agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]);
    assert_exit(&added, 0);
    assert!(
        !project.join(".agentuna").exists(),
        "precondition: the project has no store yet"
    );

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--project",
            &project_arg,
            "--scope",
            "user",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--project",
            &project_arg,
            "--scope",
            "user",
            "--plan",
            &plan_id,
            "--yes",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    assert!(
        envelope["data"]["completed"].as_u64().unwrap_or(0) > 0,
        "the apply wrote its files: {envelope}"
    );
}

#[test]
fn project_scope_apply_initializes_a_missing_project_store() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    write(&project.join("AGENTS.md"), "# Project Guidelines\n");
    let project_arg = project.to_string_lossy().into_owned();

    let added = agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]);
    assert_exit(&added, 0);
    assert!(
        !project.join(".agentuna").exists(),
        "precondition: the project has no store yet"
    );

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--project",
            &project_arg,
            "--scope",
            "project",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--project",
            &project_arg,
            "--scope",
            "project",
            "--plan",
            &plan_id,
            "--yes",
            "--json",
        ],
    );
    assert_exit(&output, 0);
    assert!(
        project.join(".agentuna/store.json").exists(),
        "apply initialized the missing project store"
    );

    let database = at_db::Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let stored: Option<String> = database
        .connection()
        .query_row(
            "SELECT project_id FROM items WHERE scope = 'project' AND kind = 'rule'",
            [],
            |row| row.get(0),
        )
        .expect("the project-scope rule row exists");
    assert!(
        stored.is_some(),
        "the project-scope item carries the selected project id"
    );
}

#[test]
fn undo_resolves_a_project_scope_operation_without_the_project_flag() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    let rule = project.join("CLAUDE.md");
    write(&rule, "# Project Guidelines\n");
    let original_rule = fs::read(&rule).expect("read the original rule");
    let project_arg = project.to_string_lossy().into_owned();

    let added = agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]);
    assert_exit(&added, 0);

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--project",
            &project_arg,
            "--scope",
            "project",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--project",
            &project_arg,
            "--scope",
            "project",
            "--plan",
            &plan_id,
            "--yes",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let operation_id = envelope["data"]["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();
    assert!(
        project.join(".agentuna/store.json").exists(),
        "the apply wrote the project store"
    );

    // No `--project`: a Project-scope operation names its own project in the stored record.
    let output = agentuna(
        &sandbox,
        &["migrate", "undo", &operation_id, "--yes", "--json"],
    );
    let envelope = assert_exit(&output, 0);
    assert!(
        envelope["data"]["undo_operation_id"].is_string(),
        "the undo ran without --project: {envelope}"
    );
    assert_eq!(
        fs::read(&rule).expect("read the restored rule"),
        original_rule,
        "the project rule is restored byte-identically"
    );
}

#[test]
fn ambiguous_conflict_without_keep_flag_exits_needs_resolution_and_writes_nothing() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    seed_conflict(&sandbox);

    let output = agentuna(&sandbox, &["migrate", "plan", "--dry-run", "--json"]);
    assert_eq!(exit_code(&output), 6, "needs-resolution maps to exit 6");
    let envelope = json_envelope(&output);
    assert_eq!(envelope["ok"], Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "needs_resolution");
    let details = &envelope["error"]["details"];
    assert_eq!(details["conflict"], "dup");
    assert_eq!(details["kind"], "skill");
    let copies = details["copies"].as_array().expect("copies array");
    assert_eq!(copies.len(), 2, "both copies are listed: {details}");
    for (agent, directory) in [("claude_code", ".claude"), ("codex", ".codex")] {
        assert!(
            copies.iter().any(|copy| copy["agent"] == agent
                && copy["origin"] == "native"
                && copy["path"]
                    .as_str()
                    .is_some_and(|path| path.contains(&format!("{directory}/skills/dup")))),
            "the {agent} copy carries its agent, origin and path: {details}"
        );
    }
    for flag in ["keep_a", "keep_b", "keep_both"] {
        let command = details[flag].as_str().expect("ready-to-run command");
        assert!(
            command.contains(&format!("--{}", flag.replace('_', "-")))
                && command.contains("dup")
                && command.contains("--strategy links"),
            "`{flag}` is a ready-to-run command: {command}"
        );
    }
    let hint = envelope["error"]["hint"]
        .as_str()
        .expect("the failure explains the next step");
    assert!(
        hint.contains("--keep-both dup"),
        "the hint shows the exact resolution flag: {hint}"
    );
    assert!(
        hint.contains(".claude/skills/dup") && hint.contains(".codex/skills/dup"),
        "the hint names both conflicting paths: {hint}"
    );
    assert!(
        !plans_dir(&sandbox).exists(),
        "a refused plan persists nothing"
    );

    // With an explicit choice the same scan plans cleanly.
    let output = agentuna(
        &sandbox,
        &["migrate", "plan", "--keep-a", "dup", "--dry-run", "--json"],
    );
    let envelope = assert_exit(&output, 0);
    assert_eq!(envelope["ok"], Value::Bool(true));
}

#[test]
fn plan_accepts_repeated_conflict_resolution_flags() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    seed_conflict(&sandbox);
    let home = sandbox.path("home");
    write(&home.join(".codex/AGENTS.md"), "# Codex rules\n");
    write(&home.join(".omp/agent/AGENTS.md"), "# omp rules\n");

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--keep-both",
            "dup",
            "--keep-both",
            "agents",
            "--dry-run",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    assert_eq!(envelope["data"]["persisted"], Value::Bool(false));
    assert!(
        !envelope["data"]["steps"]
            .as_array()
            .expect("plan steps")
            .is_empty()
    );
}

#[test]
fn apply_needs_yes_then_apply_and_undo_round_trip() {
    let sandbox = Sandbox::new();
    let (alpha, claude_json) = seed_simple(&sandbox);
    let original_alpha = fs::read(alpha.join("SKILL.md")).expect("read alpha");
    let original_claude_json = fs::read(&claude_json).expect("read .claude.json");

    let output = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();

    // Without --yes the apply is refused and writes nothing.
    let output = agentuna(
        &sandbox,
        &["migrate", "apply", "--plan", &plan_id, "--json"],
    );
    assert_eq!(exit_code(&output), 9, "confirmation required is exit 9");
    assert_eq!(
        json_envelope(&output)["error"]["code"],
        "confirmation_required"
    );

    let output = agentuna(
        &sandbox,
        &["migrate", "apply", "--plan", &plan_id, "--yes", "--json"],
    );
    let envelope = assert_exit(&output, 0);
    let operation_id = envelope["data"]["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();
    let store_alpha = sandbox.user_store().join("skills/alpha");
    assert!(store_alpha.join("SKILL.md").exists(), "store item exists");
    let metadata = fs::symlink_metadata(&alpha).expect("symlink_metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "replace-with-links turned the original into a link"
    );

    let output = agentuna(&sandbox, &["migrate", "undo", &operation_id, "--json"]);
    assert_eq!(exit_code(&output), 9, "undo also needs --yes or --force");

    let output = agentuna(
        &sandbox,
        &["migrate", "undo", &operation_id, "--yes", "--json"],
    );
    let envelope = assert_exit(&output, 0);
    assert!(envelope["data"]["undo_operation_id"].is_string());
    assert_eq!(
        fs::read(alpha.join("SKILL.md")).expect("read restored alpha"),
        original_alpha,
        "the original skill is restored byte-identically"
    );
    assert_eq!(
        fs::read(&claude_json).expect("read restored .claude.json"),
        original_claude_json,
        "the agent MCP config is restored byte-identically"
    );
    let metadata = fs::symlink_metadata(&alpha).expect("symlink_metadata");
    assert!(
        !metadata.file_type().is_symlink(),
        "the link is gone after undo"
    );
    assert!(
        !store_alpha.join("SKILL.md").exists(),
        "the created store item is gone after undo"
    );
}

#[test]
fn undo_after_the_window_is_refused_with_undo_expired() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);

    let output = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    let plan_id = json_envelope(&output)["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();
    let output = agentuna(
        &sandbox,
        &["migrate", "apply", "--plan", &plan_id, "--yes", "--json"],
    );
    let operation_id = json_envelope(&output)["data"]["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    // Age the operation past the 30-day window.
    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    {
        let database =
            at_db::Database::open_file(sandbox.paths().db_path()).expect("open the sandbox index");
        database
            .connection()
            .execute(
                "UPDATE operations SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', '-31 days') WHERE id = ?2",
                (now_seconds, operation_id.as_str()),
            )
            .expect("age the operation");
    }

    let before = snapshot(&sandbox.path("home"));
    let _before_store = snapshot(&sandbox.user_store());
    let _before_support = snapshot(&sandbox.support());
    let output = agentuna(
        &sandbox,
        &["migrate", "undo", &operation_id, "--yes", "--json"],
    );
    assert_eq!(exit_code(&output), 6, "undo_expired is a user error exit");
    let envelope = json_envelope(&output);
    assert_eq!(envelope["error"]["code"], "undo_expired");
    let after = snapshot(&sandbox.path("home"));
    let after_store = snapshot(&sandbox.user_store());
    assert_eq!(before, after, "an expired undo changes nothing");
    assert_eq!(snapshot(&sandbox.user_store()), after_store);
}

#[test]
fn adopt_previews_then_replaces_the_original_with_a_link() {
    let sandbox = Sandbox::new();
    let (alpha, _claude_json) = seed_simple(&sandbox);
    let original_alpha = fs::read(alpha.join("SKILL.md")).expect("read alpha");

    let path_arg = alpha.display().to_string();
    let output = agentuna(&sandbox, &["adopt", &path_arg, "--dry-run", "--json"]);
    let envelope = assert_exit(&output, 0);
    assert_eq!(envelope["data"]["slug"], "alpha");
    assert!(
        !sandbox.user_store().join("skills/alpha").exists(),
        "a preview writes nothing"
    );

    let output = agentuna(&sandbox, &["adopt", &path_arg, "--json"]);
    assert_eq!(exit_code(&output), 9, "adopt needs --yes");

    let output = agentuna(&sandbox, &["adopt", &path_arg, "--yes", "--json"]);
    let envelope = assert_exit(&output, 0);
    let operation_id = envelope["data"]["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();
    let metadata = fs::symlink_metadata(&alpha).expect("symlink_metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "adopt replaced with a link"
    );
    assert!(
        sandbox.user_store().join("skills/alpha/SKILL.md").exists(),
        "the store holds the adopted content"
    );

    // Adopting the same path again is refused: it is already managed.
    let output = agentuna(&sandbox, &["adopt", &path_arg, "--yes", "--json"]);
    assert_eq!(exit_code(&output), 6, "already-managed is a conflict");
    assert_eq!(json_envelope(&output)["error"]["code"], "conflict");

    // The adoption is undoable like every migration operation.
    let output = agentuna(
        &sandbox,
        &["migrate", "undo", &operation_id, "--yes", "--json"],
    );
    assert_exit(&output, 0);
    assert_eq!(
        fs::read(alpha.join("SKILL.md")).expect("read restored alpha"),
        original_alpha,
        "undo of adopt restores the exact original bytes"
    );
    let metadata = fs::symlink_metadata(&alpha).expect("symlink_metadata");
    assert!(!metadata.file_type().is_symlink(), "the link is removed");
}

#[test]
fn adopt_of_a_project_scope_rule_records_the_selected_project_id() {
    let sandbox = Sandbox::new();
    init_store(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    let rule = project.join("AGENTS.md");
    write(&rule, "# Project Guidelines\n");
    let project_arg = project.to_string_lossy().into_owned();

    let registered = agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]);
    let envelope = assert_exit(&registered, 0);
    let project_id = envelope["data"]["project"]["id"]
        .as_str()
        .expect("registered project id")
        .to_owned();

    let path_arg = rule.display().to_string();
    let output = agentuna(
        &sandbox,
        &[
            "adopt",
            &path_arg,
            "--project",
            &project_arg,
            "--yes",
            "--json",
        ],
    );
    assert_exit(&output, 0);

    let database = at_db::Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let stored: Option<String> = database
        .connection()
        .query_row(
            "SELECT project_id FROM items WHERE scope = 'project' AND kind = 'rule'",
            [],
            |row| row.get(0),
        )
        .expect("the adopted project-scope item row exists");
    assert_eq!(
        stored.as_deref(),
        Some(project_id.as_str()),
        "a project-scope adoption records the selected project"
    );
}

#[test]
fn adopt_of_a_project_scope_rule_without_a_registered_project_has_a_typed_error() {
    let sandbox = Sandbox::new();
    init_store(&sandbox);
    let project = sandbox.path("unregistered");
    fs::create_dir_all(&project).expect("project directory");
    let rule = project.join("AGENTS.md");
    write(&rule, "# Unregistered Project\n");
    let project_arg = project.to_string_lossy().into_owned();

    let path_arg = rule.display().to_string();
    let output = agentuna(
        &sandbox,
        &[
            "adopt",
            &path_arg,
            "--project",
            &project_arg,
            "--yes",
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&output),
        4,
        "an unregistered project is a not-found user error: {}",
        stderr(&output)
    );
    assert_eq!(json_envelope(&output)["error"]["code"], "not_found");
}

#[test]
fn status_lists_counts_pending_plans_and_recent_operations() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);

    let output = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    assert_exit(&output, 0);

    let output = agentuna(&sandbox, &["migrate", "status", "--json"]);
    let envelope = assert_exit(&output, 0);
    assert_eq!(
        envelope["data"]["items"]["total"].as_i64(),
        Some(2),
        "two native items"
    );
    assert!(
        envelope["data"]["pending_plans"]
            .as_array()
            .is_some_and(|plans| !plans.is_empty()),
        "the persisted plan is pending"
    );

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--plan",
            json_envelope(&output)["data"]["pending_plans"][0]["plan_id"]
                .as_str()
                .expect("plan id"),
            "--yes",
            "--json",
        ],
    );
    assert_exit(&output, 0);

    let output = agentuna(&sandbox, &["migrate", "status", "--json"]);
    let envelope = assert_exit(&output, 0);
    let recent = envelope["data"]["recent_operations"]
        .as_array()
        .expect("recent operations");
    assert!(
        recent
            .iter()
            .any(|row| row["kind"].as_str() == Some("migrate")),
        "the applied migration appears in recent operations: {recent:?}"
    );
}

#[test]
fn migrate_human_output_renders_counts() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);

    let output = agentuna(&sandbox, &["migrate", "scan"]);
    assert_eq!(exit_code(&output), 0);
    let human = stdout(&output);
    assert!(human.contains("2 native item(s)"), "counts: {human}");
    assert!(human.contains("alpha"), "item rows: {human}");
}

#[test]
fn a_mixed_selection_migrates_into_each_scope_store_and_one_undo_restores_the_tree() {
    let sandbox = Sandbox::new();
    let (user_skill, _) = seed_simple(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    let project_skill = project.join(".claude/skills/alpha");
    write(
        &project_skill.join("SKILL.md"),
        &skill_doc("alpha", "Project copy of the plain native skill"),
    );
    write(&project.join("CLAUDE.md"), "# Project Guidelines\n");
    let project_arg = project.to_string_lossy().into_owned();
    assert_exit(
        &agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]),
        0,
    );

    let before_home = snapshot(&sandbox.path("home"));
    let before_project = snapshot(&project);

    // The two `alpha` copies live in different scopes, so they never contend for one store name.
    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--project",
            &project_arg,
            "--keep-a",
            "alpha",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let plan_id = envelope["data"]["plan_id"]
        .as_str()
        .expect("plan_id")
        .to_owned();
    let scopes = envelope["data"]["scopes"]
        .as_array()
        .expect("scopes")
        .clone();
    let user_plan = scopes
        .iter()
        .find(|entry| entry["scope"] == "user")
        .expect("the plan names the User store");
    let project_plan = scopes
        .iter()
        .find(|entry| entry["scope"] == "project")
        .expect("the plan names the Project store");
    let expected_user_store = sandbox.user_store().display().to_string();
    let expected_project_store = project.join(".agentuna").display().to_string();
    assert_eq!(
        user_plan["store_path"].as_str(),
        Some(expected_user_store.as_str())
    );
    assert_eq!(
        project_plan["store_path"].as_str(),
        Some(expected_project_store.as_str())
    );
    assert_eq!(user_plan["counts"]["items"].as_i64(), Some(2));
    assert_eq!(project_plan["counts"]["items"].as_i64(), Some(2));

    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "apply",
            "--project",
            &project_arg,
            "--plan",
            &plan_id,
            "--yes",
            "--json",
        ],
    );
    let envelope = assert_exit(&output, 0);
    let operation_id = envelope["data"]["operation_id"]
        .as_str()
        .expect("operation_id")
        .to_owned();

    let user_store = sandbox.user_store();
    let project_store = project.join(".agentuna");
    assert!(
        user_store.join("skills/alpha/SKILL.md").exists(),
        "the user skill landed in the User store"
    );
    assert!(
        user_store.join("mcp.json").exists(),
        "the MCP server landed in the User store"
    );
    assert!(
        project_store.join("skills/alpha/SKILL.md").exists(),
        "the project skill landed in the Project store"
    );
    assert!(
        project_store.join("rules/claude.md").exists(),
        "the project rule landed in the Project store"
    );
    assert_eq!(
        fs::canonicalize(&user_skill).expect("user link"),
        fs::canonicalize(user_store.join("skills/alpha")).expect("user item"),
        "the user native skill links into the User store"
    );
    assert_eq!(
        fs::canonicalize(&project_skill).expect("project link"),
        fs::canonicalize(project_store.join("skills/alpha")).expect("project item"),
        "the project native skill links into the Project store"
    );

    // One Undo reverses both stores and every native original; the stores keep their own
    // bookkeeping, so only the native tree is compared.
    let output = agentuna(
        &sandbox,
        &["migrate", "undo", &operation_id, "--yes", "--json"],
    );
    assert_exit(&output, 0);
    assert_eq!(snapshot(&sandbox.path("home")), before_home);
    let restored_project = snapshot(&project)
        .into_iter()
        .filter(|(key, _)| !key.starts_with(".agentuna"))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(restored_project, before_project);
}

#[test]
fn a_project_scope_item_without_a_registered_project_refuses_the_plan() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let project = sandbox.path("project");
    write(
        &project.join(".claude/skills/alpha/SKILL.md"),
        &skill_doc("alpha", "Project copy of the plain native skill"),
    );
    let project_arg = project.to_string_lossy().into_owned();

    // Deliberately not registered: the item has no project identity to record, so the plan
    // refuses instead of writing it into the User store.
    let output = agentuna(
        &sandbox,
        &[
            "migrate",
            "plan",
            "--project",
            &project_arg,
            "--keep-a",
            "alpha",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 3, "a missing project maps to exit 3");
    let envelope = json_envelope(&output);
    assert_eq!(envelope["error"]["code"], "validation");
    let message = envelope["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("project-scope"),
        "the refusal names the scope: {message}"
    );
    assert!(
        !project.join(".agentuna").exists(),
        "a refused plan writes nothing"
    );
    assert!(
        !plans_dir(&sandbox).exists(),
        "a refused plan persists nothing"
    );
}

#[test]
fn apply_without_a_plan_lists_the_persisted_ids_and_the_next_command() {
    let sandbox = Sandbox::new();
    seed_simple(&sandbox);
    let planned = agentuna(&sandbox, &["migrate", "plan", "--json"]);
    let planned = assert_exit(&planned, 0);
    let plan_id = planned["data"]["plan_id"]
        .as_str()
        .expect("migrate plan prints its id")
        .to_owned();

    // TC-030 names the plan on the command line, so a planless apply stays a usage error;
    // the refusal now lists what the caller could pass instead of stopping at "missing".
    let output = agentuna(&sandbox, &["migrate", "apply", "--json"]);
    let envelope = assert_exit(&output, 2);
    assert_eq!(envelope["error"]["code"], "usage");
    let details = &envelope["error"]["details"];
    assert_eq!(details["latest"], plan_id);
    let listed = details["plans"].as_array().expect("plans array");
    assert!(
        listed
            .iter()
            .any(|plan| plan["plan_id"] == plan_id.as_str()),
        "the persisted plan is listed: {details}"
    );
    let next = details["next_command"]
        .as_str()
        .expect("the refusal carries the next command");
    assert!(next.contains("agentuna migrate apply --plan"));
    assert!(next.contains(&plan_id));
    assert!(
        next.ends_with("--dry-run"),
        "the next command reviews first: {next}"
    );
    let hint = envelope["error"]["hint"].as_str().expect("hint");
    assert!(
        hint.contains(&plan_id),
        "the hint names the available plan: {hint}"
    );
    assert!(
        hint.contains("agentuna migrate status"),
        "the hint names the listing command: {hint}"
    );
}

#[test]
fn apply_without_a_plan_and_none_persisted_explains_how_to_build_one() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let output = agentuna(&sandbox, &["migrate", "apply", "--json"]);
    let envelope = assert_exit(&output, 2);
    assert_eq!(envelope["error"]["code"], "usage");
    assert!(envelope["error"]["details"]["plans"].is_null());
    let hint = envelope["error"]["hint"].as_str().expect("hint");
    assert!(
        hint.contains("agentuna migrate plan"),
        "the hint says how to build a plan: {hint}"
    );
    assert!(
        hint.contains("--dry-run"),
        "the hint previews before applying: {hint}"
    );
}
