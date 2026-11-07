//! Behaviour tests for the omp MCP adapter (TM-007).
//!
//! Every test works on synthetic fixtures copied into a temporary directory; nothing here
//! touches a real home directory, a real agent folder or the network.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_adapters::omp_mcp::{
    DEFAULT_SCHEMA, NEW_FILE_MODE, OmpMcpAdapter, OmpMcpConfig, OmpMcpError, OmpServer, agent_root,
    detect, mcp_path, schema_from, skills_path,
};
use at_fs::{BackupPolicy, FsError};
use at_model::{EnvValue, McpTransport, SecretRef};
use similar::{DiffTag, TextDiff};
use tempfile::TempDir;

const GOLDEN: &str = include_str!("fixtures/omp/golden_mcp.json");
const ENABLED_FALSE: &str = include_str!("fixtures/omp/enabled_false.json");
const UNKNOWN_KEYS: &str = include_str!("fixtures/omp/unknown_keys.json");

fn http_server(name: &str, url: &str, enabled: bool) -> OmpServer {
    OmpServer::new(
        name,
        McpTransport::Http {
            url: url.parse().expect("test URL"),
            headers: BTreeMap::new(),
            oauth: None,
        },
        enabled,
    )
}

fn stdio_server(name: &str, command: &str, args: &[&str]) -> OmpServer {
    OmpServer::new(
        name,
        McpTransport::Stdio {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: None,
            env: BTreeMap::new(),
        },
        true,
    )
}

/// Writes `text` to `<temp>/mcp.json` and returns the directory guard with the path.
fn seeded(text: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("mcp.json");
    fs::write(&path, text).expect("seed file");
    (dir, path)
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("read back")
}

fn document(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("fixture is plain JSON")
}

/// Bytes from the start of the document through the opening brace of `mcpServers`.
fn servers_prefix(text: &str) -> &str {
    let marker = "\"mcpServers\": {";
    let index = text.find(marker).expect("mcpServers") + marker.len();
    &text[..index]
}

/// Bytes after the closing brace of `mcpServers`, string- and nesting-aware.
fn servers_suffix(text: &str) -> &str {
    let start = text.find("\"mcpServers\"").expect("mcpServers");
    let open = start + text[start..].find('{').expect("open brace");
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in text[open..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + offset + character.len_utf8()..];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced mcpServers object");
}

/// Nothing outside the `mcpServers` object changed, byte for byte.
fn assert_servers_envelope_unchanged(old: &str, new: &str) {
    assert_eq!(servers_prefix(old), servers_prefix(new), "prefix changed");
    assert_eq!(servers_suffix(old), servers_suffix(new), "suffix changed");
}

/// `new` is `old` with exactly `name` added to `mcpServers`.
fn assert_only_server_added(old: &str, new: &str, name: &str) {
    assert_servers_envelope_unchanged(old, new);
    let mut value = document(new);
    let servers = value["mcpServers"]
        .as_object_mut()
        .expect("mcpServers object");
    assert!(servers.remove(name).is_some(), "`{name}` was not added");
    assert_eq!(
        value,
        document(old),
        "something other than `{name}` changed"
    );
}

/// All changed old lines lie inside the 1-based inclusive `start..=end` span.
fn assert_changes_within(old: &str, new: &str, start: usize, end: usize) {
    let diff = TextDiff::from_lines(old, new);
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let range = op.old_range();
        let first = range.start + 1;
        let last = range.end.max(first);
        assert!(
            first >= start && last <= end,
            "change outside lines {start}..={end}: {:?} at old lines {first}..={last}",
            op.tag()
        );
    }
}

/// 1-based inclusive line span of the `mcpServers` entry called `name`.
fn entry_span(text: &str, name: &str) -> (usize, usize) {
    let needle = format!("\"{name}\": {{");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(&needle))
        .expect("entry start")
        + 1;
    let end = lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.starts_with("    }"))
        .map(|(index, _)| index + 1)
        .expect("entry end");
    (start, end)
}

