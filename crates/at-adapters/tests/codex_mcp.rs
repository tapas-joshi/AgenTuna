//! Acceptance tests for the Codex MCP adapter (TM-006).
//!
//! Fixtures are synthetic. Each golden test compares whole documents byte for byte, so any
//! change to a comment, a blank line or an unrelated table fails the test.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use at_adapters::codex_mcp::{
    CODEX_HOME, CONFIG_FILE, CodexConfigFile, CodexError, CodexFlag, CodexSave, CodexServerEntry,
    CodexUnmodelled, config_path,
};
use at_fs::{BackupPolicy, ExpectedState, FsError, sha256_hex};
use at_model::mcp::{EnvValue, McpTransport, SecretRef};
use tempfile::TempDir;
use url::Url;

const GOLDEN: &str = include_str!("fixtures/codex/golden.toml");
const GOLDEN_ADDED: &str = include_str!("fixtures/codex/golden.add.expected.toml");
const GOLDEN_DELETED: &str = include_str!("fixtures/codex/golden.deleted.expected.toml");
const GOLDEN_COMMAND: &str = include_str!("fixtures/codex/golden.command.expected.toml");
const DOTTED: &str = include_str!("fixtures/codex/dotted.toml");
const NESTED: &str = include_str!("fixtures/codex/nested.toml");
const INTERPOLATION: &str = include_str!("fixtures/codex/interpolation.toml");

/// The golden config with every mcp_servers table removed.
const NO_SERVERS: &str = concat!(
    "# Synthetic Codex config used by the TM-006 golden tests.\n",
    "# Every value is invented; this is not a real user configuration.\n",
    "\n",
    "model = \"gpt-5-codex\"\n",
    "approval_policy = \"on-request\"\n",
    "\n",
    "# Trust settings for this checkout.\n",
    "[projects.\"<project>\"]\n",
    "trust_level = \"trusted\"\n",
    "\n",
    "[features]\n",
    "web_search = true\n",
    "view_image = true\n",
    "\n",
    "# Plugin registries are managed outside this file.\n",
    "[marketplaces.local]\n",
    "path = \"<project>/marketplace\"\n",
);

/// A temporary directory holding one config file, written from `source`.
fn sandbox(source: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write fixture");
    (dir, path)
}

fn server_with_command(command: &str) -> CodexServerEntry {
    CodexServerEntry {
        command: Some(command.to_owned()),
        ..CodexServerEntry::default()
    }
}

fn assert_same_document(actual: &str, expected: &str) {
    if actual != expected {
        let first = actual
            .lines()
            .zip(expected.lines())
            .position(|(left, right)| left != right);
        panic!(
            "documents differ at line {:?}\n--- actual ---\n{actual}--- expected ---\n{expected}",
            first.map(|index| index + 1)
        );
    }
}

#[test]
fn resolved_config_path_is_the_native_codex_location() {
    let path = config_path(Path::new("/tmp/example-home"));
    assert_eq!(
        path,
        Path::new("/tmp/example-home")
            .join(CODEX_HOME)
            .join(CONFIG_FILE)
    );
}

// Acceptance 1: a golden fixture with comments, blank lines and unrelated tables round-trips
// byte-identical when no edit is requested.
#[test]
fn golden_config_round_trips_byte_identically() {
    let (_dir, path) = sandbox(GOLDEN);
    let mut config = CodexConfigFile::load(&path).expect("load golden");
    assert_eq!(config.text(), GOLDEN);
    assert!(!config.is_dirty());

    let saved = config.save(&path, BackupPolicy::None).expect("save");
    assert_eq!(saved, CodexSave::Unchanged);
    assert_eq!(fs::read_to_string(&path).expect("read back"), GOLDEN);
    assert_eq!(
        config.expected(),
        &ExpectedState::Hash(sha256_hex(GOLDEN.as_bytes()))
    );

    // Parsing the same bytes from memory is equally lossless.
    assert_eq!(
        CodexConfigFile::parse(GOLDEN).expect("parse").text(),
        GOLDEN
    );
}

