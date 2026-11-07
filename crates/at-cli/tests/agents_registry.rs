//! TA-070/TM-027 regression: the CLI must index agent detection before reporting on it.
//!
//! Every test runs the real `agentuna` binary with `HOME`, the stores and the support
//! directory redirected into a temporary sandbox, so the developer's agent folders are
//! never read or written.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::Value;
use support::{Sandbox, agentuna, exit_code, stderr, stdout};

/// Writes `contents`, creating parents.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// A skill document the scanner accepts.
fn skill_doc(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n")
}

/// Seeds native Claude Code, Codex and omp skills, rules and MCP servers.
fn seed_native(sandbox: &Sandbox) {
    let home = sandbox.path("home");
    write(
        &home.join(".claude/skills/duplicate/SKILL.md"),
        &skill_doc("duplicate", "Claude copy of the conflicting skill"),
    );
    write(
        &home.join(".codex/skills/duplicate/SKILL.md"),
        &skill_doc("duplicate", "Codex copy of the conflicting skill"),
    );
    write(
        &home.join(".agents/skills/shared-skill/SKILL.md"),
        &skill_doc("shared-skill", "Shared by the Codex and omp user roots"),
    );
    write(&home.join(".claude/CLAUDE.md"), "# Claude rules\n");
    write(&home.join(".codex/AGENTS.md"), "# Codex rules\n");
    write(&home.join(".omp/agent/AGENTS.md"), "# omp rules\n");
    write(
        &home.join(".claude.json"),
        "{\n  \"mcpServers\": {\n    \"claude_alpha\": {\"command\": \"node\"},\n    \"claude_beta\": {\"command\": \"python\"}\n  }\n}\n",
    );
    write(
        &home.join(".codex/config.toml"),
        "[mcp_servers.codex_alpha]\ncommand = \"node\"\n\n[mcp_servers.codex_beta]\ncommand = \"python\"\n",
    );
    write(
        &home.join(".omp/agent/mcp.json"),
        "{\n  \"mcpServers\": {\n    \"omp_alpha\": {\"command\": \"node\"},\n    \"omp_beta\": {\"command\": \"python\"}\n  }\n}\n",
    );
}

/// Initializes an empty canonical User store so migration has a manifest to update.
fn init_store(sandbox: &Sandbox) {
    fs::create_dir_all(sandbox.support()).expect("create support");
    at_store::layout::init_store(at_model::Scope::User, sandbox.user_store())
        .expect("initialize the sandbox user store");
}

/// Parses stdout as exactly one JSON envelope.
fn envelope(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).unwrap_or_else(|error| {
        panic!(
            "one JSON envelope expected ({error}); stderr: {}\nstdout: {}",
            stderr(output),
            stdout(output)
        )
    })
}

/// Runs a command that must succeed and returns its `data` payload.
fn ok(sandbox: &Sandbox, args: &[&str]) -> Value {
    let output = agentuna(sandbox, args);
    let envelope = envelope(&output);
    assert_eq!(
        exit_code(&output),
        0,
        "`{args:?}` must succeed; stderr: {}\nstdout: {}",
        stderr(&output),
        stdout(&output)
    );
    assert_eq!(envelope["ok"], Value::Bool(true), "{envelope}");
    envelope["data"].clone()
}

/// The row with `id` in an agents list payload.
fn agent<'a>(agents: &'a Value, id: &str) -> &'a Value {
    agents
        .as_array()
        .expect("agents array")
        .iter()
        .find(|agent| agent["id"] == id)
        .unwrap_or_else(|| panic!("agent `{id}` is listed: {agents}"))
}