#[test]
fn golden_fixture_round_trips_byte_identical_through_a_save() {
    let adapter = OmpMcpAdapter::new();
    let (_dir, path) = seeded(GOLDEN);
    let config = adapter.read(&path).expect("read").expect("exists");

    assert_eq!(config.schema(), Some(DEFAULT_SCHEMA));
    assert_eq!(config.servers().len(), 2);
    // A missing `type` is inferred as stdio, and an absent `enabled` means enabled.
    let local = config.server("local-tool").expect("local-tool");
    assert!(local.transport.is_stdio());
    assert!(local.enabled);
    // Unknown per-server keys are carried verbatim.
    assert_eq!(local.extra.get("timeout"), Some(&serde_json::json!(20000)));
    assert_eq!(
        local.extra.get("instructions"),
        Some(&serde_json::json!(false))
    );
    let remote = config.server("remote-tool").expect("remote-tool");
    assert!(remote.transport.is_http());
    assert!(!remote.enabled);

    let rendered = config.render_unchanged().expect("render");
    assert_eq!(rendered, GOLDEN, "parse then render must be byte identical");

    adapter
        .save_edit(&path, &config, &rendered, BackupPolicy::None)
        .expect("unchanged save");
    assert_eq!(read_text(&path), GOLDEN, "no edit means no byte changes");
}

#[test]
fn adding_an_http_server_writes_type_url_enabled_and_no_command() {
    let config = OmpMcpConfig::parse(GOLDEN).expect("parse");
    let server = http_server("added", "https://mcp.example.test/second", true);

    let updated = config.upsert_server(&server).expect("upsert");
    assert_only_server_added(GOLDEN, &updated, "added");

    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    let added = reparsed.server("added").expect("added entry");
    match &added.transport {
        McpTransport::Http { url, .. } => {
            assert_eq!(url.as_str(), "https://mcp.example.test/second");
        }
        other => panic!("expected http transport, got {other:?}"),
    }
    assert!(added.enabled);

    let (start, end) = entry_span(&updated, "added");
    let entry = updated
        .lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(entry.contains("\"type\": \"http\""), "{entry}");
    assert!(entry.contains("\"url\": \"https://mcp.example.test/second\""));
    assert!(entry.contains("\"enabled\": true"));
    assert!(
        !entry.contains("\"command\""),
        "http entry must not carry a command"
    );
    // The header and the pre-existing servers are untouched.
    assert_eq!(updated.matches("\"$schema\"").count(), 1);
    assert!(updated.contains("\"remote-tool\": {"));
}

#[test]
fn unknown_top_level_and_per_server_keys_survive_a_write() {
    let config = OmpMcpConfig::parse(UNKNOWN_KEYS).expect("parse");
    assert_eq!(
        config.schema(),
        Some("https://example.test/mcp-schema.json")
    );
    assert!(config.extra().contains_key("customTopLevel"));
    assert!(config.extra().contains_key("enabledServers"));

    let updated = config
        .upsert_server(&stdio_server("beta", "beta-mcp", &["--flag"]))
        .expect("upsert");
    assert_only_server_added(UNKNOWN_KEYS, &updated, "beta");
    assert_eq!(updated.matches("\"$schema\"").count(), 1, "{updated}");

    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    assert_eq!(
        reparsed.schema(),
        Some("https://example.test/mcp-schema.json")
    );
    assert!(reparsed.extra().contains_key("customTopLevel"));
    assert!(reparsed.extra().contains_key("enabledServers"));
    let alpha = reparsed.server("alpha").expect("alpha");
    assert_eq!(
        alpha.extra.get("x-vendor"),
        Some(&serde_json::json!({"flag": 1}))
    );
    assert!(reparsed.server("beta").is_some());
}

#[test]
fn a_blank_source_is_preserved_until_an_edit_lands() {
    let config = OmpMcpConfig::parse("\n").expect("parse blank");
    assert_eq!(config.render_unchanged().expect("render"), "\n");
    assert!(config.servers().is_empty());

    let updated = config
        .upsert_server(&stdio_server("first", "first-mcp", &[]))
        .expect("upsert into blank");
    assert_eq!(
        updated,
        concat!(
            "{\n",
            "  \"mcpServers\": {\n",
            "    \"first\": {\n",
            "      \"type\": \"stdio\",\n",
            "      \"command\": \"first-mcp\",\n",
            "      \"enabled\": true\n",
            "    }\n",
            "  }\n",
            "}\n",
        )
    );
    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    assert!(reparsed.server("first").is_some());
}

