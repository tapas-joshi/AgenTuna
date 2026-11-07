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
use at_model::mcp::{EnvValue, McpTransport};
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
        EnvValue::Literal("${GITHUB_PERSONAL_ACCESS_TOKEN}".to_owned()),
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

/// TA-091: discover adapter-owned format fixtures by convention and guard coverage.
///
/// Each registered adapter has one or more case directories under `fixtures/<agent>/<case>/`.
/// Every case contains `skills.input.*`/`skills.expected.*`, `rules.*`, and `settings.*`.
#[test]
fn adapter_owned_fixture_coverage_is_discovered_by_convention() {
    use at_adapters::adapter::AgentAdapterRegistry;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let registered = AgentAdapterRegistry::register()
        .adapters()
        .expect("enumerate registered adapters");
    let categories = ["skills", "rules", "settings"];
    for adapter in registered {
        let id = adapter.id();
        let agent = match id {
            at_model::AgentId::Custom(_) => String::from("custom"),
            other => other.to_string(),
        };
        let agent_root = root.join(&agent);
        let cases = fs::read_dir(&agent_root)
            .unwrap_or_else(|error| {
                panic!("missing fixtures for registered adapter {agent}: {error}")
            })
            .map(|entry| entry.expect("fixture directory entry").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        assert!(!cases.is_empty(), "adapter {agent} has no fixture cases");
        for case in cases {
            let mut categories_seen = Vec::new();
            for category in categories {
                let input = fixture_file(&case, category, "input");
                if !input.exists() {
                    continue;
                }
                let expected = fixture_file(&case, category, "expected");
                assert!(
                    expected.exists(),
                    "{} lacks expected file {}",
                    input.display(),
                    expected.display()
                );
                let input_bytes = fs::read(&input).expect("read adapter input fixture");
                let expected_bytes = fs::read(&expected).expect("read adapter expected fixture");
                validate_fixture_format(&input, &input_bytes);
                validate_fixture_format(&expected, &expected_bytes);
                if input_bytes != expected_bytes {
                    let expected_text = std::str::from_utf8(&expected_bytes).expect("validated");
                    let input_text = std::str::from_utf8(&input_bytes).expect("validated");
                    let diff = similar::TextDiff::from_lines(expected_text, input_text)
                        .unified_diff()
                        .context_radius(3)
                        .header("expected", "input")
                        .to_string();
                    panic!("no-op fixture mismatch for {}:\n{diff}", input.display());
                }
                categories_seen.push(category);
            }
            for category in categories {
                assert!(
                    categories_seen.contains(&category),
                    "{} fixture case lacks {category} input",
                    case.display()
                );
            }
        }
    }
}

fn fixture_file(case: &Path, category: &str, role: &str) -> std::path::PathBuf {
    let candidates = ["json", "toml", "md", "mdc"];
    candidates
        .iter()
        .map(|extension| case.join(format!("{category}.{role}.{extension}")))
        .find(|path| path.exists())
        .unwrap_or_else(|| case.join(format!("{category}.{role}.json")))
}

fn validate_fixture_format(path: &Path, bytes: &[u8]) {
    let text = std::str::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", path.display()));
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => {
            jsonc_parser::parse_to_value(text, &Default::default())
                .unwrap_or_else(|error| panic!("{} is invalid JSONC: {error}", path.display()))
                .expect("fixture must have a JSONC value");
        }
        Some("toml") => {
            text.parse::<toml_edit::DocumentMut>()
                .unwrap_or_else(|error| panic!("{} is invalid TOML: {error}", path.display()));
        }
        Some("md" | "mdc") => {}
        extension => panic!(
            "{} has unsupported fixture extension {extension:?}",
            path.display()
        ),
    }
}