// Acceptance 2: adding a server inserts a comment-free table after the final mcp_servers
// table and the diff touches no other line.
#[test]
fn adding_a_server_appends_after_the_last_mcp_servers_table() {
    let mut config = CodexConfigFile::parse(GOLDEN).expect("parse");
    let entry = CodexServerEntry {
        command: Some("npx".to_owned()),
        args: Some(vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-github".to_owned(),
        ]),
        env: Some(BTreeMap::from([(
            "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
            EnvValue::Literal("${GITHUB_PERSONAL_ACCESS_TOKEN}".to_owned()),
        )])),
        ..CodexServerEntry::default()
    };
    let flags = config.set_server("github", &entry).expect("set server");
    assert_eq!(
        flags,
        [CodexFlag::UnverifiedInterpolation {
            server: "github".to_owned(),
            env_name: "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
        }]
    );

    let text = config.text();
    assert_same_document(&text, GOLDEN_ADDED);

    // Only insertions: every original line still appears, in order.
    let mut remaining = text.lines();
    for line in GOLDEN.lines() {
        assert!(
            remaining.any(|candidate| candidate == line),
            "line `{line}` disappeared or moved"
        );
    }

    // The new table sits inside the mcp_servers group, before the marketplaces table.
    let added = text.find("[mcp_servers.github]").expect("new header");
    let last_server = text
        .find("[mcp_servers.computer-use]")
        .expect("last server header");
    let other_table = text.find("[marketplaces.local]").expect("marketplaces");
    assert!(last_server < added && added < other_table);
    assert!(!text.contains("# Launched on demand\n[mcp_servers.github]"));
}

// Acceptance 3: a dotted id is a quoted key, not a nested table, and a quote round-trips.
#[test]
fn dotted_and_quoted_server_ids_round_trip() {
    let mut config = CodexConfigFile::parse(DOTTED).expect("parse");
    config
        .set_server("a.b", &server_with_command("two"))
        .expect("dotted id");
    let text = config.text();
    assert!(
        text.contains("[mcp_servers.\"a.b\"]\ncommand = \"two\"\n"),
        "expected a quoted header, got:\n{text}"
    );

    let reparsed = CodexConfigFile::parse(&text).expect("re-parse");
    assert_eq!(reparsed.server_ids().expect("ids"), ["plain", "a.b"]);
    let server = reparsed.server("a.b").expect("server").expect("present");
    assert_eq!(server.entry.command.as_deref(), Some("two"));

    // One quote type alone needs no escape: TOML has a literal-key form for it.
    let quoted = "weird \"quoted\" id";
    config
        .set_server(quoted, &server_with_command("three"))
        .expect("quoted id");
    let text = config.text();
    let quoted_header = text
        .lines()
        .find(|line| line.contains("weird"))
        .expect("header for the quoted id");
    assert!(
        quoted_header.starts_with("[mcp_servers.'") || quoted_header.starts_with("[mcp_servers.\""),
        "server id was not quoted: {quoted_header}"
    );

    // Both quote characters force an escaped basic-string key.
    let mixed = "weird \"double\" and 'single'";
    config
        .set_server(mixed, &server_with_command("four"))
        .expect("mixed id");
    let text = config.text();
    let mixed_header = text
        .lines()
        .find(|line| line.contains("double"))
        .expect("header for the mixed id");
    assert!(
        mixed_header.contains("\\\""),
        "expected an escaped quote in {mixed_header}"
    );
    let reparsed = CodexConfigFile::parse(&text).expect("re-parse");
    assert_eq!(
        reparsed.server_ids().expect("ids"),
        ["plain", "a.b", quoted, mixed]
    );
}

// A nested header and a quoted key name the same TOML path; the adapter treats them as one
// server and never writes a duplicate.
#[test]
fn nested_table_header_is_read_and_updated_in_place() {
    let config = CodexConfigFile::parse(NESTED).expect("parse");
    assert_eq!(config.server_ids().expect("ids"), ["org.team"]);

    let mut config = config;
    config
        .set_server("org.team", &server_with_command("changed"))
        .expect("update nested");
    let text = config.text();
    assert_eq!(
        text.matches("command =").count(),
        1,
        "duplicate keys:\n{text}"
    );
    assert!(
        !text.contains("\"org.team\""),
        "nested table was re-keyed:\n{text}"
    );
    let reparsed = CodexConfigFile::parse(&text).expect("re-parse");
    let server = reparsed
        .server("org.team")
        .expect("server")
        .expect("present");
    assert_eq!(server.entry.command.as_deref(), Some("changed"));
}