#[test]
fn missing_file_is_created_once_with_schema_and_empty_servers() {
    let adapter = OmpMcpAdapter::new();
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(".omp/agent/mcp.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("agent dir");

    assert!(adapter.read(&path).expect("read absent").is_none());

    adapter
        .create(&path, &OmpMcpAdapter::create_text(None), BackupPolicy::None)
        .expect("create");

    let text = read_text(&path);
    assert_eq!(
        text,
        format!("{{\n  \"$schema\": {DEFAULT_SCHEMA:?},\n  \"mcpServers\": {{}}\n}}\n")
    );
    let config = OmpMcpConfig::parse(&text).expect("parse created");
    assert_eq!(config.schema(), Some(DEFAULT_SCHEMA));
    assert!(config.servers().is_empty());

    let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, NEW_FILE_MODE, "new file must be 0644");
}

#[test]
fn a_second_save_does_not_duplicate_the_schema_header() {
    let adapter = OmpMcpAdapter::new();
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("mcp.json");

    // Absent file: one create plus the first server, in one write.
    adapter
        .upsert_server(
            &path,
            &http_server("first", "https://one.example.test/mcp", true),
            BackupPolicy::None,
        )
        .expect("first save");
    // Existing file: read, patch, replace.
    adapter
        .upsert_server(
            &path,
            &stdio_server("second", "second-mcp", &["--go"]),
            BackupPolicy::None,
        )
        .expect("second save");

    let text = read_text(&path);
    assert_eq!(text.matches("\"$schema\"").count(), 1, "{text}");
    assert_eq!(text.matches("\"mcpServers\"").count(), 1, "{text}");
    let config = OmpMcpConfig::parse(&text).expect("parse");
    assert_eq!(config.schema(), Some(DEFAULT_SCHEMA));
    assert_eq!(config.servers().len(), 2);
}

#[test]
fn enabled_false_survives_an_unrelated_args_edit() {
    let config = OmpMcpConfig::parse(ENABLED_FALSE).expect("parse");
    let mut quiet = config.server("quiet").expect("quiet").clone();
    assert!(!quiet.enabled);
    match &mut quiet.transport {
        McpTransport::Stdio { args, .. } => args.push("--dry-run".to_owned()),
        other => panic!("expected stdio, got {other:?}"),
    }

    let updated = config.upsert_server(&quiet).expect("upsert");
    let (start, end) = entry_span(ENABLED_FALSE, "quiet");
    assert_changes_within(ENABLED_FALSE, &updated, start, end);

    assert!(updated.contains("\"enabled\": false"), "{updated}");
    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    let quiet = reparsed.server("quiet").expect("quiet");
    assert!(!quiet.enabled);
    match &quiet.transport {
        McpTransport::Stdio { args, .. } => {
            assert_eq!(
                args,
                &[
                    "--once".to_owned(),
                    "--verbose".to_owned(),
                    "--dry-run".to_owned()
                ]
            );
        }
        other => panic!("expected stdio, got {other:?}"),
    }
    assert_eq!(
        reparsed.server("neighbour").expect("neighbour"),
        config.server("neighbour").expect("neighbour")
    );
}

#[test]
fn upserting_an_unchanged_server_changes_no_byte() {
    let config = OmpMcpConfig::parse(GOLDEN).expect("parse");
    for name in ["local-tool", "remote-tool"] {
        let server = config.server(name).expect("server").clone();
        let updated = config.upsert_server(&server).expect("upsert");
        assert_eq!(updated, GOLDEN, "`{name}` must be left alone");
    }
}

#[test]
fn switching_a_stdio_entry_to_http_removes_stdio_only_keys() {
    let config = OmpMcpConfig::parse(GOLDEN).expect("parse");
    let updated = config
        .upsert_server(&http_server(
            "local-tool",
            "https://local.example.test/mcp",
            true,
        ))
        .expect("switch transport");

    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    let entry = reparsed.server("local-tool").expect("local-tool");
    assert!(entry.transport.is_http());
    assert!(
        !updated.contains("\"local-tool-mcp\""),
        "stale args must go"
    );
    // Unknown keys of the entry stay.
    assert_eq!(entry.extra.get("timeout"), Some(&serde_json::json!(20000)));
}

