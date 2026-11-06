//! TF-046: golden-file round-trip corpus for the Codex TOML and omp JSONC adapters.
//!
//! Every case copies a synthetic fixture into a sandboxed home, runs one adapter edit through
//! the adapter's real load/save path, and compares the bytes on disk with
//! `tests/golden/<case>/expected.<ext>`. `UPDATE_GOLDEN=1` rewrites the expected files; a
//! regeneration still reparses the output and runs the semantic check, so a broken patch is not
//! blessed as golden.
//!
//! `at-adapters/src/golden.rs` is included by path rather than imported from the crate: the
//! harness is test support and is deliberately absent from the production API surface.

#[path = "../src/golden.rs"]
mod harness;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use at_adapters::claude_mcp::{ClaudeConfigScope, ClaudeMcpAdapter, ClaudeMcpConfig, ClaudeServer};
use at_adapters::codex_mcp::{CodexConfigFile, CodexServerEntry};
use at_adapters::omp_mcp::{OmpMcpAdapter, OmpMcpConfig, OmpServer};
use at_fs::BackupPolicy;
use at_fs::testkit::TestHome;
use at_model::mcp::{EnvValue, McpTransport, SecretRef};
use harness::{Format, GoldenCase, Suite, single_edit_span};
use url::Url;

const CLAUDE_GOLDEN: &str = include_str!("fixtures/claude/golden.json");
const CODEX_GOLDEN: &str = include_str!("fixtures/codex/golden.toml");
const OMP_GOLDEN: &str = include_str!("fixtures/omp/golden_mcp.json");

// ---------------------------------------------------------------------------------------------
// Claude Code JSONC cases
// ---------------------------------------------------------------------------------------------