// Acceptance 4: deleting a server removes exactly its table and its trailing blank line,
// leaving the next header intact.
#[test]
fn deleting_a_server_removes_only_its_table() {
    let mut config = CodexConfigFile::parse(GOLDEN).expect("parse");
    assert!(config.remove_server("node_repl").expect("remove"));
    let text = config.text();
    assert_same_document(&text, GOLDEN_DELETED);
    assert!(text.contains("[mcp_servers.shuttle]\nurl = \"https://mcp.example.com/mcp\"\n"));
    assert!(!text.contains("NODE_REPL_MODE"));
    assert!(!text.contains("node_repl"));

    // Removing something that is not a server table changes nothing.
    let mut config = CodexConfigFile::parse(GOLDEN).expect("parse");
    assert!(!config.remove_server("absent").expect("remove absent"));
    assert!(!config.is_dirty());
    assert_eq!(config.text(), GOLDEN);

    // Removing the last table of the group leaves the unrelated tables alone.
    let mut config = CodexConfigFile::parse(GOLDEN).expect("parse");
    for id in ["playwright", "node_repl", "shuttle", "computer-use"] {
        assert!(config.remove_server(id).expect("remove"));
    }
    let text = config.text();
    assert!(!text.contains("mcp_servers"));
    assert!(text.contains("[marketplaces.local]\npath = \"<project>/marketplace\"\n"));
    assert_same_document(&text, NO_SERVERS);
}

// Acceptance 5: env_vars and enabled values written in an existing file are preserved when
// only command changes.
#[test]
fn changing_only_the_command_keeps_env_vars_and_enabled() {
    let mut config = CodexConfigFile::parse(GOLDEN).expect("parse");
    let flags = config
        .set_server("node_repl", &server_with_command("uvx"))
        .expect("patch command");
    assert!(flags.is_empty());
    let text = config.text();
    assert_same_document(&text, GOLDEN_COMMAND);
    assert!(text.contains("env_vars = [\"NODE_REPL_TOKEN\"]\n"));
    assert!(text.contains("enabled = true\n"));
    assert!(text.contains("startup_timeout_sec = 20\n"));
}

#[test]
fn secrets_are_written_as_references_and_flagged() {
    let secret = EnvValue::Secret(SecretRef {
        env_name: "FIGMA_TOKEN".to_owned(),
        keychain_account: "agentuna/figma".to_owned(),
    });
    let entry = CodexServerEntry {
        command: Some("npx".to_owned()),
        env: Some(BTreeMap::from([("FIGMA_TOKEN".to_owned(), secret)])),
        ..CodexServerEntry::default()
    };
    let mut config = CodexConfigFile::parse(DOTTED).expect("parse");
    let flags = config.set_server("figma", &entry).expect("set server");
    assert_eq!(
        flags,
        [CodexFlag::UnverifiedInterpolation {
            server: "figma".to_owned(),
            env_name: "FIGMA_TOKEN".to_owned(),
        }]
    );
    let text = config.text();
    assert!(
        text.contains("env = { FIGMA_TOKEN = \"${FIGMA_TOKEN}\" }"),
        "{text}"
    );
    assert!(
        !text.contains("agentuna/figma"),
        "keychain account leaked:\n{text}"
    );
}

#[test]
fn reading_flags_interpolated_env_values() {
    let config = CodexConfigFile::parse(INTERPOLATION).expect("parse");
    let servers = config.servers().expect("servers");
    assert_eq!(servers.len(), 1);
    let server = &servers[0];
    assert_eq!(server.id, "github");
    assert_eq!(
        server.flags,
        [CodexFlag::UnverifiedInterpolation {
            server: "github".to_owned(),
            env_name: "GITHUB_TOKEN".to_owned(),
        }]
    );
    assert_eq!(
        server
            .entry
            .env
            .as_ref()
            .and_then(|env| env.get("GITHUB_TOKEN")),
        Some(&EnvValue::Literal("${GITHUB_TOKEN}".to_owned()))
    );
    assert_eq!(
        server.entry.env.as_ref().and_then(|env| env.get("STATIC")),
        Some(&EnvValue::Literal("1".to_owned()))
    );
}

#[test]
fn env_written_as_a_sub_table_is_read() {
    let source =
        "[mcp_servers.sub]\ncommand = \"x\"\n[mcp_servers.sub.env]\nTOKEN = \"${TOKEN}\"\n";
    let config = CodexConfigFile::parse(source).expect("parse");
    let server = config.server("sub").expect("server").expect("present");
    assert_eq!(
        server.entry.env,
        Some(BTreeMap::from([(
            "TOKEN".to_owned(),
            EnvValue::Literal("${TOKEN}".to_owned())
        )]))
    );
    assert_eq!(
        server.flags,
        [CodexFlag::UnverifiedInterpolation {
            server: "sub".to_owned(),
            env_name: "TOKEN".to_owned(),
        }]
    );
}

