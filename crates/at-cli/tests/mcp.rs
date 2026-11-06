//! TM-027: the MCP command surface is registered and secret values are not arguments.

mod support;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use at_cli::{Color, Stdio, production_registry, run};
use at_model::{AgentId, ItemKind, ItemState, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ContentHash, ManifestItem, StoreManifest, StorePath};
use at_store::mcp_store::McpStore;
use serde_json::Value;
use support::{Sandbox, agentuna, exit_code, stderr, stdout};

/// A sentinel that must never appear in any command output.
const SENTINEL: &str = "sk-sentinel-must-never-be-printed";

#[test]
fn production_tree_contains_all_mcp_and_secret_operations() {
    let registry = production_registry().expect("production command tree");
    let command = registry.command();
    let mcp = command
        .get_subcommands()
        .find(|command| command.get_name() == "mcp")
        .expect("mcp group registered");
    for name in [
        "list", "show", "add", "edit", "remove", "test", "enable", "disable", "secret",
    ] {
        assert!(
            mcp.get_subcommands()
                .any(|command| command.get_name() == name),
            "missing mcp {name} command"
        );
    }
    let secret = mcp
        .get_subcommands()
        .find(|command| command.get_name() == "secret")
        .expect("nested secret group");
    for name in ["list", "set", "rm"] {
        assert!(
            secret
                .get_subcommands()
                .any(|command| command.get_name() == name),
            "missing mcp secret {name} command"
        );
    }
}

#[test]
fn secret_set_rejects_and_redacts_a_value_passed_as_an_argument() {
    let registry = production_registry().expect("production command tree");
    let args = [
        OsString::from("agentuna"),
        OsString::from("mcp"),
        OsString::from("secret"),
        OsString::from("set"),
        OsString::from("OPENAI_API_KEY"),
        OsString::from("sk-sentinel-must-never-be-argv"),
        OsString::from("--json"),
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = {
        let mut stdio = Stdio {
            out: &mut out,
            err: &mut err,
            color: Color::Plain,
        };
        run(&registry, &args, &mut stdio)
    };
    assert_eq!(status, 2);
    let stdout = String::from_utf8(out).expect("JSON stdout");
    let stderr = String::from_utf8(err).expect("diagnostic stderr");
    assert!(!stdout.contains("sk-sentinel-must-never-be-argv"));
    assert!(!stderr.contains("sk-sentinel-must-never-be-argv"));
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("error envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "usage");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stdin")
    );
}

#[test]
fn mcp_add_accepts_dry_run_only_through_shared_global_flag() {
    let command = production_registry()
        .expect("production command tree")
        .command();
    let matches = command
        .try_get_matches_from(["agentuna", "mcp", "add", "--dry-run", "--json"])
        .expect("shared global flags propagate to MCP commands");
    let (noun, matches) = matches.subcommand().expect("noun");
    assert_eq!(noun, "mcp");
    let (subcommand, _) = matches.subcommand().expect("MCP operation");
    assert_eq!(subcommand, "add");
}

/// A stdio MCP server that answers `initialize` and `tools/list` over newline-delimited JSON.
fn fake_server_script(sandbox: &Sandbox) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = sandbox.path("fake-mcp-server.sh");
    let script = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      ver=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
      [ -n "$ver" ] || ver=2025-06-18
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"fixture-server","version":"1.0.0"}}}\n' "$id" "$ver"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
  esac
done
"#;
    fs::write(&path, script).expect("write fake server");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod fake server");
    path
}

/// The agent home the spawned binary resolves from `HOME` in the sandbox.
fn agent_home(sandbox: &Sandbox) -> PathBuf {
    let home = sandbox.path("home");
    fs::create_dir_all(&home).expect("agent home");
    home
}