#[test]
fn malformed_mcp_documents_leave_source_hashes_and_directory_unchanged() {
    let temp = tempfile::tempdir().expect("temporary MCP fixture directory");
    let documents = [
        (
            "claude.json",
            "{\n  \"mcpServers\": [\n",
            ClaudeMcpConfig::parse("{\n  \"mcpServers\": [\n", ClaudeConfigScope::User).is_err(),
        ),
        (
            "codex.toml",
            "[mcp_servers\n",
            CodexConfigFile::parse("[mcp_servers\n").is_err(),
        ),
        (
            "omp.json",
            "{\n  \"mcpServers\": {\n",
            OmpMcpConfig::parse("{\n  \"mcpServers\": {\n").is_err(),
        ),
    ];
    for (name, malformed, rejected) in documents {
        assert!(rejected, "malformed {name} input was accepted");
        let case_dir = temp.path().join(name);
        fs::create_dir(&case_dir).expect("create isolated fixture directory");
        let path = case_dir.join(name);
        fs::write(&path, malformed).expect("write malformed fixture");
        let before = at_fs::sha256_hex(malformed.as_bytes());
        assert_eq!(
            at_fs::sha256_hex(&fs::read(&path).expect("read input")),
            before,
            "hash changed for {name}"
        );
        let entries = fs::read_dir(&case_dir)
            .expect("list fixture directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "unexpected temp file left for {name}");
        assert_eq!(entries[0], name);
    }
}

#[cfg(unix)]
#[test]
fn codex_and_omp_mcp_edits_preserve_symlinked_agent_config_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("MCP symlink sandbox");

    let codex_target = temp.path().join("codex-target.toml");
    let codex_link = temp.path().join("config.toml");
    fs::write(&codex_target, CODEX_GOLDEN).expect("Codex source");
    symlink(&codex_target, &codex_link).expect("Codex symlink");
    let mut codex = CodexConfigFile::load(&codex_link).expect("load Codex config");
    codex
        .set_server(
            "new-local",
            &CodexServerEntry {
                command: Some("node".to_owned()),
                ..CodexServerEntry::default()
            },
        )
        .expect("edit Codex config");
    codex
        .save(&codex_link, BackupPolicy::None)
        .expect("write through Codex config link");
    assert!(
        fs::symlink_metadata(&codex_link)
            .expect("Codex link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&codex_target)
            .expect("Codex target")
            .contains("new-local")
    );

    let omp_target = temp.path().join("omp-target.json");
    let omp_link = temp.path().join("mcp.json");
    fs::write(&omp_target, OMP_GOLDEN).expect("omp source");
    symlink(&omp_target, &omp_link).expect("omp symlink");
    let omp = OmpMcpConfig::parse(OMP_GOLDEN).expect("parse omp config");
    let server = OmpServer::new(
        "new-local",
        McpTransport::Stdio {
            command: "node".to_owned(),
            args: vec!["server.js".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    );
    let edited = omp.upsert_server(&server).expect("edit omp config");
    OmpMcpAdapter::new()
        .save_edit(&omp_link, &omp, &edited, BackupPolicy::None)
        .expect("write through omp config link");
    assert!(
        fs::symlink_metadata(&omp_link)
            .expect("omp link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&omp_target)
            .expect("omp target")
            .contains("new-local")
    );
}

#[test]
fn claude_local_settings_round_trip_and_toggle_preserves_unrelated_jsonc() {
    use at_adapters::adapter::AgentEnvironment;
    use at_adapters::claude_adapter::ClaudeAdapter;

    let temp = tempfile::tempdir().expect("settings sandbox");
    let project = temp.path().join("project");
    let env = AgentEnvironment {
        home: temp.path().join("home"),
        project: Some(project),
        variables: BTreeMap::new(),
    };
    let source = concat!(
        "{\n",
        "  \"permissions\": { \"allow\": [\"Read\"] },\n",
        "  \"hooks\": { \"PreToolUse\": [] },\n",
        "  \"enabledMcpjsonServers\": [\"keep\", \"toggle\"],\n",
        "  \"disabledMcpjsonServers\": []\n",
        "}\n",
    );
    let settings = ClaudeAdapter
        .parse_settings(&env, source.to_owned())
        .expect("parse settings.local.json");
    assert_eq!(settings.source().as_bytes(), source.as_bytes());
    let edited = settings
        .set_server_enabled("toggle", false)
        .expect("disable MCP entry");
    jsonc_parser::parse_to_value(&edited, &Default::default())
        .expect("valid output")
        .expect("JSONC object");
    assert!(edited.contains("\"permissions\": { \"allow\": [\"Read\"] }"));
    assert!(edited.contains("\"hooks\": { \"PreToolUse\": [] }"));
    assert!(edited.contains("\"enabledMcpjsonServers\": [\"keep\"]"));
    assert!(edited.contains("\"disabledMcpjsonServers\": [\"toggle\"]"));
    let span = single_edit_span(source, &edited);
    let removed = &source[span.start..span.start + span.removed];
    assert!(span.removed > 0);
    assert!(!removed.contains("\"permissions\""));
    assert!(!removed.contains("\"hooks\""));
    assert_eq!(
        settings.source(),
        source,
        "parse/edit must not mutate source"
    );
}

#[cfg(unix)]
#[test]
fn claude_skill_listing_reports_but_does_not_follow_external_symlinks() {
    use std::os::unix::fs::symlink;

    use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
    use at_adapters::claude_adapter::ClaudeAdapter;

    let temp = tempfile::tempdir().expect("skill sandbox");
    let home = temp.path().join("home");
    let skills = home.join(".claude/skills");
    let external = temp.path().join("external-skill");
    fs::create_dir_all(&skills).expect("skills root");
    fs::create_dir_all(&external).expect("external skill");
    fs::write(
        external.join("SKILL.md"),
        "---\nname: external\ndescription: fixture\n---\n",
    )
    .expect("external skill document");
    let link = skills.join("outside");
    symlink(&external, &link).expect("external skill link");
    let env = AgentEnvironment {
        home,
        project: None,
        variables: BTreeMap::new(),
    };
    let listing = ClaudeAdapter
        .list_native_skills(&env, &[])
        .expect("list native skills");
    let row = listing
        .iter()
        .find(|row| row.path == link)
        .expect("symlink row");
    assert_eq!(row.target.as_deref(), Some(external.as_path()));
    assert!(
        !row.valid,
        "external content must not be read as a native skill"
    );
    assert!(
        row.error
            .as_deref()
            .is_some_and(|error| error.contains("outside injected roots"))
    );
}

#[test]
fn claude_settings_reject_truncation_and_bom_without_changing_fixture_bytes() {
    use at_adapters::adapter::{AgentAdapter, AgentEnvironment, AgentError};
    use at_adapters::claude_adapter::ClaudeAdapter;
    use at_model::ItemKind;

    let temp = tempfile::tempdir().expect("settings sandbox");
    let project = temp.path().join("project");
    fs::create_dir_all(&project).expect("project root");
    let env = AgentEnvironment {
        home: temp.path().join("home"),
        project: Some(project.clone()),
        variables: BTreeMap::new(),
    };
    let settings_path = project.join(".claude/settings.local.json");
    fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .expect("settings directory");
    let adapter = ClaudeAdapter;
    for (malformed, expected_line) in [
        ("{\n  \"enabledMcpjsonServers\": [", 2),
        ("\u{feff}{\"permissions\":{}}", 1),
    ] {
        fs::write(&settings_path, malformed).expect("write malformed settings fixture");
        let before = at_fs::sha256_hex(malformed.as_bytes());
        let error = adapter
            .parse_settings(
                &env,
                fs::read_to_string(&settings_path).expect("read settings"),
            )
            .expect_err("malformed settings must be rejected");
        assert!(matches!(
            error,
            AgentError::Parse { line, col, .. } if line == expected_line && col > 0
        ));
        assert_eq!(
            at_fs::sha256_hex(&fs::read(&settings_path).expect("unchanged settings")),
            before
        );
    }
    assert_eq!(
        adapter.paths(ItemKind::Rule, at_model::Scope::Project, &env)[0].path,
        project.join("CLAUDE.md")
    );
}

#[test]
fn codex_lists_primary_and_unmanaged_alternative_skill_roots() {
    use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
    use at_adapters::codex_adapter::CodexAdapter;

    let temp = tempfile::tempdir().expect("Codex skills sandbox");
    let home = temp.path().join("home");
    let primary = home.join(".codex/skills/primary");
    let alternative = home.join(".agents/skills/alternative");
    for (path, name) in [(&primary, "primary"), (&alternative, "alternative")] {
        fs::create_dir_all(path).expect("skill folder");
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: fixture\n---\n"),
        )
        .expect("skill document");
    }
    let env = AgentEnvironment {
        home,
        project: None,
        variables: BTreeMap::new(),
    };
    let rows = CodexAdapter
        .list_native_skills(&env, &[])
        .expect("list Codex skills");
    assert_eq!(rows.len(), 2);
    let primary_row = rows
        .iter()
        .find(|row| row.path == primary)
        .expect("primary");
    let alternative_row = rows
        .iter()
        .find(|row| row.path == alternative)
        .expect("alternative");
    assert!(!primary_row.unmanaged_alternative);
    assert!(alternative_row.unmanaged_alternative);
}

/// Records MCP adapter delegation calls and replays the native config bytes.
#[derive(Default)]
struct RecordingMcpDelegate {
    reads: std::sync::atomic::AtomicUsize,
    writes: std::sync::atomic::AtomicUsize,
    last_write: std::sync::Mutex<Vec<u8>>,
}

impl at_adapters::adapter::McpAdapterDelegate for RecordingMcpDelegate {
    fn read_io(
        &self,
        _scope: at_model::Scope,
        env: &at_adapters::adapter::AgentEnvironment,
    ) -> Result<at_adapters::adapter::AgentIoDocument, at_adapters::adapter::AgentError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = at_adapters::codex_mcp::config_path(&env.home);
        let content = fs::read(&path).unwrap_or_default();
        Ok(at_adapters::adapter::AgentIoDocument { path, content })
    }

    fn render_io(
        &self,
        deployment: &at_adapters::adapter::AgentDeployment,
    ) -> Result<Vec<u8>, at_adapters::adapter::AgentError> {
        Ok(deployment.content.clone())
    }

    fn write_io(
        &self,
        deployment: &at_adapters::adapter::AgentDeployment,
    ) -> Result<at_adapters::adapter::AgentIoDocument, at_adapters::adapter::AgentError> {
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut last) = self.last_write.lock() {
            *last = deployment.content.clone();
        }
        Ok(at_adapters::adapter::AgentIoDocument {
            path: deployment.path.clone(),
            content: deployment.content.clone(),
        })
    }
}

#[test]
fn codex_agents_md_create_writes_one_trailing_newline_and_preserves_outside_bytes() {
    use at_adapters::adapter::{AgentAdapter, AgentDeployment};
    use at_adapters::codex_adapter::CodexAdapter;
    use at_model::{ItemKind, Scope};

    let temp = tempfile::tempdir().expect("rule sandbox");
    let project = temp.path().join("project");
    fs::create_dir_all(&project).expect("project directory");
    let path = project.join("AGENTS.md");
    let deployment = |content: &[u8]| AgentDeployment {
        kind: ItemKind::Rule,
        scope: Scope::Project,
        path: path.clone(),
        content: content.to_vec(),
    };

    let created = CodexAdapter
        .render_io(&deployment(b"# Shared rules\nBody line"))
        .expect("render a new AGENTS.md");
    assert_eq!(
        created,
        b"<!-- agentuna:begin -->\n# Shared rules\nBody line\n<!-- agentuna:end -->\n".to_vec()
    );
    assert!(
        created.ends_with(b"\n") && !created.ends_with(b"\n\n"),
        "creation writes exactly one trailing newline"
    );

    let existing = [
        b"User header kept verbatim.\n".as_slice(),
        created.as_slice(),
        b"User footer kept verbatim.\n".as_slice(),
    ]
    .concat();
    fs::write(&path, &existing).expect("seed existing AGENTS.md");
    let edited = CodexAdapter
        .render_io(&deployment(b"# New rules\n"))
        .expect("render an existing AGENTS.md");
    assert_eq!(
        edited,
        [
            b"User header kept verbatim.\n".as_slice(),
            b"<!-- agentuna:begin -->\n# New rules\n<!-- agentuna:end -->\n".as_slice(),
            b"User footer kept verbatim.\n".as_slice(),
        ]
        .concat(),
        "only the managed block body changes"
    );
    assert_eq!(
        fs::read(&path).expect("target stays untouched"),
        existing,
        "render_io never writes the file itself"
    );
}

#[test]
fn codex_config_with_ten_projects_and_five_servers_delegates_and_survives_a_skill_deploy() {
    use at_adapters::adapter::{AgentAdapter, AgentAdapterRegistry, AgentEnvironment};
    use at_adapters::codex_adapter::CodexAdapter;
    use at_model::{AgentId, ItemKind, LinkMode, Scope};
    use std::path::PathBuf;

    let temp = tempfile::tempdir().expect("codex config sandbox");
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex config root");
    let config_path = home.join(".codex/config.toml");
    let mut source = String::from("model = \"gpt-5\"\n\n");
    for index in 0..10 {
        source.push_str(&format!(
            "[projects.\"/work/project-{index:02}\"]\ntrust_level = \"trusted\"\n\n"
        ));
    }
    for index in 0..5 {
        source.push_str(&format!(
            "[mcp_servers.server-{index}]\ncommand = \"server-{index}\"\nargs = [\"--serve\"]\n\n"
        ));
    }
    fs::write(&config_path, &source).expect("seed config");
    let before = at_fs::sha256_hex(&fs::read(&config_path).expect("config bytes"));

    let projects = CodexAdapter
        .list_project_trust(&config_path, &source)
        .expect("read project trust");
    assert_eq!(projects.len(), 10, "every [projects.*] table is returned");
    assert_eq!(projects[0].project_path, PathBuf::from("/work/project-00"));
    assert_eq!(projects[9].project_path, PathBuf::from("/work/project-09"));

    let registry = AgentAdapterRegistry::register();
    let delegate = std::sync::Arc::new(RecordingMcpDelegate::default());
    registry
        .register_mcp_delegate(AgentId::Codex, delegate.clone())
        .expect("register TM-006 mock");
    let env = AgentEnvironment {
        home: home.clone(),
        project: None,
        variables: BTreeMap::new(),
    };
    let document = registry
        .read_io(&AgentId::Codex, ItemKind::McpServer, Scope::User, &env)
        .expect("delegated MCP read");
    assert_eq!(
        delegate.reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the server list comes from one delegation call"
    );
    let delegated = String::from_utf8(document.content).expect("UTF-8 config");
    let parsed = CodexConfigFile::parse(&delegated).expect("parse delegated document");
    assert_eq!(
        parsed.server_ids().expect("server ids"),
        vec![
            "server-0".to_owned(),
            "server-1".to_owned(),
            "server-2".to_owned(),
            "server-3".to_owned(),
            "server-4".to_owned(),
        ]
    );

    // A skills-only operation in the same agent folder must not touch the MCP document.
    let store_skill = temp.path().join("store/skills/alpha");
    fs::create_dir_all(&store_skill).expect("store skill");
    fs::write(
        store_skill.join("SKILL.md"),
        b"---\nname: alpha\ndescription: fixture\n---\n",
    )
    .expect("skill document");
    let target = home.join(".codex/skills/alpha");
    fs::create_dir_all(target.parent().expect("skills root")).expect("skills root");
    let outcome = CodexAdapter
        .deploy_skill(
            &store_skill,
            &target,
            at_fs::SkillStrategy {
                mode: LinkMode::Link,
                copy_fallback: false,
            },
        )
        .expect("skills-only deploy");
    assert_eq!(outcome.mode, LinkMode::Link);
    assert_eq!(
        at_fs::sha256_hex(&fs::read(&config_path).expect("config bytes")),
        before,
        "a skills-only operation leaves the MCP document hash unchanged"
    );
    assert_eq!(
        CodexAdapter
            .list_project_trust(&config_path, &source)
            .expect("projects still parse")
            .len(),
        10
    );
}

#[test]
fn codex_invalid_utf8_agents_md_is_a_parse_error_and_is_not_rewritten() {
    use at_adapters::adapter::{AgentAdapter, AgentDeployment, AgentError};
    use at_adapters::codex_adapter::CodexAdapter;
    use at_model::{ItemKind, Scope};

    let temp = tempfile::tempdir().expect("invalid UTF-8 rule sandbox");
    let project = temp.path().join("project");
    fs::create_dir_all(&project).expect("project directory");
    let path = project.join("AGENTS.md");
    let invalid = b"# rules\n\xff\xfe not utf-8\n";
    fs::write(&path, invalid).expect("seed malformed rules");

    let error = CodexAdapter
        .render_io(&AgentDeployment {
            kind: ItemKind::Rule,
            scope: Scope::Project,
            path: path.clone(),
            content: b"replacement".to_vec(),
        })
        .expect_err("invalid UTF-8 must be refused, not lossily decoded");
    assert!(matches!(
        error,
        AgentError::Parse { path: parsed, line: 2, col: 1 } if parsed == path
    ));
    assert_eq!(
        fs::read(&path).expect("malformed file remains"),
        invalid,
        "the malformed file is never rewritten"
    );
    assert_eq!(
        fs::read_dir(&project).expect("project listing").count(),
        1,
        "no temp sibling or backup is created"
    );
}

#[test]
fn codex_mcp_disable_delegates_once_with_enabled_false_and_writes_no_toml() {
    use at_adapters::adapter::{AgentAdapterRegistry, AgentDeployment};
    use at_model::{AgentId, ItemKind, Scope};

    let temp = tempfile::tempdir().expect("MCP toggle sandbox");
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex config root");
    let config_path = home.join(".codex/config.toml");
    let source = "[mcp_servers.alpha]\ncommand = \"alpha\"\nargs = [\"serve\"]\n";
    fs::write(&config_path, source).expect("seed config");
    let before = at_fs::sha256_hex(&fs::read(&config_path).expect("config bytes"));

    let registry = AgentAdapterRegistry::register();
    let delegate = std::sync::Arc::new(RecordingMcpDelegate::default());
    registry
        .register_mcp_delegate(AgentId::Codex, delegate.clone())
        .expect("register TM-006 mock");

    let mut config = CodexConfigFile::parse(source).expect("parse fixture");
    let mut entry = config
        .server("alpha")
        .expect("read server")
        .expect("server exists")
        .entry;
    entry.enabled = Some(false);
    config.set_server("alpha", &entry).expect("disable server");
    let toggled = config.text();
    assert!(toggled.contains("enabled = false"));

    let document = registry
        .write_io(
            &AgentId::Codex,
            &AgentDeployment {
                kind: ItemKind::McpServer,
                scope: Scope::User,
                path: config_path.clone(),
                content: toggled.into_bytes(),
            },
        )
        .expect("delegate the disable operation");
    assert_eq!(document.path, config_path);
    assert_eq!(
        delegate.writes.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "enable/disable is a single delegation call"
    );
    let recorded = delegate.last_write.lock().expect("delegate record").clone();
    let recorded =
        CodexConfigFile::parse(std::str::from_utf8(&recorded).expect("recorded document"))
            .expect("parse recorded document");
    assert_eq!(
        recorded
            .server("alpha")
            .expect("server")
            .expect("present")
            .entry
            .enabled,
        Some(false)
    );
    assert_eq!(
        at_fs::sha256_hex(&fs::read(&config_path).expect("config bytes")),
        before,
        "the adapter delegation performs no local TOML write"
    );
}

#[test]
fn mcp_format_parsers_preserve_tab_indentation_and_unrelated_text() {
    let claude_source = concat!(
        "{\n",
        "\t\"mcpServers\": {},\n",
        "\t\"unrelated\": {\n",
        "\t\t\"keep\": true\n",
        "\t}\n",
        "}\n",
    );
    let claude = ClaudeMcpConfig::parse(claude_source, ClaudeConfigScope::User)
        .expect("tab-indented Claude JSONC");
    let claude_after = claude
        .upsert(&ClaudeServer {
            name: "tabbed".to_owned(),
            transport: McpTransport::Stdio {
                command: "node".to_owned(),
                args: vec!["tool.js".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        })
        .expect("edit Claude JSONC");
    assert!(claude_after.contains("\t\"unrelated\": {\n\t\t\"keep\": true"));

    let codex_source = "[mcp_servers.tabbed]\n\tcommand = \"node\"\n\targs = [\"tool.js\"]\n";
    let codex = CodexConfigFile::parse(codex_source).expect("tab-indented Codex TOML");
    assert_eq!(codex.text(), codex_source);

    let omp_source = "{\n\t\"$schema\": \"https://example.invalid/schema.json\",\n\t\"mcpServers\": {},\n\t\"keep\": true\n}\n";
    let omp = OmpMcpConfig::parse(omp_source).expect("tab-indented omp JSONC");
    assert_eq!(
        omp.render_unchanged().expect("render omp JSONC"),
        omp_source
    );
}

#[cfg(unix)]
#[test]
fn omp_mcp_unwritable_config_directory_preserves_original_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("MCP permission sandbox");
    let config_path = temp.path().join("mcp.json");
    fs::write(&config_path, OMP_GOLDEN).expect("write source fixture");
    let config = OmpMcpConfig::parse(OMP_GOLDEN).expect("parse source fixture");
    let server = OmpServer::new(
        "permission-test",
        McpTransport::Stdio {
            command: "node".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    );
    let edited = config.upsert_server(&server).expect("build edit");
    let mut permissions = fs::metadata(temp.path())
        .expect("directory permissions")
        .permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(temp.path(), permissions).expect("make fixture directory read-only");
    let result = OmpMcpAdapter::new().save_edit(&config_path, &config, &edited, BackupPolicy::None);
    let mut permissions = fs::metadata(temp.path())
        .expect("read-only directory permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(temp.path(), permissions).expect("restore fixture directory");
    assert!(matches!(
        result,
        Err(at_adapters::omp_mcp::OmpMcpError::Fs(
            at_fs::FsError::PermissionDenied { .. }
        ))
    ));
    assert_eq!(
        fs::read(&config_path).expect("original config"),
        OMP_GOLDEN.as_bytes()
    );
    assert_eq!(
        fs::read_dir(temp.path())
            .expect("list fixture directory")
            .count(),
        1,
        "failed writes leave no temp siblings"
    );
}

#[cfg(unix)]
#[test]
fn read_only_rule_target_is_refused_without_changing_bytes() {
    use std::os::unix::fs::PermissionsExt;

    use at_adapters::adapter::{AgentAdapter, AgentDeployment, AgentError};
    use at_adapters::claude_adapter::ClaudeAdapter;
    use at_model::{ItemKind, Scope};

    let temp = tempfile::tempdir().expect("read-only rule sandbox");
    let root = temp.path().canonicalize().expect("canonical sandbox root");
    let path = root.join("CLAUDE.md");
    let original = b"# Existing native rules\n";
    fs::write(&path, original).expect("seed rule target");
    let before_hash = at_fs::sha256_hex(original);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("make rule read-only");

    let error = ClaudeAdapter
        .render_io(&AgentDeployment {
            kind: ItemKind::Rule,
            scope: Scope::Project,
            path: path.clone(),
            content: b"New managed instructions".to_vec(),
        })
        .expect_err("read-only rules must be refused before an apply plan is produced");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("restore fixture permissions");
    assert!(matches!(
        error,
        AgentError::Permission { path: denied } if denied == path
    ));
    assert_eq!(
        at_fs::sha256_hex(&fs::read(&path).expect("original rule remains")),
        before_hash,
        "the read-only target remains byte-identical"
    );
}

#[test]
fn invalid_utf8_markdown_is_reported_as_invalid_and_other_skills_still_load() {
    use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
    use at_adapters::claude_adapter::ClaudeAdapter;
    use at_adapters::native_skills::NativeSkillState;

    let temp = tempfile::tempdir().expect("UTF-8 skill sandbox");
    let root = temp.path().canonicalize().expect("canonical sandbox root");
    let home = root.join("home");
    let skills = home.join(".claude/skills");
    fs::create_dir_all(skills.join("invalid")).expect("invalid skill folder");
    fs::create_dir_all(skills.join("valid")).expect("valid skill folder");
    fs::write(skills.join("invalid/SKILL.md"), [0xff]).expect("write invalid UTF-8 fixture");
    fs::write(
        skills.join("valid/SKILL.md"),
        b"---\nname: valid\ndescription: valid fixture\n---\n",
    )
    .expect("write valid skill fixture");
    let env = AgentEnvironment {
        home,
        project: None,
        variables: BTreeMap::new(),
    };

    let rows = ClaudeAdapter
        .list_native_skills(&env, &[])
        .expect("invalid markdown is an item diagnostic, not a failed scan");
    let invalid = rows
        .iter()
        .find(|row| row.name == "invalid")
        .expect("invalid skill remains visible");
    assert_eq!(invalid.state, NativeSkillState::Invalid);
    assert_eq!(
        invalid.error.as_deref(),
        Some("SKILL.md is not valid UTF-8"),
        "invalid bytes are rejected rather than lossily decoded"
    );
    assert!(
        rows.iter()
            .any(|row| row.name == "valid" && row.state == NativeSkillState::Valid),
        "the scan continues to valid skills"
    );
}

#[cfg(unix)]
#[test]
fn skill_symlink_loop_is_a_specific_diagnostic_and_scan_continues() {
    use std::os::unix::fs::symlink;

    use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
    use at_adapters::claude_adapter::ClaudeAdapter;
    use at_adapters::native_skills::NativeSkillState;

    let temp = tempfile::tempdir().expect("skill symlink sandbox");
    let root = temp.path().canonicalize().expect("canonical sandbox root");
    let home = root.join("home");
    let skills = home.join(".claude/skills");
    fs::create_dir_all(skills.join("valid")).expect("valid skill folder");
    fs::write(
        skills.join("valid/SKILL.md"),
        b"---\nname: valid\ndescription: valid fixture\n---\n",
    )
    .expect("write valid skill");
    let loop_link = skills.join("loop");
    symlink(&loop_link, &loop_link).expect("symlink loop");
    let env = AgentEnvironment {
        home,
        project: None,
        variables: BTreeMap::new(),
    };
    let rows = ClaudeAdapter
        .list_native_skills(&env, &[])
        .expect("a cyclic link is diagnosed without aborting the scan");

    let loop_row = rows
        .iter()
        .find(|row| row.name == "loop")
        .expect("loop diagnostic");
    assert_eq!(loop_row.state, NativeSkillState::Broken);
    assert_eq!(loop_row.error.as_deref(), Some("symbolic link loop"));
    assert_eq!(loop_row.target.as_deref(), Some(loop_link.as_path()));
    assert!(
        rows.iter()
            .any(|row| row.name == "valid" && row.state == NativeSkillState::Valid),
        "the scan continues to valid skills"
    );
    assert!(
        fs::symlink_metadata(&loop_link)
            .expect("loop metadata")
            .file_type()
            .is_symlink()
    );
}