// The five fields the story lists are read and written, including a URL-only server.
#[test]
fn every_modelled_field_round_trips() {
    let config = CodexConfigFile::parse(GOLDEN).expect("parse");
    let node_repl = config
        .server("node_repl")
        .expect("server")
        .expect("present");
    assert_eq!(node_repl.entry.command.as_deref(), Some("npx"));
    assert_eq!(
        node_repl.entry.args,
        Some(vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-everything".to_owned()
        ])
    );
    assert_eq!(
        node_repl.entry.env_vars,
        Some(vec!["NODE_REPL_TOKEN".to_owned()])
    );
    assert_eq!(node_repl.entry.enabled, Some(true));
    assert_eq!(node_repl.entry.startup_timeout_sec, Some(20));

    let shuttle = config.server("shuttle").expect("server").expect("present");
    assert_eq!(
        shuttle.entry.url.as_deref(),
        Some("https://mcp.example.com/mcp")
    );
    assert_eq!(shuttle.entry.command, None);

    let computer_use = config
        .server("computer-use")
        .expect("server")
        .expect("present");
    assert_eq!(
        computer_use.entry.cwd,
        Some(PathBuf::from("<project>/tools"))
    );
    assert_eq!(computer_use.entry.enabled, Some(false));
}

#[test]
fn http_transport_is_projected_to_url_and_headers_are_reported() {
    let url = Url::parse("https://mcp.example.com/mcp").expect("url");
    let transport = McpTransport::Http {
        url,
        headers: BTreeMap::from([(
            "Authorization".to_owned(),
            EnvValue::Literal("Bearer x".to_owned()),
        )]),
        oauth: None,
    };
    let (entry, unmodelled) = CodexServerEntry::from_transport(&transport);
    assert_eq!(entry.url.as_deref(), Some("https://mcp.example.com/mcp"));
    assert_eq!(entry.command, None);
    assert_eq!(unmodelled, [CodexUnmodelled::HttpHeaders { count: 1 }]);

    let mut config = CodexConfigFile::parse(DOTTED).expect("parse");
    config.set_server("http", &entry).expect("set server");
    assert!(!config.text().contains("headers"));
    assert!(!config.text().contains("Authorization"));
}

#[test]
fn stdio_transport_is_projected_to_command_args_and_env() {
    let transport = McpTransport::Stdio {
        command: "uvx".to_owned(),
        args: vec!["mcp-server-fetch".to_owned()],
        cwd: Some(PathBuf::from("<project>")),
        env: BTreeMap::from([(
            "TOKEN".to_owned(),
            EnvValue::Secret(SecretRef {
                env_name: "TOKEN".to_owned(),
                keychain_account: "agentuna/token".to_owned(),
            }),
        )]),
    };
    let (entry, unmodelled) = CodexServerEntry::from_transport(&transport);
    assert!(unmodelled.is_empty());
    assert_eq!(entry.command.as_deref(), Some("uvx"));
    assert_eq!(entry.args, Some(vec!["mcp-server-fetch".to_owned()]));

    let mut config = CodexConfigFile::parse(DOTTED).expect("parse");
    let flags = config.set_server("fetch", &entry).expect("set server");
    assert!(config.text().contains("env = { TOKEN = \"${TOKEN}\" }"));
    assert_eq!(
        flags,
        [CodexFlag::UnverifiedInterpolation {
            server: "fetch".to_owned(),
            env_name: "TOKEN".to_owned(),
        }]
    );
}

#[test]
fn invalid_patches_are_refused() {
    let mut config = CodexConfigFile::parse(DOTTED).expect("parse");
    assert!(matches!(
        config.set_server("", &server_with_command("x")),
        Err(CodexError::EmptyServerId)
    ));
    assert!(matches!(
        config.set_server("bad\nid", &server_with_command("x")),
        Err(CodexError::ControlCharacterInId(_))
    ));
    assert!(matches!(
        config.set_server("empty", &CodexServerEntry::default()),
        Err(CodexError::MissingTransport { .. })
    ));
    let both = CodexServerEntry {
        command: Some("x".to_owned()),
        url: Some("https://mcp.example.com/mcp".to_owned()),
        ..CodexServerEntry::default()
    };
    assert!(matches!(
        config.set_server("both", &both),
        Err(CodexError::AmbiguousTransport { .. })
    ));
    let bad_url = CodexServerEntry {
        url: Some("not a url".to_owned()),
        ..CodexServerEntry::default()
    };
    assert!(matches!(
        config.set_server("bad-url", &bad_url),
        Err(CodexError::InvalidUrl { .. })
    ));

    // `shuttle` is a URL server; adding a command next to its url is ambiguous.
    let mut golden = CodexConfigFile::parse(GOLDEN).expect("parse");
    assert!(matches!(
        golden.set_server("shuttle", &server_with_command("npx")),
        Err(CodexError::AmbiguousTransport { .. })
    ));
}

