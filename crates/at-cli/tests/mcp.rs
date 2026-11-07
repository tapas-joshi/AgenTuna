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
use support::{Sandbox, agentuna, agentuna_with_stdin, exit_code, stderr, stdout};

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
        &at_db::deployments::DeploymentSpec::applied(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
            Some("native"),
            claude_json.to_str(),
            at_db::deployments::DeploymentHashes::none(),
        ),
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

#[test]
fn mcp_mutations_refuse_read_only_without_changing_store() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    seed_user_store(&sandbox, Path::new("fixture-server"));
    let layout = StoreLayout::for_scope(&sandbox.paths(), Scope::User).expect("user store");
    let before = fs::read(layout.mcp_file()).expect("MCP store bytes");

    let add_input = serde_json::json!({
        "id": "new-server",
        "definition": {
            "display_name": "New Server",
            "description": null,
            "transport": {
                "type": "stdio",
                "command": "fixture",
                "args": [],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string();
    let add = agentuna_with_stdin(
        &sandbox,
        &["--scope", "user", "--read-only", "mcp", "add", "--json"],
        add_input.as_bytes(),
    );
    assert_eq!(exit_code(&add), 5, "add: {}", stderr(&add));
    assert_eq!(json_output(&add)["error"]["code"], "locked");
    assert_eq!(
        fs::read(layout.mcp_file()).expect("MCP store bytes"),
        before
    );

    let edit_input = serde_json::json!({
        "id": "fixture-server",
        "definition": {
            "display_name": "Updated Fixture",
            "description": null,
            "transport": {
                "type": "stdio",
                "command": "fixture-updated",
                "args": [],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string();
    let edit = agentuna_with_stdin(
        &sandbox,
        &[
            "--scope",
            "user",
            "--read-only",
            "mcp",
            "edit",
            "fixture-server",
            "--json",
        ],
        edit_input.as_bytes(),
    );
    assert_eq!(exit_code(&edit), 5, "edit: {}", stderr(&edit));
    assert_eq!(json_output(&edit)["error"]["code"], "locked");
    assert_eq!(
        fs::read(layout.mcp_file()).expect("MCP store bytes"),
        before
    );

    let remove = agentuna(
        &sandbox,
        &[
            "--scope",
            "user",
            "--read-only",
            "--yes",
            "mcp",
            "remove",
            "fixture-server",
            "--json",
        ],
    );
    assert_eq!(exit_code(&remove), 5, "remove: {}", stderr(&remove));
    assert_eq!(json_output(&remove)["error"]["code"], "locked");
    assert_eq!(
        fs::read(layout.mcp_file()).expect("MCP store bytes"),
        before
    );
}

#[test]
fn mcp_enable_reports_unverified_omp_secret_refusal() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();
    let layout = StoreLayout::for_scope(&paths, Scope::User).expect("user store");
    fs::create_dir_all(layout.root()).expect("user store directory");

    let item_id = at_model::ItemId::new();
    let definition = at_model::McpServerDef {
        display_name: String::from("Fixture Server"),
        description: None,
        transport: at_model::McpTransport::Stdio {
            command: String::from("fixture-server"),
            args: Vec::new(),
            cwd: None,
            env: std::collections::BTreeMap::from([(
                String::from("TOKEN"),
                at_model::EnvValue::Secret(at_model::SecretRef {
                    env_name: String::from("API_TOKEN"),
                    keychain_account: String::from("fixture-account"),
                }),
            )]),
        },
        extra: std::collections::BTreeMap::new(),
    };
    let mut store = McpStore::empty();
    store.set("fixture-server", definition);
    let store_bytes = store.to_bytes().expect("MCP store bytes");
    let hash = ContentHash::from_bytes(&store_bytes);
    fs::write(layout.mcp_file(), &store_bytes).expect("write MCP store");

    let mut manifest = StoreManifest::new(Scope::User, at_store::layout::StoreId::new());
    manifest
        .insert_item(
            ManifestItem::new(
                item_id,
                ItemKind::McpServer,
                Slug::parse("fixture-server").expect("MCP slug"),
                StorePath::parse("mcp.json").expect("store path"),
                hash.clone(),
            )
            .with_name("Fixture Server"),
        )
        .expect("manifest item");
    fs::write(
        layout.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write manifest");

    let home = agent_home(&sandbox);
    let native = home.join(".omp/agent/mcp.json");
    fs::create_dir_all(native.parent().expect("OMP parent")).expect("OMP directory");
    fs::write(
        &native,
        "{\"mcpServers\":{\"fixture-server\":{\"command\":\"fixture-server\",\"args\":[],\"enabled\":false}}}\n",
    )
    .expect("native config");

    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO agents (id, display_name, kind, version, installed) \
             VALUES ('omp', 'omp', 'omp', '1.0.0', 1)",
            [],
        )
        .expect("agent row");
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, content_hash, state, managed) \
             VALUES (?1, 'mcp_server', 'user', ?2, ?3, ?4, ?5, 'present', 1)",
            [
                item_id.to_string(),
                String::from("fixture-server"),
                String::from("Fixture Server"),
                layout.mcp_file().display().to_string(),
                hash.as_str().to_owned(),
            ],
        )
        .expect("item row");

    let output = agentuna(
        &sandbox,
        &[
            "--scope",
            "user",
            "mcp",
            "enable",
            "fixture-server",
            "--agent",
            "omp",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    assert_eq!(envelope["ok"], true);
    assert!(envelope["warnings"].as_array().is_some_and(|warnings| {
        warnings.iter().any(|warning| {
            warning["message"]
                .as_str()
                .is_some_and(|message| message.contains("refused secret"))
        })
    }));
    let native_text = fs::read_to_string(native).expect("updated native config");
    let native_config: Value = serde_json::from_str(&native_text).expect("native JSON config");
    assert_eq!(
        native_config["mcpServers"]["fixture-server"]["enabled"],
        true
    );
    assert!(!native_text.contains("API_TOKEN"));
    assert!(!native_text.contains("fixture-account"));
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

fn files_under(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else if path.is_file() {
            files.push(path);
        }
    }
    files
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
        br#"{"mcpServers":{"fixture-server":{"command":"fake","env":{"TOKEN":"${REMOTE_TOKEN}"}}},"other":true}"#,
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
    let backups = files_under(paths.backup_dir());
    assert!(
        !backups.is_empty(),
        "disable flow created an operation backup"
    );
    let backup_bytes = backups
        .iter()
        .map(|path| fs::read(path).expect("read MCP backup"))
        .collect::<Vec<_>>();
    assert!(
        backup_bytes.iter().any(|bytes| bytes
            .windows(b"${REMOTE_TOKEN}".len())
            .any(|window| window == b"${REMOTE_TOKEN}")),
        "the backup preserves the ${{NAME}} reference"
    );
    assert!(
        backup_bytes.iter().all(|bytes| !bytes
            .windows(SENTINEL.len())
            .any(|window| window == SENTINEL.as_bytes())),
        "MCP backups never contain the plaintext sentinel"
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

/// Seeds one canonical MCP server (store, manifest, index row) without a deployment.
fn seed_server(sandbox: &Sandbox, slug: &str, command: &str) -> at_model::ItemId {
    let paths = sandbox.paths();
    let layout = StoreLayout::for_scope(&paths, Scope::User).expect("user layout");
    fs::create_dir_all(layout.root()).expect("user store");
    let definition = at_model::McpServerDef {
        display_name: slug.to_owned(),
        description: None,
        transport: at_model::McpTransport::Stdio {
            command: command.to_owned(),
            args: Vec::new(),
            cwd: None,
            env: std::collections::BTreeMap::new(),
        },
        extra: std::collections::BTreeMap::new(),
    };
    let mut store = McpStore::empty();
    store.set(slug, definition.clone());
    fs::write(layout.mcp_file(), store.to_bytes().expect("MCP bytes")).expect("write mcp.json");

    let hash = at_store::manifest::hash_mcp_entry(
        &serde_json::to_value(&definition).expect("definition value"),
    )
    .expect("entry hash");
    let item_id = at_model::ItemId::new();
    let mut manifest = StoreManifest::new(Scope::User, at_store::layout::StoreId::new());
    manifest
        .insert_item(
            ManifestItem::new(
                item_id,
                ItemKind::McpServer,
                Slug::parse(slug).expect("slug"),
                StorePath::parse("mcp.json").expect("path"),
                hash,
            )
            .with_name(slug),
        )
        .expect("manifest item");
    fs::write(
        layout.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write store.json");

    let database = at_db::Database::open_file(paths.db_path()).expect("index");
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, content_hash, state, managed) \
             VALUES (?1, 'mcp_server', 'user', ?2, ?2, ?3, ?4, 'present', 1)",
            [
                item_id.to_string(),
                slug.to_owned(),
                layout.mcp_file().display().to_string(),
                "0".repeat(64),
            ],
        )
        .expect("item row");
    item_id
}

#[test]
fn mcp_add_records_the_manifest_identity_and_can_be_enabled() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    agent_home(&sandbox);
    let input = serde_json::json!({
        "id": "added-server",
        "definition": {
            "display_name": "Added Server",
            "description": null,
            "transport": {
                "type": "stdio",
                "command": "fixture-server",
                "args": [],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string();
    let output = agentuna_with_stdin(
        &sandbox,
        &["mcp", "add", "--scope", "user", "--json"],
        input.as_bytes(),
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));
    let envelope = json_output(&output);
    let item_id = envelope["data"]["item_id"]
        .as_str()
        .expect("add reports the identity")
        .to_owned();

    let layout = StoreLayout::for_scope(&sandbox.paths(), Scope::User).expect("user layout");
    let manifest = StoreManifest::load(&layout).expect("manifest").manifest;
    assert_eq!(
        manifest
            .item_by_slug(
                ItemKind::McpServer,
                &Slug::parse("added-server").expect("slug")
            )
            .map(|item| item.id.to_string()),
        Some(item_id.clone()),
        "store.json must record the new server"
    );
    let listed = json_output(&agentuna(
        &sandbox,
        &["mcp", "list", "--scope", "user", "--json"],
    ));
    let row = listed["data"]["servers"]
        .as_array()
        .expect("servers")
        .iter()
        .find(|row| row["id"] == serde_json::json!("added-server"))
        .unwrap_or_else(|| panic!("the added server is listed: {listed}"));
    assert_eq!(row["transport"], serde_json::json!("stdio"), "{listed}");
    assert_eq!(row["item_id"], serde_json::json!(item_id), "{listed}");

    let enabled = agentuna(
        &sandbox,
        &[
            "mcp",
            "enable",
            "added-server",
            "--agent",
            "claude_code",
            "--scope",
            "user",
            "--json",
        ],
    );
    assert_eq!(exit_code(&enabled), 0, "stderr: {}", stderr(&enabled));
    let claude_json = agent_home(&sandbox).join(".claude.json");
    let written: Value =
        serde_json::from_slice(&fs::read(&claude_json).expect("read")).expect("json");
    assert_eq!(
        written["mcpServers"]["added-server"]["command"],
        serde_json::json!("fixture-server"),
        "enabling writes the added definition: {written}"
    );
}

#[test]
fn mcp_edit_accepts_an_imported_underscore_id() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    seed_server(&sandbox, "claude_alpha", "old-command");
    let input = serde_json::json!({
        "id": "claude_alpha",
        "definition": {
            "display_name": "claude_alpha",
            "description": null,
            "transport": {
                "type": "stdio",
                "command": "new-command",
                "args": ["--flag"],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string();
    let output = agentuna_with_stdin(
        &sandbox,
        &["mcp", "edit", "claude_alpha", "--scope", "user", "--json"],
        input.as_bytes(),
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));

    let layout = StoreLayout::for_scope(&sandbox.paths(), Scope::User).expect("user layout");
    let store = McpStore::load(&layout).expect("MCP store").store;
    let definition = store.get("claude_alpha").expect("edited definition");
    match &definition.transport {
        at_model::McpTransport::Stdio { command, args, .. } => {
            assert_eq!(command, "new-command");
            assert_eq!(args, &vec!["--flag".to_owned()]);
        }
        other => panic!("expected a stdio definition, got {other:?}"),
    }
}

#[test]
fn mcp_remove_clears_the_manifest_and_the_index() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let server = fake_server_script(&sandbox);
    let item_id = seed_user_store(&sandbox, &server);
    let claude_json = agent_home(&sandbox).join(".claude.json");
    fs::write(
        &claude_json,
        br#"{"mcpServers":{"fixture-server":{"command":"fixture-server"}},"keep":true}"#,
    )
    .expect("seed claude json");

    let output = agentuna(
        &sandbox,
        &[
            "mcp",
            "remove",
            "fixture-server",
            "--yes",
            "--scope",
            "user",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "stderr: {}", stderr(&output));

    let layout = StoreLayout::for_scope(&sandbox.paths(), Scope::User).expect("user layout");
    let store = McpStore::load(&layout).expect("MCP store").store;
    assert!(store.get("fixture-server").is_none(), "mcp.json drops it");
    let manifest = StoreManifest::load(&layout).expect("manifest").manifest;
    assert!(
        manifest.item(item_id).is_none(),
        "store.json drops the item record"
    );
    let database = at_db::Database::open_file(sandbox.paths().db_path()).expect("index");
    assert!(
        at_db::items::item_id_by_identity(
            database.connection(),
            ItemKind::McpServer,
            Scope::User,
            None,
            "fixture-server",
        )
        .expect("item lookup")
        .is_none(),
        "the index row is removed"
    );
    let listed = json_output(&agentuna(
        &sandbox,
        &["mcp", "list", "--scope", "user", "--json"],
    ));
    assert_eq!(
        listed["data"]["count"],
        serde_json::json!(0),
        "a removed server is not listed: {listed}"
    );
    let written: Value =
        serde_json::from_slice(&fs::read(&claude_json).expect("read")).expect("json");
    assert!(
        written["mcpServers"].get("fixture-server").is_none(),
        "the native entry is removed: {written}"
    );
    assert_eq!(written["keep"], true, "unrelated keys survive: {written}");
}

#[test]
fn mcp_enable_does_not_leave_the_native_file_looking_drifted() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    agent_home(&sandbox);
    seed_server(&sandbox, "added-server", "fixture-server");
    let enabled = agentuna(
        &sandbox,
        &[
            "mcp",
            "enable",
            "added-server",
            "--agent",
            "claude_code",
            "--scope",
            "user",
            "--json",
        ],
    );
    assert_eq!(exit_code(&enabled), 0, "stderr: {}", stderr(&enabled));

    // The enable wrote the file; the recorded baseline must be that file, not the old bytes.
    let listed = agentuna(&sandbox, &["drift", "list", "--scope", "user", "--json"]);
    assert_eq!(exit_code(&listed), 0, "stderr: {}", stderr(&listed));
    assert_eq!(
        json_output(&listed)["data"]["events"],
        Value::Array(Vec::new()),
        "our own enable must not look like drift: {}",
        stdout(&listed)
    );
}

/// `mcp add`/`mcp edit` stdin payload for one stdio server.
fn server_input(id: &str, display_name: &str, command: &str) -> String {
    serde_json::json!({
        "id": id,
        "definition": {
            "display_name": display_name,
            "description": null,
            "transport": {
                "type": "stdio",
                "command": command,
                "args": [],
                "cwd": null,
                "env": {}
            }
        }
    })
    .to_string()
}

#[test]
fn mcp_copy_requires_confirmation_then_copies_the_definition_into_the_project() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project directory");
    // A real machine always has a home directory; the native configs live under it.
    fs::create_dir_all(sandbox.path("home")).expect("home directory");
    let project_arg = project.to_str().expect("UTF-8 sandbox path").to_owned();
    let registered = agentuna(&sandbox, &["projects", "add", &project_arg, "--json"]);
    assert_eq!(exit_code(&registered), 0, "stderr: {}", stderr(&registered));

    let added = agentuna_with_stdin(
        &sandbox,
        &["mcp", "add", "--scope", "user", "--json"],
        server_input("web-search", "Web Search", "web-search-server").as_bytes(),
    );
    assert_eq!(exit_code(&added), 0, "stderr: {}", stderr(&added));
    let source_id = json_output(&added)["data"]["item_id"]
        .as_str()
        .expect("add reports the identity")
        .to_owned();
    // The project scope must already have a store, which the first project add creates.
    let seeded = agentuna_with_stdin(
        &sandbox,
        &[
            "mcp",
            "add",
            "--scope",
            "project",
            "--project",
            &project_arg,
            "--json",
        ],
        server_input("project-only", "Project Only", "project-server").as_bytes(),
    );
    assert_eq!(exit_code(&seeded), 0, "stderr: {}", stderr(&seeded));

    let unconfirmed = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&unconfirmed),
        at_cli::exit_code::CONFIRMATION_REQUIRED,
        "stderr: {}",
        stderr(&unconfirmed)
    );

    let dry = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(exit_code(&dry), 0, "stderr: {}", stderr(&dry));
    let dry_json = json_output(&dry);
    assert_eq!(
        dry_json["data"]["target_scope"],
        serde_json::json!("project")
    );
    assert_eq!(
        dry_json["data"]["target_slug"],
        serde_json::json!("web-search")
    );
    assert_eq!(dry_json["data"]["dry_run"], serde_json::json!(true));
    assert!(
        !dry_json["data"]["files"]
            .as_array()
            .expect("reviewed files")
            .is_empty(),
        "a dry run names the files it would write: {dry_json}"
    );
    let project_paths = sandbox
        .paths()
        .for_project(&project)
        .expect("project paths");
    let project_layout =
        StoreLayout::for_scope(&project_paths, Scope::Project).expect("project layout");
    assert!(
        McpStore::load(&project_layout)
            .expect("project store")
            .store
            .get("web-search")
            .is_none(),
        "a dry run writes nothing"
    );

    let applied = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&applied), 0, "stderr: {}", stderr(&applied));
    let applied_json = json_output(&applied);
    let copied_id = applied_json["data"]["target_item_id"]
        .as_str()
        .expect("the copy reports its identity")
        .to_owned();
    assert_ne!(copied_id, source_id, "a copy is a new item");
    let project_store = McpStore::load(&project_layout)
        .expect("project store")
        .store;
    assert!(
        project_store.get("web-search").is_some(),
        "the definition is copied into the project store"
    );
    assert!(
        project_store.get("project-only").is_some(),
        "unrelated project servers stay"
    );
    let native = fs::read_to_string(project.join("mcp.json")).expect("project agent document");
    assert!(
        native.contains("web-search"),
        "the project agent document holds the copy: {native}"
    );
    let user_layout = StoreLayout::for_scope(&sandbox.paths(), Scope::User).expect("user layout");
    assert!(
        McpStore::load(&user_layout)
            .expect("user store")
            .store
            .get("web-search")
            .is_some(),
        "a copy keeps the source definition"
    );

    // The destination already holds this exact definition, so a second copy is refused.
    let again = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--yes",
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&again),
        at_cli::exit_code::CONFLICT,
        "stderr: {}",
        stderr(&again)
    );

    // A changed source definition needs an explicit --on-collision decision.
    let edited = agentuna_with_stdin(
        &sandbox,
        &["mcp", "edit", "web-search", "--scope", "user", "--json"],
        server_input("web-search", "Web Search", "web-search-server-v2").as_bytes(),
    );
    assert_eq!(exit_code(&edited), 0, "stderr: {}", stderr(&edited));
    let collision = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--yes",
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&collision),
        at_cli::exit_code::CONFLICT,
        "stderr: {}",
        stderr(&collision)
    );
    assert!(
        json_output(&collision)["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("choose Replace or Rename")),
        "the refusal names the decision: {}",
        stdout(&collision)
    );
    let renamed = agentuna(
        &sandbox,
        &[
            "mcp",
            "copy",
            "web-search",
            "--to",
            "project",
            "--project",
            &project_arg,
            "--on-collision",
            "rename",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&renamed), 0, "stderr: {}", stderr(&renamed));
    assert_eq!(
        json_output(&renamed)["data"]["target_slug"],
        serde_json::json!("web-search-2"),
        "Rename derives a free lowercase id"
    );
}