#[test]
fn removing_a_server_touches_only_its_entry() {
    let config = OmpMcpConfig::parse(GOLDEN).expect("parse");
    let updated = config.remove_server("local-tool").expect("remove");

    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    assert!(reparsed.server("local-tool").is_none());
    assert!(reparsed.server("remote-tool").is_some());
    assert_eq!(reparsed.schema(), Some(DEFAULT_SCHEMA));
    assert!(updated.contains("\"disabledServers\""));
    // Removing the last entry of an object must not leave a dangling comma behind.
    let stripped = reparsed.remove_server("remote-tool").expect("remove all");
    let reparsed = OmpMcpConfig::parse(&stripped).expect("reparse empty");
    assert!(reparsed.servers().is_empty());
    assert!(stripped.contains("\"mcpServers\": {}"), "{stripped}");
}

#[test]
fn secret_env_values_refuse_unverified_expansion() {
    let mut env = BTreeMap::new();
    env.insert(
        String::from("GITHUB_TOKEN"),
        EnvValue::Secret(SecretRef {
            env_name: String::from("GITHUB_TOKEN"),
            keychain_account: String::from("github-token"),
        }),
    );
    let server = OmpServer::new(
        "github",
        McpTransport::Stdio {
            command: String::from("github-mcp"),
            args: Vec::new(),
            cwd: None,
            env,
        },
        true,
    );

    let config = OmpMcpAdapter::create_document(None).expect("empty document");
    let error = config
        .upsert_server(&server)
        .expect_err("secret in omp env must be refused");
    assert!(
        matches!(
            error,
            OmpMcpError::UnverifiedSecretExpansion { ref server, ref variable }
            if server == "github" && variable == "GITHUB_TOKEN"
        ),
        "expected UnverifiedSecretExpansion, got {error:?}"
    );
}

#[test]
fn an_external_edit_between_read_and_write_is_refused_and_retry_preserves_it() {
    let adapter = OmpMcpAdapter::new();
    let (_dir, path) = seeded(GOLDEN);
    let original_mtime = fs::metadata(&path)
        .expect("original metadata")
        .modified()
        .expect("original mtime");
    let config = adapter.read(&path).expect("read").expect("exists");
    let planned = config
        .upsert_server(&stdio_server("planned", "planned-mcp", &[]))
        .expect("plan");

    // Another writer (omp itself, or the user) lands first with different bytes and mtime.
    let external = config
        .upsert_server(&stdio_server("external", "external-mcp", &[]))
        .expect("external patch");
    fs::write(&path, &external).expect("external write");
    let external_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::File::open(&path)
        .expect("open external config")
        .set_times(fs::FileTimes::new().set_modified(external_mtime))
        .expect("set external mtime");
    let changed_mtime = fs::metadata(&path)
        .expect("external metadata")
        .modified()
        .expect("external mtime");
    assert_ne!(changed_mtime, original_mtime, "the external mtime changed");
    assert_eq!(read_text(&path), external, "the external content changed");

    let error = adapter
        .save_edit(&path, &config, &planned, BackupPolicy::None)
        .expect_err("stale preview must be refused");
    match error {
        OmpMcpError::Fs(FsError::ConcurrentModification { path: refused, .. }) => {
            assert_eq!(refused, path);
        }
        other => panic!("expected ConcurrentModification, got {other:?}"),
    }
    assert_eq!(
        read_text(&path),
        external,
        "the other writer's version stays"
    );
    assert_eq!(
        fs::read_dir(path.parent().expect("config parent"))
            .expect("list config directory")
            .count(),
        1,
        "a conflict leaves no temp sibling"
    );

    // Re-reading and re-applying succeeds and keeps the external entry.
    let fresh = adapter.read(&path).expect("re-read").expect("exists");
    let retry = fresh
        .upsert_server(&stdio_server("planned", "planned-mcp", &[]))
        .expect("retry");
    adapter
        .save_edit(&path, &fresh, &retry, BackupPolicy::None)
        .expect("retry save");
    let text = read_text(&path);
    assert!(text.contains("\"external\": {"));
    assert!(text.contains("\"planned\": {"));
    assert_eq!(
        fs::read_dir(path.parent().expect("config parent"))
            .expect("list config directory")
            .count(),
        1,
        "a successful retry also leaves no temp sibling"
    );
}