/// Seeds the User store, the index rows the UI query reads, and one deployment.
fn seed_user_store(sandbox: &Sandbox, command: &Path) -> at_model::ItemId {
    let paths = sandbox.paths();
    let layout = StoreLayout::for_scope(&paths, Scope::User).expect("user layout");
    fs::create_dir_all(layout.root()).expect("user store");
    let mut store = McpStore::empty();
    store.set(
        "fixture-server",
        at_model::McpServerDef {
            display_name: String::from("Fixture Server"),
            description: None,
            transport: at_model::McpTransport::Stdio {
                command: command.display().to_string(),
                args: Vec::new(),
                cwd: None,
                env: std::collections::BTreeMap::from([(
                    String::from("FIXTURE_TOKEN"),
                    at_model::EnvValue::Literal(SENTINEL.to_owned()),
                )]),
            },
            extra: std::collections::BTreeMap::new(),
        },
    );
    fs::write(layout.mcp_file(), store.to_bytes().expect("MCP bytes")).expect("write mcp.json");

    let item_id = at_model::ItemId::new();
    let mut manifest = StoreManifest::new(Scope::User, at_store::layout::StoreId::new());
    manifest
        .insert_item(
            ManifestItem::new(
                item_id,
                ItemKind::McpServer,
                Slug::parse("fixture-server").expect("slug"),
                StorePath::parse("mcp.json").expect("path"),
                ContentHash::from_bytes(b"fixture"),
            )
            .with_name("Fixture Server"),
        )
        .expect("manifest item");
    fs::write(
        layout.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write store.json");

    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO agents (id, display_name, kind, version, installed) \
             VALUES ('claude_code', 'Claude Code', 'claude_code', '2.0.0', 1)",
            [],
        )
        .expect("agent row");
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, content_hash, state, managed) \
             VALUES (?1, 'mcp_server', 'user', 'fixture-server', 'Fixture Server', ?2, ?3, 'present', 1)",
            [
                item_id.to_string(),
                layout.mcp_file().display().to_string(),
                "0".repeat(64),
            ],
        )
        .expect("item row");
    let claude_json = agent_home(sandbox).join(".claude.json");
    at_db::deployments::upsert_deployment(
        connection,
        &at_db::deployments::DeploymentSpec::new(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
        )
        .with_mode("native")
        .with_target_path(claude_json.to_str().expect("utf-8 path")),
    )
    .expect("deployment row");
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, agent_id, server_name, transport, \
                                        checked_at, status, trigger, latency_ms, tools_count) \
             VALUES (?1, 'user', 'claude_code', 'Fixture Server', 'stdio', 1700000000, 'ok', 'manual', 12, 1)",
            [item_id.to_string()],
        )
        .expect("health row");
    item_id
}

fn json_output(output: &std::process::Output) -> Value {
    let text = stdout(output);
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON object ({error}): {text:?} stderr: {:?}",
            stderr(output)
        )
    })
}

#[test]
fn mcp_list_matches_the_ui_index_and_reports_state_and_health() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    seed_user_store(&sandbox, &server);
    let paths = sandbox.paths();
    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let ui_rows = at_db::mcp_index::mcp_list_rows(database.connection(), Some(Scope::User), None)
        .expect("UI list query");

    let output = agentuna(&sandbox, &["mcp", "list", "--scope", "user", "--json"]);
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(envelope["ok"], true);
    let servers = envelope["data"]["servers"].as_array().expect("servers");
    assert_eq!(
        servers.len(),
        ui_rows.len(),
        "mcp list counts what the UI list query returns"
    );
    assert_eq!(envelope["data"]["count"], servers.len());
    let row = servers
        .iter()
        .find(|row| row["id"] == "fixture-server")
        .expect("canonical server row");
    assert_eq!(row["scope"], "user");
    assert_eq!(row["transport"], "stdio");
    assert_eq!(row["health"], "ok");
    assert_eq!(row["agent_state"]["claude_code"]["state"], "linked");
}

#[test]
fn mcp_test_reports_tools_and_fails_with_the_error_envelope() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    seed_user_store(&sandbox, &server);

    let output = agentuna(
        &sandbox,
        &["mcp", "test", "fixture-server", "--scope", "user", "--json"],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["tools_count"], 1);
    assert_eq!(envelope["data"]["tools"][0], "ping");
    assert!(envelope["data"]["latency_ms"].is_u64());

    // A server whose executable is missing fails with the documented error envelope.
    let paths = sandbox.paths();
    let layout = StoreLayout::for_scope(&paths, Scope::User).expect("user layout");
    let mut store = McpStore::load(&layout).expect("load store");
    store.store.set(
        "fixture-server",
        at_model::McpServerDef {
            display_name: String::from("Fixture Server"),
            description: None,
            transport: at_model::McpTransport::Stdio {
                command: String::from("/nonexistent/agentuna-fixture-server"),
                args: Vec::new(),
                cwd: None,
                env: std::collections::BTreeMap::new(),
            },
            extra: std::collections::BTreeMap::new(),
        },
    );
    fs::write(layout.mcp_file(), store.store.to_bytes().expect("bytes")).expect("write mcp.json");

    let output = agentuna(
        &sandbox,
        &["mcp", "test", "fixture-server", "--scope", "user", "--json"],
    );
    assert_ne!(exit_code(&output), 0, "a failed test is not success");
    let envelope = json_output(&output);
    assert_eq!(envelope["ok"], false);
    assert!(envelope["error"]["code"].is_string());
    assert!(envelope["error"]["hint"].is_string());
}