#[test]
fn a_scalar_server_entry_is_refused_rather_than_overwritten() {
    let source = "mcp_servers = { broken = 1 }\n";
    let mut config = CodexConfigFile::parse(source).expect("parse");
    assert!(matches!(
        config.set_server("broken", &server_with_command("x")),
        Err(CodexError::RootNotTable { .. })
    ));
    assert_eq!(config.text(), source);

    // The same rule for a non-table key inside a real group.
    let source = "[mcp_servers]\nscalar = 1\n";
    let mut config = CodexConfigFile::parse(source).expect("parse");
    assert!(matches!(
        config.set_server("scalar", &server_with_command("x")),
        Err(CodexError::EntryNotTable { .. })
    ));
    assert!(!config.is_dirty());
}

// The write goes through at-fs: a config changed by someone else since the load is refused.
#[test]
fn save_refuses_a_concurrent_edit_and_writes_nothing() {
    let (_dir, path) = sandbox(GOLDEN);
    let mut config = CodexConfigFile::load(&path).expect("load");
    config
        .set_server("node_repl", &server_with_command("uvx"))
        .expect("patch");

    let external = GOLDEN.replace("model = \"gpt-5-codex\"", "model = \"gpt-5\"");
    fs::write(&path, &external).expect("external edit");

    let error = config
        .save(&path, BackupPolicy::None)
        .expect_err("stale write must be refused");
    match error {
        CodexError::Write(FsError::ConcurrentModification { .. }) => {}
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).expect("read back"), external);
}

#[test]
fn save_creates_a_missing_config_file_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    // An installed Codex owns its home directory; the adapter only writes the file.
    fs::create_dir(dir.path().join(CODEX_HOME)).expect("codex home");
    let path = config_path(dir.path());
    let config = CodexConfigFile::load(&path).expect("missing file is not an error");
    assert_eq!(config.expected(), &ExpectedState::Absent);

    let mut config = config;
    config
        .set_server("playwright", &server_with_command("npx"))
        .expect("set server");
    let saved = config.save(&path, BackupPolicy::None).expect("save");
    assert!(saved.outcome().is_some());
    let text = fs::read_to_string(&path).expect("read back");
    assert_eq!(text, "[mcp_servers.playwright]\ncommand = \"npx\"\n");

    // A second save of the same document writes nothing.
    assert_eq!(
        config.save(&path, BackupPolicy::None).expect("save again"),
        CodexSave::Unchanged
    );
}

#[test]
fn adding_a_server_does_not_need_a_comment_or_a_header_block() {
    let mut config = CodexConfigFile::parse("").expect("parse");
    config
        .set_server("first", &server_with_command("one"))
        .expect("set server");
    assert_same_document(&config.text(), "[mcp_servers.first]\ncommand = \"one\"\n");
}

#[test]
fn save_writes_through_a_symlinked_config() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dotfiles = dir.path().join("dotfiles");
    fs::create_dir(&dotfiles).expect("dotfiles dir");
    let target = dotfiles.join("config.toml");
    fs::write(&target, GOLDEN).expect("write target");
    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    let mut config = CodexConfigFile::load(&link).expect("load through link");
    config
        .set_server("node_repl", &server_with_command("uvx"))
        .expect("patch");
    let saved = config.save(&link, BackupPolicy::None).expect("save");
    assert!(
        saved
            .outcome()
            .and_then(|outcome| outcome.symlink.as_ref())
            .is_some_and(|symlink| symlink.link_path == link),
        "expected a symlink write, got {saved:?}"
    );
    assert!(
        fs::symlink_metadata(&link)
            .expect("link meta")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), GOLDEN_COMMAND);
}

// An unrelated table between two server tables keeps its position, and the new server is
// still appended after the last server table rather than next to the first one.
#[test]
fn an_interleaved_config_keeps_its_order_and_gains_the_server_last() {
    let source = "[mcp_servers.a]\ncommand = \"1\"\n\n[features]\nx = true\n\n[mcp_servers.b]\ncommand = \"2\"\n";
    let mut config = CodexConfigFile::parse(source).expect("parse");
    assert_eq!(config.text(), source);

    config
        .set_server("c", &server_with_command("3"))
        .expect("add server");
    assert_same_document(
        &config.text(),
        "[mcp_servers.a]\ncommand = \"1\"\n\n[features]\nx = true\n\n[mcp_servers.b]\ncommand = \"2\"\n\n[mcp_servers.c]\ncommand = \"3\"\n",
    );
}