#[test]
fn paths_and_detection_do_not_require_a_skills_directory() {
    let home = TempDir::new().expect("home");

    assert_eq!(agent_root(home.path()), home.path().join(".omp/agent"));
    assert_eq!(
        mcp_path(home.path()),
        home.path().join(".omp/agent/mcp.json")
    );
    assert_eq!(
        skills_path(home.path()),
        home.path().join(".omp/agent/skills")
    );
    assert_eq!(detect(home.path()), None);

    // omp installed with no `skills` sibling: still detected, and the MCP path is unchanged.
    fs::create_dir_all(agent_root(home.path())).expect("agent root");
    assert_eq!(detect(home.path()), Some(agent_root(home.path())));
    assert!(!skills_path(home.path()).exists());
}

#[test]
fn an_unmodeled_transport_is_reported_instead_of_converted() {
    let source = r#"{
  "mcpServers": {
    "legacy": {
      "type": "sse",
      "url": "https://legacy.example.test/sse"
    }
  }
}
"#;
    match OmpMcpConfig::parse(source) {
        Err(OmpMcpError::UnsupportedTransport { server, kind }) => {
            assert_eq!(server, "legacy");
            assert_eq!(kind, "sse");
        }
        other => panic!("expected UnsupportedTransport, got {other:?}"),
    }
}

#[test]
fn names_omp_rejects_are_refused_before_writing() {
    let config = OmpMcpAdapter::create_document(None).expect("document");
    let invalid = stdio_server("has space", "x-mcp", &[]);
    match config.upsert_server(&invalid) {
        Err(OmpMcpError::InvalidServerName { name }) => assert_eq!(name, "has space"),
        other => panic!("expected InvalidServerName, got {other:?}"),
    }

    // Dots and hyphens are legal and round-trip as literal JSON keys.
    let valid = stdio_server("with.dot-and-dash", "dot-mcp", &[]);
    let updated = config.upsert_server(&valid).expect("upsert dotted name");
    assert!(updated.contains("\"with.dot-and-dash\": {"));
    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    assert!(reparsed.server("with.dot-and-dash").is_some());
}

#[test]
fn a_missing_servers_object_is_created_around_the_existing_document() {
    let source = "{\n  \"$schema\": \"https://example.test/mcp-schema.json\"\n}\n";
    let config = OmpMcpConfig::parse(source).expect("parse");
    let updated = config
        .upsert_server(&http_server("first", "https://one.example.test/mcp", true))
        .expect("upsert");

    assert!(
        updated.starts_with("{\n  \"$schema\": \"https://example.test/mcp-schema.json\",\n"),
        "{updated}"
    );
    assert_eq!(updated.matches("\"mcpServers\"").count(), 1);
    assert_eq!(updated.matches("\"$schema\"").count(), 1);
    let mut value = document(&updated);
    value["mcpServers"]
        .as_object_mut()
        .expect("mcpServers object")
        .remove("first")
        .expect("added server");
    value
        .as_object_mut()
        .expect("root object")
        .remove("mcpServers");
    assert_eq!(value, document(source), "only mcpServers was added");
    let reparsed = OmpMcpConfig::parse(&updated).expect("reparse");
    assert_eq!(
        reparsed.schema(),
        Some("https://example.test/mcp-schema.json")
    );
    assert!(reparsed.server("first").is_some());
}

#[test]
fn a_new_document_copies_the_schema_from_an_existing_omp_file() {
    let dir = TempDir::new().expect("dir");
    let existing = dir.path().join(".mcp.json");
    fs::write(
        &existing,
        "{\n  \"$schema\": \"https://example.test/project-schema.json\",\n  \"mcpServers\": {}\n}\n",
    )
    .expect("seed existing document");
    let broken = dir.path().join("broken.json");
    fs::write(&broken, "{ not json").expect("seed broken document");

    // A broken candidate is skipped rather than blocking creation.
    let schema = schema_from(&[broken, existing]).expect("schema copied from the valid file");
    assert_eq!(schema, "https://example.test/project-schema.json");

    let config = OmpMcpAdapter::create_document(Some(&schema)).expect("document");
    assert_eq!(
        config.schema(),
        Some("https://example.test/project-schema.json")
    );

    // Nothing to copy: the verified default is used.
    assert_eq!(schema_from(&[dir.path().join("absent.json")]), None);
    assert_eq!(
        OmpMcpAdapter::create_document(None)
            .expect("default")
            .schema(),
        Some(DEFAULT_SCHEMA)
    );
}

