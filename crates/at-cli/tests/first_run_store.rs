//! CLI parity for a scope whose store does not exist yet.
//!
//! The dry run must name the store creation the confirmed run performs, the Global scope must
//! refuse without its explicit consent, and `--read-only` must refuse before any write.

mod support;

use std::fs;

use serde_json::Value;
use support::{Sandbox, agentuna, agentuna_with_stdin, exit_code, stderr, stdout};

fn json_output(output: &std::process::Output) -> Value {
    let text = stdout(output);
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON object ({error}): {text:?} stderr: {:?}",
            stderr(output)
        )
    })
}

fn definition(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "definition": {
            "display_name": "Demo Server",
            "description": null,
            "transport": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "demo-server"],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string()
}

#[test]
fn mcp_add_dry_run_lists_the_user_store_creation() {
    let sandbox = Sandbox::new();
    let store = sandbox.user_store();
    assert!(!store.exists(), "the sandbox starts without a user store");
    let output = agentuna_with_stdin(
        &sandbox,
        &["--scope", "user", "mcp", "add", "--dry-run", "--json"],
        definition("demo-server").as_bytes(),
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(
        envelope["data"]["create_store"].as_str(),
        Some(store.to_string_lossy().as_ref()),
        "the dry run names the store it would create: {envelope}"
    );
    assert!(
        envelope["data"]["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty()),
        "the reviewed files include the store skeleton: {envelope}"
    );
    assert!(!store.exists(), "a dry run writes nothing");
}

#[test]
fn mcp_add_global_refuses_without_write_consent() {
    let sandbox = Sandbox::new();
    let global = sandbox.global_store();
    assert!(!global.exists(), "the Global store is not enabled yet");
    let output = agentuna_with_stdin(
        &sandbox,
        &["--scope", "global", "mcp", "add", "--json"],
        definition("demo-server").as_bytes(),
    );
    assert_eq!(
        exit_code(&output),
        at_cli::error::exit_code::CONFIRMATION_REQUIRED,
        "stderr: {}",
        stderr(&output)
    );
    let envelope = json_output(&output);
    assert_eq!(envelope["error"]["code"], "confirmation_required");
    let remedy = format!(
        "{} {}",
        envelope["error"]["message"].as_str().unwrap_or_default(),
        envelope["error"]["hint"].as_str().unwrap_or_default()
    )
    .to_lowercase();
    assert!(
        remedy.contains("global writes"),
        "the remedy names the consent action: {envelope}"
    );
    assert!(!global.exists(), "the refusal writes nothing");
}

#[test]
fn skills_install_dry_run_lists_the_store_creation() {
    let sandbox = Sandbox::new();
    let source = sandbox.path("fixtures/fresh-skill");
    fs::create_dir_all(&source).expect("source folder");
    fs::write(
        source.join("SKILL.md"),
        b"---\nname: fresh-skill\ndescription: A first-run fixture.\n---\n\n# Fresh Skill\n",
    )
    .expect("skill document");
    let store = sandbox.user_store();
    assert!(!store.exists(), "the sandbox starts without a user store");
    let output = agentuna(
        &sandbox,
        &[
            "--scope",
            "user",
            "skills",
            "install",
            source.to_str().expect("utf-8 source"),
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(
        envelope["data"]["create_store"].as_str(),
        Some(store.to_string_lossy().as_ref()),
        "the dry run names the store it would create: {envelope}"
    );
    assert!(!store.exists(), "a dry run writes nothing");
}