#[test]
fn apply_records_detection_and_reads_report_agents_servers_and_loads() {
    let sandbox = Sandbox::new();
    seed_native(&sandbox);
    init_store(&sandbox);

    let _scan = ok(&sandbox, &["--scope", "user", "migrate", "scan", "--json"]);
    let plan = ok(
        &sandbox,
        &[
            "--scope",
            "user",
            "migrate",
            "plan",
            "--strategy",
            "links",
            "--keep-both",
            "duplicate",
            "--keep-both",
            "agents",
            "--json",
        ],
    );
    let plan_id = plan["plan_id"].as_str().expect("plan id").to_owned();
    let applied = ok(
        &sandbox,
        &[
            "--scope", "user", "migrate", "apply", "--plan", &plan_id, "--yes", "--json",
        ],
    );
    let operation_id = applied["operation_id"]
        .as_str()
        .expect("operation id")
        .to_owned();

    let agents = ok(&sandbox, &["--scope", "user", "agents", "list", "--json"]);
    for id in ["claude_code", "codex", "omp"] {
        let row = agent(&agents, id);
        assert_eq!(
            row["installed"],
            Value::Bool(true),
            "{id} is installed: {row}"
        );
        assert!(
            row["primary_path"]
                .as_str()
                .is_some_and(|path| !path.is_empty()),
            "{id} carries its native path: {row}"
        );
        assert!(
            row["health_line"]
                .as_str()
                .is_some_and(|line| line.contains(" \u{b7} ")),
            "{id} carries a health line: {row}"
        );
    }

    let mcp = ok(&sandbox, &["--scope", "user", "mcp", "list", "--json"]);
    assert_eq!(mcp["count"], 6, "every migrated MCP server is listed");
    let servers = mcp["servers"].as_array().expect("server rows");
    let claude_alpha = servers
        .iter()
        .find(|server| server["name"] == "claude_alpha")
        .expect("claude_alpha row");
    assert_eq!(
        claude_alpha["agent_state"]["claude_code"]["state"], "linked",
        "the migrated server is linked into Claude Code: {claude_alpha}"
    );
    for server in servers {
        let states = server["agent_state"].as_object().expect("agent cells");
        for id in ["claude_code", "codex", "omp"] {
            assert!(states.contains_key(id), "{id} has a cell: {server}");
        }
    }

    let skills = ok(&sandbox, &["--scope", "user", "skills", "list", "--json"]);
    assert_eq!(skills["count"], 3, "three migrated skills are indexed");
    let duplicate = skills["skills"]
        .as_array()
        .expect("skill rows")
        .iter()
        .find(|skill| skill["slug"] == "duplicate-2")
        .expect("duplicate-2 row");
    assert_eq!(
        duplicate["agents"]["codex"]["state"], "linked",
        "the skill deployment reaches Codex: {duplicate}"
    );

    let effective = ok(&sandbox, &["--scope", "user", "effective", "--json"]);
    let items = effective["items"].as_array().expect("effective items");
    let duplicate = items
        .iter()
        .find(|item| item["logical_id"]["key"]["Name"] == "duplicate")
        .expect("duplicate item");
    assert_eq!(
        duplicate["loads_in_agents"], 2,
        "Claude Code and Codex load the migrated duplicate: {duplicate}"
    );
    assert!(
        duplicate["agents"]
            .as_array()
            .is_some_and(|agents| agents.len() >= 3),
        "each installed agent has a state entry: {duplicate}"
    );

    let _undo = ok(
        &sandbox,
        &[
            "--scope",
            "user",
            "migrate",
            "undo",
            &operation_id,
            "--yes",
            "--json",
        ],
    );
    let mcp_after = ok(&sandbox, &["--scope", "user", "mcp", "list", "--json"]);
    let canonical = mcp_after["servers"]
        .as_array()
        .expect("server rows")
        .iter()
        .filter(|server| server["native"] == Value::Bool(false))
        .count();
    assert_eq!(
        canonical, 0,
        "undo removes every migrated server: {mcp_after}"
    );
    let effective_after = ok(&sandbox, &["--scope", "user", "effective", "--json"]);
    for item in effective_after["items"]
        .as_array()
        .expect("effective items")
    {
        assert_eq!(
            item["loads_in_agents"], 0,
            "undo removes the migration deployments: {item}"
        );
    }
}

#[test]
fn read_only_read_never_records_detection_or_creates_the_index() {
    let sandbox = Sandbox::new();
    seed_native(&sandbox);

    let agents = ok(
        &sandbox,
        &["--scope", "user", "agents", "list", "--read-only", "--json"],
    );
    assert_eq!(
        agents.as_array().map(Vec::len),
        Some(0),
        "--read-only leaves an empty registry untouched: {agents}"
    );
    assert!(
        !sandbox.paths().db_path().exists(),
        "--read-only must not create the index"
    );
}

#[test]
fn first_read_command_records_detection_without_apply() {
    let sandbox = Sandbox::new();
    seed_native(&sandbox);
    sandbox.healthy();

    let agents = ok(&sandbox, &["--scope", "user", "agents", "list", "--json"]);
    for id in ["claude_code", "codex", "omp"] {
        let row = agent(&agents, id);
        assert_eq!(
            row["installed"],
            Value::Bool(true),
            "the first read records detection for {id}: {row}"
        );
        assert!(
            row["primary_path"]
                .as_str()
                .is_some_and(|path| !path.is_empty()),
            "{id} carries its native path: {row}"
        );
    }
}