#[test]
fn invalid_text_is_never_written() {
    let adapter = OmpMcpAdapter::new();
    let (_dir, path) = seeded(GOLDEN);
    let config = adapter.read(&path).expect("read").expect("exists");

    let error = adapter
        .save_edit(&path, &config, "{ broken", BackupPolicy::None)
        .expect_err("invalid JSONC must be refused");
    assert!(matches!(error, OmpMcpError::Parse { .. }), "{error:?}");
    assert_eq!(read_text(&path), GOLDEN, "the file is untouched");

    let dir = TempDir::new().expect("dir");
    let fresh = dir.path().join("mcp.json");
    let error = adapter
        .create(&fresh, "not json", BackupPolicy::None)
        .expect_err("invalid JSONC must be refused");
    assert!(matches!(error, OmpMcpError::Parse { .. }), "{error:?}");
    assert!(!fresh.exists(), "nothing is created");
}

#[test]
fn saving_through_a_symlinked_config_updates_the_target() {
    let dir = TempDir::new().expect("dir");
    let target = dir.path().join("real-mcp.json");
    fs::write(&target, GOLDEN).expect("seed target");
    let link = dir.path().join("mcp.json");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    let adapter = OmpMcpAdapter::new();
    let config = adapter.read(&link).expect("read").expect("exists");
    let updated = config
        .upsert_server(&http_server(
            "linked",
            "https://linked.example.test/mcp",
            true,
        ))
        .expect("upsert");
    adapter
        .save_edit(&link, &config, &updated, BackupPolicy::None)
        .expect("save through the link");

    let metadata = fs::symlink_metadata(&link).expect("link metadata");
    assert!(
        metadata.file_type().is_symlink(),
        "the link must stay a link"
    );
    let written = read_text(&target);
    assert!(written.contains("\"linked\": {"), "{written}");
    assert!(written.contains("\"local-tool\": {"));
    assert_eq!(written.matches("\"$schema\"").count(), 1);
}

fn omp_skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: fixture skill\n---\n# {name}\n")
}

#[test]
fn managed_skills_only_fixture_reports_the_managed_root_with_two_skills() {
    use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
    use at_adapters::omp_adapter::OmpAdapter;
    use at_model::{ItemKind, Scope};

    let home = TempDir::new().expect("home");
    let managed = agent_root(home.path()).join("managed-skills");
    for name in ["alpha", "beta"] {
        let folder = managed.join(name);
        fs::create_dir_all(&folder).expect("managed skill folder");
        fs::write(folder.join("SKILL.md"), omp_skill(name)).expect("skill document");
    }
    let env = AgentEnvironment {
        home: home.path().to_path_buf(),
        project: None,
        variables: BTreeMap::new(),
    };
    let skills = OmpAdapter
        .list_native_skills(&env, &[])
        .expect("managed skills only");
    assert_eq!(skills.len(), 2, "the managed root contributes both skills");
    assert!(
        skills.iter().all(|skill| skill.path.starts_with(&managed)),
        "every row comes from the managed root: {skills:?}"
    );
    let resolved =
        at_adapters::paths::resolve(&OmpAdapter, ItemKind::Skill, Scope::User, &env, None, None)
            .expect("managed root resolves");
    assert_eq!(resolved.primary, managed);
    let unmanaged = resolved
        .candidates
        .iter()
        .find(|candidate| candidate.candidate.path == skills_path(home.path()))
        .expect("unmanaged skills candidate is still reported");
    assert!(!unmanaged.exists, "the skills root exists=false");
}

