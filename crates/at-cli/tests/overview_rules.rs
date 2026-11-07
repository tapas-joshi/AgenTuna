//! TA-071: `overview` and `rules` command groups against a sandbox.
//!
//! Every test runs the real `agentuna` binary with HOME, the user store, the Global store and
//! the project root redirected into a temporary directory, so no test reads or writes the
//! developer's real configuration.

mod support;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use support::{Sandbox, agentuna, agentuna_with_stdin, exit_code, stderr, stdout};

/// Creates the project folder the CLI is told about with `--project`.
fn project(sandbox: &Sandbox, name: &str) -> PathBuf {
    let root = sandbox.path(name);
    fs::create_dir_all(root.join(".agentuna")).expect("project store");
    root
}

fn json_stdout(output: &std::process::Output) -> Value {
    let text = stdout(output);
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON object ({error}): {text:?} stderr: {:?}",
            stderr(output)
        )
    })
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read fixture file")
}

#[test]
fn overview_json_returns_the_screen_projection_inside_the_envelope() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let output = agentuna(&sandbox, &["overview", "--scope", "user", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_stdout(&output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "overview");
    let data = &envelope["data"];
    for field in [
        "scopes",
        "attention",
        "attention_total",
        "changes",
        "global_writable",
        "user_path",
        "global_path",
    ] {
        assert!(
            !data[field].is_null(),
            "overview payload is missing {field}"
        );
    }
    // The same engine projection the Overview screen renders.
    let paths = sandbox.paths();
    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let expected = at_engine::overview::build(database.connection(), at_model::Scope::User)
        .expect("engine overview");
    assert_eq!(
        data["scopes"].as_array().expect("scope cards").len(),
        expected.scopes.len()
    );
    assert_eq!(
        data["attention_total"].as_u64().expect("attention total"),
        expected.needs_attention_total as u64
    );
    assert_eq!(
        data["changes"].as_array().expect("changes").len(),
        expected.recent_changes.len()
    );
}

#[test]
fn rules_list_and_show_report_scope_path_managed_size_hash_and_raw_bytes() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let root = project(&sandbox, "rules-project");
    let agents = root.join("AGENTS.md");
    let content = b"# Project rules\n\nKeep diffs small.\n";
    fs::write(&agents, content).expect("write AGENTS.md");

    let output = agentuna(
        &sandbox,
        &[
            "rules",
            "list",
            "--scope",
            "project",
            "--project",
            root.to_str().expect("utf-8 project"),
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_stdout(&output);
    let rows = envelope["data"]["rules"].as_array().expect("rules array");
    let row = rows
        .iter()
        .find(|row| row["path"] == agents.display().to_string())
        .expect("AGENTS.md is listed");
    assert_eq!(row["scope"], "project");
    assert_eq!(row["managed"], false);
    assert_eq!(row["size"], content.len() as u64);
    assert_eq!(
        row["hash"],
        at_fs::sha256_hex(content).as_str(),
        "the hash is the file's sha256"
    );

    let output = agentuna(
        &sandbox,
        &["rules", "show", agents.to_str().expect("utf-8 path")],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output).as_bytes(),
        content,
        "rules show writes the raw bytes with no decoration"
    );
}

#[test]
fn rules_save_with_an_unchanged_buffer_exits_zero_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let root = project(&sandbox, "rules-save-project");
    let agents = root.join("AGENTS.md");
    fs::write(&agents, b"# Project rules\n").expect("write AGENTS.md");
    let before = bytes(&agents);

    let output = agentuna_with_stdin(
        &sandbox,
        &[
            "rules",
            "save",
            agents.to_str().expect("utf-8 path"),
            "--stdin",
            "--scope",
            "project",
            "--project",
            root.to_str().expect("utf-8 project"),
        ],
        &before,
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("unchanged"), "{}", stdout(&output));
    assert_eq!(bytes(&agents), before, "an unchanged buffer writes nothing");
}