#[test]
fn no_mcp_command_prints_the_literal_secret() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    seed_user_store(&sandbox, &server);
    let outputs = [
        agentuna(&sandbox, &["mcp", "list", "--scope", "user", "--json"]),
        agentuna(
            &sandbox,
            &["mcp", "show", "fixture-server", "--scope", "user", "--json"],
        ),
        agentuna(&sandbox, &["mcp", "list", "--scope", "user"]),
        agentuna(
            &sandbox,
            &["mcp", "show", "fixture-server", "--scope", "user"],
        ),
    ];
    for output in outputs {
        let combined = format!("{}{}", stdout(&output), stderr(&output));
        assert!(
            !combined.contains(SENTINEL),
            "command output leaked the literal secret: {combined}"
        );
    }
}

#[test]
fn mcp_disable_on_a_locked_server_exits_locked_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    let item_id = seed_user_store(&sandbox, &server);
    let paths = sandbox.paths();
    let claude_json = agent_home(&sandbox).join(".claude.json");
    fs::write(
        &claude_json,
        br#"{"mcpServers":{"fixture-server":{"command":"fake"}}}"#,
    )
    .expect("seed claude json");
    let before = fs::read(&claude_json).expect("read claude json");
    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    at_db::locks::upsert_lock(
        database.connection(),
        &at_db::locks::LockSpec {
            item_id: &item_id,
            scope: Scope::Global,
            reason: Some("approved version"),
            locked_by: Some("fixture"),
        },
    )
    .expect("lock row");

    let output = agentuna(
        &sandbox,
        &[
            "mcp",
            "disable",
            "fixture-server",
            "--agent",
            "claude_code",
            "--scope",
            "user",
            "--json",
        ],
    );
    // TU-033 publishes `locked` as exit code 5; TM-027's prose says 2, which the table wins.
    assert_eq!(
        exit_code(&output),
        at_cli::exit_code::LOCKED,
        "stdout: {} stderr: {}",
        stdout(&output),
        stderr(&output)
    );
    let envelope = json_output(&output);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "locked");
    assert_eq!(
        fs::read(&claude_json).expect("read claude json"),
        before,
        "a locked server is not written"
    );
}

#[test]
fn mcp_disable_and_enable_update_the_native_file_and_the_index() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    let item_id = seed_user_store(&sandbox, &server);
    let paths = sandbox.paths();
    let claude_json = agent_home(&sandbox).join(".claude.json");
    fs::write(
        &claude_json,
        br#"{"mcpServers":{"fixture-server":{"command":"fake"}},"other":true}"#,
    )
    .expect("seed claude json");

    let output = agentuna(
        &sandbox,
        &[
            "mcp",
            "disable",
            "fixture-server",
            "--agent",
            "claude_code",
            "--scope",
            "user",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(envelope["data"]["enabled"], false);
    let written: Value =
        serde_json::from_slice(&fs::read(&claude_json).expect("read")).expect("json");
    assert!(
        written["mcpServers"].get("fixture-server").is_none(),
        "disabling removes the entry: {written}"
    );
    assert_eq!(written["other"], true, "unrelated keys are preserved");
    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let cell = at_db::deployments::find_deployment(
        database.connection(),
        &item_id,
        &AgentId::ClaudeCode,
        Scope::User,
        None,
    )
    .expect("deployment lookup")
    .expect("deployment row");
    assert_eq!(cell.state, ItemState::Off);

    let output = agentuna(
        &sandbox,
        &[
            "mcp",
            "enable",
            "fixture-server",
            "--agent",
            "claude_code",
            "--scope",
            "user",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let written: Value =
        serde_json::from_slice(&fs::read(&claude_json).expect("read")).expect("json");
    assert!(
        written["mcpServers"].get("fixture-server").is_some(),
        "enabling writes the entry: {written}"
    );
    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let cell = at_db::deployments::find_deployment(
        database.connection(),
        &item_id,
        &AgentId::ClaudeCode,
        Scope::User,
        None,
    )
    .expect("deployment lookup")
    .expect("deployment row");
    assert_eq!(cell.state, ItemState::Linked);
}