#[test]
fn deleting_the_managed_skills_directory_flips_to_needs_path_on_the_next_scan() {
    use at_adapters::adapter::AgentEnvironment;
    use at_adapters::omp_adapter::OmpAdapter;
    use at_model::{ItemKind, Scope};

    let home = TempDir::new().expect("home");
    let managed = agent_root(home.path()).join("managed-skills");
    fs::create_dir_all(managed.join("alpha")).expect("managed skill folder");
    fs::write(managed.join("alpha/SKILL.md"), omp_skill("alpha")).expect("skill document");
    let env = AgentEnvironment {
        home: home.path().to_path_buf(),
        project: None,
        variables: BTreeMap::new(),
    };
    let first =
        at_adapters::paths::resolve(&OmpAdapter, ItemKind::Skill, Scope::User, &env, None, None)
            .expect("first scan finds the managed root");
    assert_eq!(first.primary, managed);

    fs::remove_dir_all(&managed).expect("delete the managed root");
    let needs_path = at_adapters::paths::resolve(
        &OmpAdapter,
        ItemKind::Skill,
        Scope::User,
        &env,
        None,
        Some(&managed),
    )
    .expect_err("no skills root remains");
    assert_eq!(
        needs_path.last_known_path.as_deref(),
        Some(managed.as_path())
    );
    assert!(
        needs_path
            .candidates
            .iter()
            .all(|candidate| !candidate.verified),
        "both candidates are unverified after the deletion"
    );
}

#[derive(Default)]
struct RecordingOmpMcpDelegate {
    reads: std::sync::atomic::AtomicUsize,
}

impl at_adapters::adapter::McpAdapterDelegate for RecordingOmpMcpDelegate {
    fn read_io(
        &self,
        _: at_model::Scope,
        env: &at_adapters::adapter::AgentEnvironment,
    ) -> Result<at_adapters::adapter::AgentIoDocument, at_adapters::adapter::AgentError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = mcp_path(&env.home);
        let content = fs::read(&path).map_err(|source| at_adapters::adapter::AgentError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(at_adapters::adapter::AgentIoDocument { path, content })
    }

    fn render_io(
        &self,
        _: &at_adapters::adapter::AgentDeployment,
    ) -> Result<Vec<u8>, at_adapters::adapter::AgentError> {
        Err(at_adapters::adapter::AgentError::Unsupported {
            agent: at_model::AgentId::Omp,
            kind: String::from("render is not exercised by a read"),
        })
    }

    fn write_io(
        &self,
        _: &at_adapters::adapter::AgentDeployment,
    ) -> Result<at_adapters::adapter::AgentIoDocument, at_adapters::adapter::AgentError> {
        Err(at_adapters::adapter::AgentError::Unsupported {
            agent: at_model::AgentId::Omp,
            kind: String::from("write is not exercised by a read"),
        })
    }
}

#[test]
fn mcp_read_through_the_registry_delegates_once_and_leaves_the_document_unchanged() {
    use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
    use at_model::{AgentId, ItemKind, Scope};

    let home = TempDir::new().expect("home");
    fs::create_dir_all(agent_root(home.path())).expect("agent root");
    let path = mcp_path(home.path());
    fs::write(&path, GOLDEN).expect("seed mcp.json");
    let before = at_fs::sha256_hex(&fs::read(&path).expect("fixture bytes"));
    let env = AgentEnvironment {
        home: home.path().to_path_buf(),
        project: None,
        variables: BTreeMap::new(),
    };
    let registry = AgentAdapterRegistry::register();
    let delegate = std::sync::Arc::new(RecordingOmpMcpDelegate::default());
    registry
        .register_mcp_delegate(AgentId::Omp, delegate.clone())
        .expect("register the TM-007 delegate");
    let document = registry
        .read_io(&AgentId::Omp, ItemKind::McpServer, Scope::User, &env)
        .expect("delegated MCP read");
    assert_eq!(
        delegate.reads.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the document comes from exactly one delegation call"
    );
    assert_eq!(document.path, path);
    assert_eq!(
        document.content,
        fs::read(&path).expect("fixture bytes"),
        "the delegated read returns the native bytes"
    );
    assert_eq!(
        at_fs::sha256_hex(&fs::read(&path).expect("fixture bytes after read")),
        before,
        "reading leaves the on-disk mcp.json hash unchanged"
    );
}