fn apply_claude_noop(path: &Path) -> Result<(), String> {
    let config = ClaudeMcpAdapter::read(path, ClaudeConfigScope::User)
        .map_err(|error| error.to_string())?
        .ok_or("missing Claude config")?;
    config
        .save_edit(path, config.render_unchanged(), BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn check_claude_noop(text: &str) -> Result<(), String> {
    if text == CLAUDE_GOLDEN {
        Ok(())
    } else {
        Err("Claude no-op changed bytes".to_owned())
    }
}

fn apply_claude_unicode_server(path: &Path) -> Result<(), String> {
    let config = ClaudeMcpAdapter::read(path, ClaudeConfigScope::User)
        .map_err(|error| error.to_string())?
        .ok_or("missing Claude config")?;
    let server = ClaudeServer {
        name: "mcp.测试-server".to_owned(),
        transport: McpTransport::Stdio {
            command: "node".to_owned(),
            args: vec!["server.js".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    };
    let edited = config.upsert(&server).map_err(|error| error.to_string())?;
    config
        .save_edit(path, &edited, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn check_claude_unicode(text: &str) -> Result<(), String> {
    let config =
        ClaudeMcpConfig::parse(text, ClaudeConfigScope::User).map_err(|error| error.to_string())?;
    if config.servers().contains_key("mcp.测试-server") {
        Ok(())
    } else {
        Err("Unicode server name was not retained".to_owned())
    }
}

fn apply_claude_project_server(path: &Path) -> Result<(), String> {
    let scope = ClaudeConfigScope::Project {
        project_path: "/workspace/demo".to_owned(),
    };
    let config = ClaudeMcpAdapter::read(path, scope)
        .map_err(|error| error.to_string())?
        .ok_or("missing Claude config")?;
    let server = ClaudeServer {
        name: "project-server".to_owned(),
        transport: McpTransport::Stdio {
            command: "python".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    };
    let edited = config.upsert(&server).map_err(|error| error.to_string())?;
    config
        .save_edit(path, &edited, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn check_claude_project(text: &str) -> Result<(), String> {
    let config = ClaudeMcpConfig::parse(
        text,
        ClaudeConfigScope::Project {
            project_path: "/workspace/demo".to_owned(),
        },
    )
    .map_err(|error| error.to_string())?;
    if config.servers().contains_key("project-server") {
        Ok(())
    } else {
        Err("project server missing from project-specific map".to_owned())
    }
}

fn apply_claude_project_file_noop(path: &Path) -> Result<(), String> {
    let config = ClaudeMcpAdapter::read(path, ClaudeConfigScope::ProjectFile)
        .map_err(|error| error.to_string())?
        .ok_or("missing project MCP config")?;
    config
        .save_edit(path, config.render_unchanged(), BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}
// ---------------------------------------------------------------------------------------------
// Codex TOML cases
// ---------------------------------------------------------------------------------------------

fn load_codex(path: &Path) -> Result<CodexConfigFile, String> {
    CodexConfigFile::load(path).map_err(|error| error.to_string())
}

fn save_codex(config: &mut CodexConfigFile, path: &Path) -> Result<(), String> {
    config
        .save(path, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_codex_add_server(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    let mut env = BTreeMap::new();
    env.insert(
        "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
        EnvValue::Secret(SecretRef {
            env_name: "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
            keychain_account: "agentuna/github".to_owned(),
        }),
    );
    let entry = CodexServerEntry {
        command: Some("npx".to_owned()),
        args: Some(vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-github".to_owned(),
        ]),
        env: Some(env),
        ..CodexServerEntry::default()
    };
    config
        .set_server("github", &entry)
        .map_err(|error| error.to_string())?;
    save_codex(&mut config, path)
}

fn apply_codex_edit_env(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    let mut env = BTreeMap::new();
    env.insert(
        "NODE_REPL_MODE".to_owned(),
        EnvValue::Literal("sandbox".to_owned()),
    );
    env.insert(
        "NODE_REPL_DEBUG".to_owned(),
        EnvValue::Literal("1".to_owned()),
    );
    let entry = CodexServerEntry {
        env: Some(env),
        ..CodexServerEntry::default()
    };
    config
        .set_server("node_repl", &entry)
        .map_err(|error| error.to_string())?;
    save_codex(&mut config, path)
}

fn apply_codex_remove_server(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    config
        .remove_server("node_repl")
        .map_err(|error| error.to_string())?;
    save_codex(&mut config, path)
}

fn apply_codex_noop(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    save_codex(&mut config, path)
}

fn apply_codex_add_quoted_id(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    let entry = CodexServerEntry {
        command: Some("two".to_owned()),
        ..CodexServerEntry::default()
    };
    config
        .set_server("a.b", &entry)
        .map_err(|error| error.to_string())?;
    save_codex(&mut config, path)
}

fn apply_codex_crlf_add_server(path: &Path) -> Result<(), String> {
    let mut config = load_codex(path)?;
    let entry = CodexServerEntry {
        command: Some("npx".to_owned()),
        args: Some(vec!["-y".to_owned(), "node-repl-mcp".to_owned()]),
        ..CodexServerEntry::default()
    };
    config
        .set_server("node_repl", &entry)
        .map_err(|error| error.to_string())?;
    save_codex(&mut config, path)
}

fn check_codex_add(text: &str) -> Result<(), String> {
    let config = CodexConfigFile::parse(text).map_err(|error| error.to_string())?;
    let server = config
        .server("github")
        .map_err(|error| error.to_string())?
        .ok_or("`github` was not added")?;
    if server.entry.command.as_deref() != Some("npx") {
        return Err(format!("command is {:?}", server.entry.command));
    }
    let env = server.entry.env.as_ref().ok_or("`env` is missing")?;
    if env.get("GITHUB_PERSONAL_ACCESS_TOKEN")
        != Some(&EnvValue::Literal(
            "${GITHUB_PERSONAL_ACCESS_TOKEN}".to_owned(),
        ))
    {
        return Err(format!("the secret reference is {env:?}"));
    }
    let playwright = config
        .server("playwright")
        .map_err(|error| error.to_string())?
        .ok_or("`playwright` was removed")?;
    if playwright.entry.command.as_deref() != Some("npx") {
        return Err("`playwright` was rewritten".to_owned());
    }
    if config
        .server("shuttle")
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Err("`shuttle` was removed".to_owned());
    }
    Ok(())
}

fn check_codex_env(text: &str) -> Result<(), String> {
    let config = CodexConfigFile::parse(text).map_err(|error| error.to_string())?;
    let server = config
        .server("node_repl")
        .map_err(|error| error.to_string())?
        .ok_or("`node_repl` is missing")?;
    let env = server.entry.env.as_ref().ok_or("`env` is missing")?;
    if env.get("NODE_REPL_DEBUG") != Some(&EnvValue::Literal("1".to_owned())) {
        return Err(format!("`env` is {env:?}"));
    }
    if server.entry.command.as_deref() != Some("npx") {
        return Err("`command` was rewritten by an env edit".to_owned());
    }
    Ok(())
}

fn check_codex_removed(text: &str) -> Result<(), String> {
    let config = CodexConfigFile::parse(text).map_err(|error| error.to_string())?;
    if config
        .server("node_repl")
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("`node_repl` survived removal".to_owned());
    }
    if config
        .server("playwright")
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Err("removal took `playwright` with it".to_owned());
    }
    Ok(())
}

fn check_codex_noop(text: &str) -> Result<(), String> {
    if text == CODEX_GOLDEN {
        Ok(())
    } else {
        Err("a no-op save changed bytes".to_owned())
    }
}

fn check_codex_quoted(text: &str) -> Result<(), String> {
    if !text.contains("[mcp_servers.\"a.b\"]") {
        return Err("the dotted id was not written as a quoted table".to_owned());
    }
    let config = CodexConfigFile::parse(text).map_err(|error| error.to_string())?;
    if config
        .server("plain")
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Err("the existing `plain` server was removed".to_owned());
    }
    Ok(())
}

fn check_codex_crlf(text: &str) -> Result<(), String> {
    assert_no_lone_lf(text)?;
    let config = CodexConfigFile::parse(text).map_err(|error| error.to_string())?;
    let server = config
        .server("node_repl")
        .map_err(|error| error.to_string())?
        .ok_or("`node_repl` was not added")?;
    if server.entry.command.as_deref() != Some("npx") {
        return Err(format!("command is {:?}", server.entry.command));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// omp JSONC cases
// ---------------------------------------------------------------------------------------------

fn apply_omp_add_http(path: &Path) -> Result<(), String> {
    let url = Url::parse("https://mcp.example.test/second").map_err(|error| error.to_string())?;
    let server = OmpServer::new(
        "added",
        McpTransport::Http {
            url,
            headers: BTreeMap::new(),
            oauth: None,
        },
        true,
    );
    OmpMcpAdapter::new()
        .upsert_server(path, &server, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_omp_edit_env(path: &Path) -> Result<(), String> {
    let adapter = OmpMcpAdapter::new();
    let config = adapter
        .read(path)
        .map_err(|error| error.to_string())?
        .ok_or("fixture is missing")?;
    let mut server = config
        .server("local-tool")
        .ok_or("`local-tool` is missing")?
        .clone();
    if let McpTransport::Stdio { env, .. } = &mut server.transport {
        env.insert(
            "TOOL_HOME".to_owned(),
            EnvValue::Literal("/tmp/tool-v2".to_owned()),
        );
    }
    let text = config
        .upsert_server(&server)
        .map_err(|error| error.to_string())?;
    adapter
        .save_edit(path, &config, &text, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_omp_remove_server(path: &Path) -> Result<(), String> {
    OmpMcpAdapter::new()
        .remove_server(path, "remote-tool", BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_omp_noop(path: &Path) -> Result<(), String> {
    let adapter = OmpMcpAdapter::new();
    let config = adapter
        .read(path)
        .map_err(|error| error.to_string())?
        .ok_or("fixture is missing")?;
    let server = config
        .server("local-tool")
        .ok_or("`local-tool` is missing")?
        .clone();
    let text = config
        .upsert_server(&server)
        .map_err(|error| error.to_string())?;
    adapter
        .save_edit(path, &config, &text, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_omp_commented_add(path: &Path) -> Result<(), String> {
    let adapter = OmpMcpAdapter::new();
    let config = adapter
        .read(path)
        .map_err(|error| error.to_string())?
        .ok_or("fixture is missing")?;
    let server = OmpServer::new(
        "beta",
        McpTransport::Stdio {
            command: "beta-mcp".to_owned(),
            args: vec!["--stdio".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    );
    let text = config
        .upsert_server(&server)
        .map_err(|error| error.to_string())?;
    adapter
        .save_edit(path, &config, &text, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn apply_omp_crlf_add(path: &Path) -> Result<(), String> {
    let adapter = OmpMcpAdapter::new();
    let config = adapter
        .read(path)
        .map_err(|error| error.to_string())?
        .ok_or("fixture is missing")?;
    let url = Url::parse("https://mcp.example.test/crlf").map_err(|error| error.to_string())?;
    let server = OmpServer::new(
        "crlf",
        McpTransport::Http {
            url,
            headers: BTreeMap::new(),
            oauth: None,
        },
        true,
    );
    let text = config
        .upsert_server(&server)
        .map_err(|error| error.to_string())?;
    adapter
        .save_edit(path, &config, &text, BackupPolicy::None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn check_omp_add(text: &str) -> Result<(), String> {
    let config = OmpMcpConfig::parse(text).map_err(|error| error.to_string())?;
    let server = config.server("added").ok_or("`added` was not written")?;
    if !server.transport.is_http() || !server.enabled {
        return Err(format!("`added` is {server:?}"));
    }
    for name in ["local-tool", "remote-tool"] {
        if config.server(name).is_none() {
            return Err(format!("`{name}` was removed"));
        }
    }
    Ok(())
}

fn check_omp_env(text: &str) -> Result<(), String> {
    let config = OmpMcpConfig::parse(text).map_err(|error| error.to_string())?;
    let server = config
        .server("local-tool")
        .ok_or("`local-tool` is missing")?;
    let McpTransport::Stdio { env, .. } = &server.transport else {
        return Err("`local-tool` is no longer stdio".to_owned());
    };
    if env.get("TOOL_HOME") != Some(&EnvValue::Literal("/tmp/tool-v2".to_owned())) {
        return Err(format!("`env` is {env:?}"));
    }
    if env.get("API_KEY") != Some(&EnvValue::Literal("${API_KEY}".to_owned())) {
        return Err("the neighbouring environment entry was rewritten".to_owned());
    }
    if server.extra.get("timeout") != Some(&serde_json::json!(20000)) {
        return Err("the unmodelled `timeout` key was dropped".to_owned());
    }
    Ok(())
}

fn check_omp_removed(text: &str) -> Result<(), String> {
    let config = OmpMcpConfig::parse(text).map_err(|error| error.to_string())?;
    if config.server("remote-tool").is_some() {
        return Err("`remote-tool` survived removal".to_owned());
    }
    if config.server("local-tool").is_none() {
        return Err("removal took `local-tool` with it".to_owned());
    }
    Ok(())
}

fn check_omp_noop(text: &str) -> Result<(), String> {
    if text == OMP_GOLDEN {
        Ok(())
    } else {
        Err("a no-op upsert changed bytes".to_owned())
    }
}

fn check_omp_commented(text: &str) -> Result<(), String> {
    let config = OmpMcpConfig::parse(text).map_err(|error| error.to_string())?;
    for name in ["alpha", "beta"] {
        if config.server(name).is_none() {
            return Err(format!("`{name}` is missing"));
        }
    }
    for marker in [
        "// keep this comment",
        "/* The servers block is the part AgenTuna edits. */",
        "\"--verbose\", // trailing comma on the next line",
        "\"legacy\", // trailing comma keeps the array multiline",
    ] {
        if !text.contains(marker) {
            return Err(format!("comment or trailing comma lost: {marker}"));
        }
    }
    Ok(())
}

fn check_omp_crlf(text: &str) -> Result<(), String> {
    assert_no_lone_lf(text)?;
    let config = OmpMcpConfig::parse(text).map_err(|error| error.to_string())?;
    if config.server("crlf").is_none() {
        return Err("`crlf` was not written".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Harness self-tests
// ---------------------------------------------------------------------------------------------

fn append_comment(path: &Path) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    fs::write(path, format!("{text}# edited\n")).map_err(|error| error.to_string())
}

fn accept_anything(_text: &str) -> Result<(), String> {
    Ok(())
}

/// Every newline in `text` is preceded by a carriage return.
fn assert_no_lone_lf(text: &str) -> Result<(), String> {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r') {
            return Err(format!("bare LF at byte {index}"));
        }
    }
    Ok(())
}

fn case(id: &str) -> &'static GoldenCase {
    CASES
        .iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("case `{id}` is missing"))
}

// ---------------------------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------------------------

const CASES: [GoldenCase; 16] = [
    GoldenCase {
        id: "claude-project-file-noop",
        fixture: "claude/golden.json",
        format: Format::Jsonc,
        apply: apply_claude_project_file_noop,
        check: check_claude_noop,
        unchanged: true,
    },
    GoldenCase {
        id: "claude-project-server",
        fixture: "claude/golden.json",
        format: Format::Jsonc,
        apply: apply_claude_project_server,
        check: check_claude_project,
        unchanged: false,
    },
    GoldenCase {
        id: "claude-noop",
        fixture: "claude/golden.json",
        format: Format::Jsonc,
        apply: apply_claude_noop,
        check: check_claude_noop,
        unchanged: true,
    },
    GoldenCase {
        id: "claude-unicode-server",
        fixture: "claude/golden.json",
        format: Format::Jsonc,
        apply: apply_claude_unicode_server,
        check: check_claude_unicode,
        unchanged: false,
    },
    GoldenCase {
        id: "codex-add-server",
        fixture: "codex/golden.toml",
        format: Format::Toml,
        apply: apply_codex_add_server,
        check: check_codex_add,
        unchanged: false,
    },
    GoldenCase {
        id: "codex-edit-env",
        fixture: "codex/golden.toml",
        format: Format::Toml,
        apply: apply_codex_edit_env,
        check: check_codex_env,
        unchanged: false,
    },
    GoldenCase {
        id: "codex-remove-server",
        fixture: "codex/golden.toml",
        format: Format::Toml,
        apply: apply_codex_remove_server,
        check: check_codex_removed,
        unchanged: false,
    },
    GoldenCase {
        id: "codex-noop",
        fixture: "codex/golden.toml",
        format: Format::Toml,
        apply: apply_codex_noop,
        check: check_codex_noop,
        unchanged: true,
    },
    GoldenCase {
        id: "codex-add-quoted-id",
        fixture: "codex/dotted.toml",
        format: Format::Toml,
        apply: apply_codex_add_quoted_id,
        check: check_codex_quoted,
        unchanged: false,
    },
    GoldenCase {
        id: "codex-crlf-add-server",
        fixture: "codex/crlf.toml",
        format: Format::Toml,
        apply: apply_codex_crlf_add_server,
        check: check_codex_crlf,
        unchanged: false,
    },
    GoldenCase {
        id: "omp-add-http-server",
        fixture: "omp/golden_mcp.json",
        format: Format::Jsonc,
        apply: apply_omp_add_http,
        check: check_omp_add,
        unchanged: false,
    },
    GoldenCase {
        id: "omp-edit-env",
        fixture: "omp/golden_mcp.json",
        format: Format::Jsonc,
        apply: apply_omp_edit_env,
        check: check_omp_env,
        unchanged: false,
    },
    GoldenCase {
        id: "omp-remove-server",
        fixture: "omp/golden_mcp.json",
        format: Format::Jsonc,
        apply: apply_omp_remove_server,
        check: check_omp_removed,
        unchanged: false,
    },
    GoldenCase {
        id: "omp-noop",
        fixture: "omp/golden_mcp.json",
        format: Format::Jsonc,
        apply: apply_omp_noop,
        check: check_omp_noop,
        unchanged: true,
    },
    GoldenCase {
        id: "omp-commented-add-server",
        fixture: "omp/commented.json",
        format: Format::Jsonc,
        apply: apply_omp_commented_add,
        check: check_omp_commented,
        unchanged: false,
    },
    GoldenCase {
        id: "omp-crlf-add-server",
        fixture: "omp/crlf.json",
        format: Format::Jsonc,
        apply: apply_omp_crlf_add,
        check: check_omp_crlf,
        unchanged: false,
    },
];

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_corpus_matches_expected_bytes() {
    let suite = Suite::workspace().expect("sandbox");
    let started = Instant::now();
    let outcome = suite
        .run(&CASES)
        .unwrap_or_else(|failures| panic!("{failures}"));
    let elapsed = started.elapsed();
    assert_eq!(outcome.cases, 16);
    assert_eq!(outcome.cases, CASES.len());
    if !harness::update_requested() {
        assert!(outcome.updated.is_empty(), "compare mode rewrote a file");
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "the offline corpus took {elapsed:?}"
    );
}

#[test]
fn additions_only_insert_bytes() {
    let suite = Suite::workspace().expect("sandbox");
    for id in [
        "claude-project-server",
        "claude-unicode-server",
        "codex-add-server",
        "codex-add-quoted-id",
        "codex-crlf-add-server",
        "omp-add-http-server",
        "omp-commented-add-server",
        "omp-crlf-add-server",
    ] {
        let (source, actual) = suite.apply_case(case(id)).expect("apply");
        let span = single_edit_span(&source, &actual);
        assert_eq!(span.removed, 0, "{id} rewrote existing bytes");
        assert_eq!(
            &source[..span.start],
            &actual[..span.start],
            "{id} changed bytes before the insertion point"
        );
        assert_eq!(
            actual.len() - source.len(),
            span.inserted.len(),
            "{id} byte length delta does not equal the inserted text length"
        );
        assert_eq!(span.length_delta(), span.inserted.len() as isize);
        if id.starts_with("claude-") {
            let map_start = if id == "claude-project-server" {
                source.find("\"mcpServers\"")
            } else {
                source.rfind("\"mcpServers\"")
            }
            .expect("Claude MCP map");
            assert!(span.start >= map_start, "{id} inserted outside its MCP map");
        }
    }
}

#[test]
fn edits_and_removals_touch_only_the_target_span() {
    let suite = Suite::workspace().expect("sandbox");

    let (source, actual) = suite.apply_case(case("omp-edit-env")).expect("apply");
    let span = single_edit_span(&source, &actual);
    assert!(span.removed > 0, "an env edit must replace the env value");
    assert!(span.inserted.contains("/tmp/tool-v2"));
    let entry = source.find("\"local-tool\"").expect("entry");
    let neighbour = source[entry..]
        .find("\"remote-tool\"")
        .map(|offset| offset + entry)
        .expect("neighbour");
    assert!(
        span.start >= entry && span.start + span.removed <= neighbour,
        "the env edit left the `local-tool` entry"
    );

    for id in ["codex-remove-server", "omp-remove-server"] {
        let (source, actual) = suite.apply_case(case(id)).expect("apply");
        let span = single_edit_span(&source, &actual);
        assert_eq!(span.inserted, "", "{id} inserted bytes while removing");
        assert!(span.removed > 0, "{id} removed nothing");
        assert_eq!(source.len() - actual.len(), span.removed);
    }
}

#[test]
fn crlf_fixtures_stay_crlf() {
    let suite = Suite::workspace().expect("sandbox");
    for id in ["codex-crlf-add-server", "omp-crlf-add-server"] {
        let (source, actual) = suite.apply_case(case(id)).expect("apply");
        assert_no_lone_lf(&source).expect("fixture is CRLF");
        assert_no_lone_lf(&actual).unwrap_or_else(|error| panic!("{id}: {error}"));
    }
}

#[test]
fn update_mode_regenerates_and_a_clean_run_passes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let fixtures = dir.path().join("fixtures");
    fs::create_dir_all(fixtures.join("case")).expect("fixtures");
    fs::write(fixtures.join("case/input.toml"), "[a]\nb = 1\n").expect("fixture");
    let golden = dir.path().join("golden");
    let suite = Suite::new(&fixtures, &golden).expect("sandbox");
    let one = GoldenCase {
        id: "update-case",
        fixture: "case/input.toml",
        format: Format::Toml,
        apply: append_comment,
        check: accept_anything,
        unchanged: false,
    };

    let outcome = suite
        .run_with(std::slice::from_ref(&one), true)
        .expect("update");
    assert_eq!(outcome.updated, vec!["update-case"]);
    let expected = golden.join("update-case/expected.toml");
    assert_eq!(
        fs::read_to_string(&expected).expect("expected file"),
        "[a]\nb = 1\n# edited\n"
    );

    suite
        .run_with(std::slice::from_ref(&one), false)
        .expect("a clean run after regeneration passes");

    fs::write(&expected, "[a]\nb = 2\n").expect("corrupt expected");
    let failures = suite
        .run_with(std::slice::from_ref(&one), false)
        .expect_err("a stale expected file must fail");
    let text = failures.to_string();
    assert!(text.contains("update-case"), "{text}");
    assert!(text.contains("input.toml"), "{text}");
    assert!(text.contains("--- expected"), "{text}");
    assert!(text.contains("+++ actual"), "{text}");
    assert!(text.contains('+') && text.contains('-'), "{text}");
}

#[test]
fn single_edit_span_reports_the_edit() {
    let span = single_edit_span("ab", "acb");
    assert_eq!(span.start, 1);
    assert_eq!(span.removed, 0);
    assert_eq!(span.inserted, "c");
    assert_eq!(span.length_delta(), 1);

    let span = single_edit_span("one two", "one 2");
    assert_eq!(span.start, 4);
    assert_eq!(span.removed, 3);
    assert_eq!(span.inserted, "2");
    assert_eq!(span.length_delta(), -2);

    let span = single_edit_span("same", "same");
    assert_eq!(
        (span.start, span.removed, span.inserted.as_str()),
        (4, 0, "")
    );
}

#[test]
fn five_megabyte_jsonc_document_round_trips_under_200_ms() {
    let mut text = String::with_capacity(5 * 1024 * 1024 + 4096);
    text.push_str(
        "{\n  \"$schema\": \"https://example.test/mcp-schema.json\",\n  \"mcpServers\": {\n",
    );
    let mut index = 0usize;
    let mut instructions =
        String::from("Use this synthetic server when the fixture needs a large document. ");
    while instructions.len() < 1200 {
        instructions.push_str("It has no real behaviour. ");
    }
    while text.len() < 5 * 1024 * 1024 {
        text.push_str(&format!(
            "    \"server-{index}\": {{ \"command\": \"cmd-{index}\", \"args\": [\"--flag\", \"value-{index}\"], \"env\": {{ \"TOKEN\": \"${{TOKEN-{index}}}\" }}, \"enabled\": true, \"timeout\": 1000, \"instructions\": \"{instructions}\" }},\n"
        ));
        index += 1;
    }
    text.push_str("    \"last\": { \"command\": \"last-mcp\" }\n  }\n}\n");
    assert!(text.len() >= 5 * 1024 * 1024);
    assert!(index > 0);

    let home = TestHome::new().expect("sandbox");
    let path = home
        .write("five-mb/mcp.json", text.as_bytes())
        .expect("write");
    assert_eq!(
        fs::metadata(&path).expect("metadata").len(),
        text.len() as u64
    );

    let server = OmpServer::new(
        "added",
        McpTransport::Stdio {
            command: "added-mcp".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    );
    let started = Instant::now();
    let config = OmpMcpConfig::parse(&text).expect("parse 5 MB");
    let added = config.upsert_server(&server).expect("add a server");
    let removed = config.remove_server("server-0").expect("remove a server");
    let elapsed = started.elapsed();

    assert!(added.contains("\"added\""));
    assert!(!removed.contains("\"server-0\""));
    // `cargo test` builds without optimisations; 200 ms is the release-profile budget for the
    // 5 MB document (verified with `cargo test --release`). The debug ceiling stays generous so
    // the check reports a real regression rather than the absence of codegen.
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(200)
    };
    assert!(
        elapsed < budget,
        "a 5 MB document took {elapsed:?}, budget {budget:?}"
    );
}

#[test]
fn claude_rebases_unrelated_external_edits_and_preserves_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("temp directory");
    let target = dir.path().join("target.json");
    let link = dir.path().join("claude.json");
    fs::write(&target, CLAUDE_GOLDEN).expect("fixture");
    symlink(&target, &link).expect("symlink");

    let config = ClaudeMcpConfig::parse(CLAUDE_GOLDEN, ClaudeConfigScope::User).expect("parse");
    let server = ClaudeServer {
        name: "mcp-new".to_owned(),
        transport: McpTransport::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    };
    let desired = config.upsert(&server).expect("patch");
    let request = config.replacement_request(&link, &desired, BackupPolicy::None);
    let changes = config
        .change_set(Path::new("claude.json"), &desired)
        .expect("change set");
    assert!(matches!(
        changes.ops.as_slice(),
        [at_model::FileOp::Write {
            follow_symlink: true,
            ..
        }]
    ));
    assert!(request.follow_symlink);

    let external = CLAUDE_GOLDEN.replace("\"theme\": \"dark\"", "\"theme\": \"light\"");
    fs::write(&target, &external).expect("external unrelated edit");
    let external_config =
        ClaudeMcpConfig::parse(&external, ClaudeConfigScope::User).expect("external parse");
    assert_eq!(external_config.servers(), config.servers());
    config
        .save_edit(&link, &desired, BackupPolicy::None)
        .expect("rebase");
    assert!(
        link.symlink_metadata()
            .expect("link metadata")
            .file_type()
            .is_symlink()
    );
    let result = fs::read_to_string(&target).expect("target");
    assert!(result.contains("\"theme\": \"light\""));
    assert!(result.contains("\"mcp-new\""));
}

#[test]
fn claude_refuses_to_rebase_when_server_subtree_changed() {
    let dir = tempfile::tempdir().expect("temp directory");
    let path = dir.path().join("claude.json");
    fs::write(&path, CLAUDE_GOLDEN).expect("fixture");
    let config = ClaudeMcpConfig::parse(CLAUDE_GOLDEN, ClaudeConfigScope::User).expect("parse");
    let desired = config
        .upsert(&ClaudeServer {
            name: "new-server".to_owned(),
            transport: McpTransport::Stdio {
                command: "node".to_owned(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        })
        .expect("patch");
    let external = CLAUDE_GOLDEN.replace("\"command\": \"old\"", "\"command\": \"external\"");
    fs::write(&path, &external).expect("external server edit");
    let error = config
        .save_edit(&path, &desired, BackupPolicy::None)
        .expect_err("server subtree conflict must be returned");
    assert!(matches!(
        error,
        at_adapters::claude_mcp::ClaudeMcpError::Fs(at_fs::FsError::ConcurrentModification { .. })
    ));
    assert_eq!(
        fs::read_to_string(path).expect("read external config"),
        external
    );
}

#[test]
fn claude_maps_only_referenced_variable_values_to_secrets() {
    let source = r#"{"mcpServers":{"server":{"command":"node","env":{"TOKEN":"${TOKEN}","MODE":"${MODE}"}}}}"#;
    let refs = BTreeMap::from([("TOKEN".to_owned(), "account-token".to_owned())]);
    let config = ClaudeMcpConfig::parse_with_secret_refs(source, ClaudeConfigScope::User, &refs)
        .expect("parse");
    let server = config.servers().get("server").expect("server");
    let McpTransport::Stdio { env, .. } = &server.transport else {
        panic!("expected stdio");
    };
    assert!(matches!(env.get("TOKEN"), Some(EnvValue::Secret(_))));
    assert_eq!(
        env.get("MODE"),
        Some(&EnvValue::Literal("${MODE}".to_owned()))
    );
}

#[test]
fn claude_span_patch_preserves_crlf() {
    let source = CLAUDE_GOLDEN.replace('\n', "\r\n");
    let config = ClaudeMcpConfig::parse(&source, ClaudeConfigScope::User).expect("parse");
    let output = config
        .upsert(&ClaudeServer {
            name: "a.b-测试".to_owned(),
            transport: McpTransport::Stdio {
                command: "node".to_owned(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        })
        .expect("patch");
    assert_no_lone_lf(&source).expect("CRLF fixture");
    assert_no_lone_lf(&output).expect("CRLF output");
    let span = single_edit_span(&source, &output);
    assert_eq!(span.removed, 0);
}