#[test]
fn rules_save_refuses_an_external_edit_and_leaves_the_file_untouched() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let root = project(&sandbox, "rules-conflict-project");
    let agents = root.join("AGENTS.md");
    fs::write(&agents, b"# Original\n").expect("write AGENTS.md");

    // The scan happens before stdin is read, so an edit in that window is exactly the
    // concurrent-external-write case the review check must catch. Startup time varies on a
    // loaded machine, so retry with a fresh file rather than guessing one sleep.
    let mut last = None;
    for attempt in 0..4_u64 {
        fs::write(&agents, b"# Original\n").expect("reset AGENTS.md");
        let mut child = Command::new(env!("CARGO_BIN_EXE_agentuna"))
            .args([
                "rules",
                "save",
                agents.to_str().expect("utf-8 path"),
                "--stdin",
                "--scope",
                "project",
                "--project",
                root.to_str().expect("utf-8 project"),
            ])
            .env("NO_COLOR", "1")
            .envs(sandbox.vars())
            .current_dir(sandbox.cwd())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agentuna");
        std::thread::sleep(Duration::from_secs(2 + attempt));
        fs::write(&agents, b"# External edit\n").expect("external edit");
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(b"# Editor buffer\n").expect("write stdin");
        }
        let output = child.wait_with_output().expect("collect output");
        let code = exit_code(&output);
        last = Some(output);
        if code == 6 {
            break;
        }
    }
    let output = last.expect("one attempt ran");
    assert_eq!(
        exit_code(&output),
        6,
        "a stale buffer is a conflict; stdout: {} stderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("changed since it was opened"),
        "the conflict names the external edit: {}",
        stderr(&output)
    );
    assert_eq!(
        bytes(&agents),
        b"# External edit\n",
        "the newer external edit is preserved"
    );
}

#[test]
fn rules_save_force_writes_through_the_backup_pipeline() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let root = project(&sandbox, "rules-force-project");
    let agents = root.join("AGENTS.md");
    fs::write(&agents, b"# Original\n").expect("write AGENTS.md");

    let output = agentuna_with_stdin(
        &sandbox,
        &[
            "rules",
            "save",
            agents.to_str().expect("utf-8 path"),
            "--stdin",
            "--force",
            "--scope",
            "project",
            "--project",
            root.to_str().expect("utf-8 project"),
        ],
        b"# Forced buffer\n",
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(bytes(&agents), b"# Forced buffer\n");
    let printed = stdout(&output);
    assert!(
        printed.contains("Backup:"),
        "output names the backup: {printed}"
    );
    let paths = sandbox.paths();
    let backup_root = paths.backup_dir();
    let mut backups = Vec::new();
    collect_files(backup_root, &mut backups);
    assert!(
        backups.iter().any(|path| bytes(path) == b"# Original\n"),
        "the pre-write bytes are in the backup pipeline under {}",
        backup_root.display()
    );
}

#[test]
fn rules_new_refuses_to_overwrite_an_existing_agents_file() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let root = project(&sandbox, "rules-new-project");
    let agents = root.join("AGENTS.md");
    fs::write(&agents, b"# Existing\n").expect("write AGENTS.md");

    let output = agentuna(
        &sandbox,
        &[
            "rules",
            "new",
            "--type",
            "AGENTS.md",
            "--scope",
            "project",
            "--project",
            root.to_str().expect("utf-8 project"),
        ],
    );
    assert_eq!(exit_code(&output), 6, "stderr: {}", stderr(&output));
    let diagnostic = stderr(&output);
    assert!(
        diagnostic.contains("already exists"),
        "diagnostic names the existing file: {diagnostic}"
    );
    assert_eq!(bytes(&agents), b"# Existing\n");

    // A different project creates the file with the create-only template.
    let fresh = project(&sandbox, "rules-new-fresh");
    let output = agentuna(
        &sandbox,
        &[
            "rules",
            "new",
            "--type",
            "AGENTS.md",
            "--scope",
            "project",
            "--project",
            fresh.to_str().expect("utf-8 project"),
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(
        bytes(&fresh.join("AGENTS.md")),
        b"<!-- agentuna:begin -->\n# AGENTS.md\n<!-- agentuna:end -->\n"
    );
}

#[test]
fn overview_and_rules_appear_once_in_the_help_tree() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["--help"]);
    assert_eq!(exit_code(&output), 0);
    let help = stdout(&output);
    for noun in ["overview", "rules"] {
        let occurrences = help
            .lines()
            .filter(|line| line.trim_start().starts_with(noun))
            .count();
        assert_eq!(
            occurrences, 1,
            "`{noun}` appears once in the help tree: {help}"
        );
    }
}

fn collect_files(root: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, into);
        } else {
            into.push(path);
        }
    }
}
