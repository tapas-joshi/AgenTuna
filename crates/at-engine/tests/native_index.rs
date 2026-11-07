//! TM-020 / TM-033 / TC-025: native MCP servers, rule files and skills reach the derived index.
//!
//! Every path is redirected into a temporary directory: the tests never read the developer's
//! home, stores or agent folders, and no MCP server is started anywhere in this file.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use at_adapters::adapter::AgentEnvironment;
use at_db::Database;
use at_db::mcp_index::{McpCellState, mcp_list_rows};
use at_db::native::{NativeItemFilter, NativeStatus, list_native_items};
use at_engine::native_index::refresh_native;
use at_migrate::adopt::{AdoptHandling, AdoptRequest, adopt_item};
use at_migrate::scan::{ScanRequest, StoreItem, scan};
use at_migrate::undo::{UndoOperationRequest, UndoOutcome, undo_operation};
use at_model::{Agent, AgentId, ItemKind, Scope};
use at_store::layout::StoreLayout;
use at_store::manifest::StoreManifest;
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};

/// A sandboxed home, project, store root, database and agent environment.
struct Sandbox {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    paths: AppPaths,
    database: Database,
}

impl Sandbox {
    /// Creates the temporary roots and every directory a fixture later writes into.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary root");
        let root = temp.path().canonicalize().expect("canonical root");
        let project = root.join("project");
        for directory in [
            project.clone(),
            root.join("support"),
            root.join("user-store"),
            root.join("global-store"),
            root.join(".claude"),
            root.join(".codex"),
            root.join(".omp/agent"),
        ] {
            fs::create_dir_all(&directory).expect("fixture directory");
        }
        let paths = AppPaths::resolve(
            &PathEnvironment::default()
                .with_home(&root)
                .with_project_root(&project)
                .with_overrides(PathOverrides {
                    home: Some(root.join("support")),
                    user_store: Some(root.join("user-store")),
                    global_store: Some(root.join("global-store")),
                    project_store: None,
                }),
        )
        .expect("sandbox paths resolve");
        let database = Database::open_in_memory().expect("in-memory index");
        // The MCP list projection crosses the detected agents, so the fixture records the three
        // agents whose folders it seeds.
        for (id, display_name) in [
            (AgentId::ClaudeCode, "Claude Code"),
            (AgentId::Codex, "Codex"),
            (AgentId::Omp, "Oh My Pi"),
        ] {
            at_db::agents::upsert_agent(
                database.connection(),
                &Agent {
                    id,
                    display_name: display_name.to_owned(),
                    detected: true,
                    version: None,
                },
            )
            .expect("agent row");
        }
        Self {
            _temp: temp,
            root,
            project,
            paths,
            database,
        }
    }

    /// The agent environment the scanner reads roots from.
    fn environment(&self) -> AgentEnvironment {
        AgentEnvironment {
            home: self.root.clone(),
            project: Some(self.project.clone()),
            variables: BTreeMap::new(),
        }
    }

    /// Writes `bytes` at a path relative to the sandbox home.
    fn write_home(&self, relative: &str, bytes: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(&path, bytes).expect("fixture file");
    }

    /// Writes `bytes` at a path relative to the sandbox project.
    fn write_project(&self, relative: &str, bytes: &[u8]) {
        let path = self.project.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(&path, bytes).expect("fixture file");
    }

    /// Runs the shared refresh the app and CLI call.
    fn refresh(&self) -> at_engine::native_index::NativeRefreshReport {
        refresh_native(self.database.connection(), &self.paths).expect("native refresh")
    }

    /// Runs one read-only scan with the sandbox store inventory, as the migration screens do.
    fn scan(&self) -> at_migrate::scan::ScanReport {
        let mut roots = Vec::new();
        let mut items = Vec::new();
        for scope in [Scope::Global, Scope::User, Scope::Project] {
            let Ok(layout) = StoreLayout::for_scope(&self.paths, scope) else {
                continue;
            };
            roots.push(layout.root().to_path_buf());
            if let Ok(loaded) = StoreManifest::load(&layout) {
                items.extend(loaded.manifest.items().iter().map(|item| StoreItem {
                    kind: item.kind,
                    name: item.name.clone().unwrap_or_else(|| item.slug.to_string()),
                    content_hash: item.hash.to_string(),
                    scope,
                    path: layout.root().join(item.path.to_string()),
                }));
            }
        }
        let environment = self.environment();
        let adapters = at_adapters::adapter::AgentAdapterRegistry::register();
        scan(ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &roots,
            store_items: &items,
        })
        .expect("scan")
    }

    /// Every native MCP finding keyed by the name the MCP list shows.
    fn native_mcp_rows(&self) -> Vec<NativeRow> {
        let mut statement = self
            .database
            .connection()
            .prepare(
                "SELECT name, scope, agent_id, path FROM native_items \
                  WHERE kind = 'mcp_server' AND status = 'new' ORDER BY name",
            )
            .expect("native rows query");
        statement
            .query_map([], |row| {
                Ok(NativeRow {
                    name: row.get(0)?,
                    scope: row.get(1)?,
                    agent: row.get(2)?,
                    path: row.get(3)?,
                })
            })
            .expect("native rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("native rows read")
    }
}

/// One `native_items` MCP row as the tests compare it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeRow {
    name: String,
    scope: String,
    agent: String,
    path: String,
}

/// The rule findings the refresh recorded.
fn native_rule_paths(sandbox: &Sandbox) -> Vec<String> {
    list_native_items(
        sandbox.database.connection(),
        &NativeItemFilter {
            include_missing: false,
            ..NativeItemFilter::default()
        },
    )
    .expect("native rows")
    .into_iter()
    .filter(|item| item.kind == ItemKind::Rule)
    .map(|item| item.path)
    .collect()
}

/// Seeds one Claude user config with two servers, one of them holding a secret.
fn seed_claude(sandbox: &Sandbox) {
    sandbox.write_home(
        ".claude.json",
        br#"{
  "numStartups": 3,
  "mcpServers": {
    "context7": {
      "command": "npx",
      "args": ["-y", "@upstash/context7-mcp"],
      "env": { "CONTEXT7_API_KEY": "SENTINEL_SECRET_VALUE" }
    },
    "github": {
      "type": "http",
      "url": "https://example.invalid/mcp"
    }
  }
}
"#,
    );
}

/// Seeds a Codex config with a comment and one server.
fn seed_codex(sandbox: &Sandbox) {
    sandbox.write_home(
        ".codex/config.toml",
        b"# keep this comment\nmodel = \"gpt-5\"\n\n[mcp_servers.alpha]\ncommand = \"alpha\"\nargs = [\"serve\"]\n",
    );
}

#[test]
fn refresh_indexes_native_mcp_servers_rules_and_skills_per_scope() {
    let sandbox = Sandbox::new();
    seed_claude(&sandbox);
    seed_codex(&sandbox);
    sandbox.write_home(
        ".omp/agent/mcp.json",
        br#"{"mcpServers": {"omp_docs": {"command": "omp-docs"}}}"#,
    );
    sandbox.write_project(
        ".mcp.json",
        br#"{"mcpServers": {"project_server": {"command": "project-server"}}}"#,
    );
    sandbox.write_home(".claude/CLAUDE.md", b"# Claude rules\nBe careful.\n");
    sandbox.write_home(".codex/AGENTS.md", b"# Codex rules\nBe quick.\n");
    sandbox.write_home(".omp/agent/AGENTS.md", b"# omp rules\nBe kind.\n");
    sandbox.write_project("CLAUDE.md", b"# Project rules for Claude\n");
    sandbox.write_project("AGENTS.md", b"# Project rules for every agent\n");
    sandbox.write_home(
        ".claude/skills/review/SKILL.md",
        b"---\nname: review\ndescription: Review a change\n---\n# Review\n",
    );

    let report = sandbox.refresh();
    assert!(
        report.upserted >= 9,
        "four MCP servers, five rule files and one skill are recorded: {report:?}"
    );
    assert!(
        !report.partial,
        "a complete fixture scans without warnings: {report:?}"
    );

    let rows = sandbox.native_mcp_rows();
    let by_name = |name: &str| {
        rows.iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("`{name}` is a native finding: {rows:?}"))
    };
    assert_eq!(by_name("context7").scope, "user");
    assert_eq!(by_name("context7").agent, "claude_code");
    assert_eq!(
        by_name("context7").path,
        format!("{}#context7", sandbox.root.join(".claude.json").display())
    );
    assert_eq!(by_name("github").agent, "claude_code");
    assert_eq!(by_name("alpha").agent, "codex");
    assert_eq!(by_name("omp_docs").agent, "omp");
    assert_eq!(by_name("project_server").scope, "project");
    assert_eq!(
        by_name("project_server").path,
        format!(
            "{}#project_server",
            sandbox.project.join(".mcp.json").display()
        )
    );

    let rules = native_rule_paths(&sandbox);
    assert!(
        rules.contains(&sandbox.root.join(".claude/CLAUDE.md").display().to_string()),
        "the Claude user rule file is a finding: {rules:?}"
    );
    assert!(
        rules.contains(&sandbox.project.join("AGENTS.md").display().to_string()),
        "the project AGENTS.md is a finding: {rules:?}"
    );
    let skills = list_native_items(sandbox.database.connection(), &NativeItemFilter::default())
        .expect("native rows")
        .into_iter()
        .filter(|item| item.kind == ItemKind::Skill)
        .count();
    assert_eq!(skills, 1, "the native skill folder is recorded once");

    // The MCP list projection reads the same rows, so the screen and the banner agree.
    let list = mcp_list_rows(sandbox.database.connection(), None, None).expect("mcp rows");
    let native_list: Vec<_> = list.iter().filter(|row| row.native_only).collect();
    assert_eq!(native_list.len(), 5, "one row per native server: {list:#?}");
    for row in &native_list {
        assert!(
            row.item_id.is_none(),
            "native-only rows have no canonical id"
        );
    }
    let claude_document = sandbox.root.join(".claude.json").display().to_string();
    let context7 = list
        .iter()
        .find(|row| row.display_name == "context7")
        .expect("context7 row");
    assert_eq!(
        context7.source_path.as_deref(),
        Some(claude_document.as_str()),
        "the row names the document, not the internal locator"
    );
    let cell = context7
        .agents
        .iter()
        .find(|cell| cell.agent_id == AgentId::ClaudeCode)
        .expect("claude cell");
    assert!(
        matches!(&cell.state, McpCellState::Native { path } if path.ends_with(".claude.json")),
        "the Claude cell is native and names the document: {:?}",
        cell.state
    );
}

#[test]
fn adopting_a_native_server_removes_its_row_and_undo_brings_it_back() {
    let mut sandbox = Sandbox::new();
    seed_codex(&sandbox);
    sandbox.refresh();
    assert!(
        sandbox
            .native_mcp_rows()
            .iter()
            .any(|row| row.name == "alpha"),
        "the codex server starts as a native finding"
    );

    let scan = sandbox.scan();
    let config = sandbox.root.join(".codex/config.toml");
    let authorized = at_fs::ApplyRoots::new(vec![sandbox.root.clone()]).expect("authorized roots");
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &config,
        name: Some("alpha"),
        store_root: sandbox.paths.user_store(),
        backup_root: sandbox.paths.backup_dir(),
        authorized_roots: &authorized,
        database: &mut sandbox.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt the codex server");
    assert_eq!(outcome.slug, "alpha");

    let report = sandbox.refresh();
    assert!(
        report.managed >= 1,
        "the adopted entry is recognised as AgenTuna-owned: {report:?}"
    );
    assert!(
        sandbox
            .native_mcp_rows()
            .iter()
            .all(|row| row.name != "alpha"),
        "an adopted server leaves the native-only set: {:?}",
        sandbox.native_mcp_rows()
    );
    let missing = list_native_items(
        sandbox.database.connection(),
        &NativeItemFilter {
            include_missing: true,
            ..NativeItemFilter::default()
        },
    )
    .expect("native rows")
    .into_iter()
    .find(|item| item.name == "alpha")
    .expect("the native row is kept as history");
    assert_eq!(
        missing.status,
        NativeStatus::Missing,
        "the row is marked missing instead of deleted"
    );
    let managed = mcp_list_rows(sandbox.database.connection(), None, None).expect("mcp rows");
    assert!(
        managed
            .iter()
            .any(|row| !row.native_only && row.display_name.eq_ignore_ascii_case("alpha")),
        "the canonical row replaces it: {managed:#?}"
    );

    let undo = undo_operation(
        UndoOperationRequest {
            connection: sandbox.database.connection(),
            store_root: sandbox.paths.user_store(),
            backup_root: sandbox.paths.backup_dir(),
            authorized_roots: &authorized,
            actor: at_db::ops::OperationActor::Ui,
            force: false,
        },
        &outcome.operation_id,
    )
    .expect("undo the adoption");
    assert!(matches!(undo, UndoOutcome::Restored { .. }));

    let report = sandbox.refresh();
    assert_eq!(
        report.resurrected, 1,
        "the previous row is visible again instead of duplicated: {report:?}"
    );
    let rows = sandbox.native_mcp_rows();
    assert_eq!(
        rows.len(),
        1,
        "only the codex server is a finding: {rows:?}"
    );
    assert_eq!(rows[0].name, "alpha");
}

#[test]
fn a_native_config_secret_never_reaches_the_index() {
    let sandbox = Sandbox::new();
    seed_claude(&sandbox);
    sandbox.write_home(
        ".omp/agent/mcp.json",
        br#"{"mcpServers": {"omp_docs": {"command": "omp-docs", "env": {"TOKEN": "SENTINEL_SECRET_VALUE"}}}}"#,
    );
    sandbox.refresh();

    let mut dump = String::new();
    let mut statement = sandbox
        .database
        .connection()
        .prepare(
            "SELECT id, agent_id, scope, kind, name, path, COALESCE(content_hash, '') \
               FROM native_items",
        )
        .expect("native rows query");
    let rows = statement
        .query_map([], |row| {
            let mut text = String::new();
            for index in 0..7 {
                text.push_str(&row.get::<_, String>(index)?);
                text.push('\u{1f}');
            }
            Ok(text)
        })
        .expect("native rows");
    for row in rows {
        dump.push_str(&row.expect("native row"));
        dump.push('\n');
    }
    let settings: String = sandbox
        .database
        .connection()
        .query_row(
            "SELECT COALESCE(group_concat(key || '=' || value_json, '\n'), '') FROM settings",
            [],
            |row| row.get(0),
        )
        .expect("settings");
    dump.push_str(&settings);

    assert!(
        !dump.contains("SENTINEL_SECRET_VALUE"),
        "a secret inside a native definition must not be stored: {dump}"
    );
    assert!(
        dump.contains("context7") && dump.contains("omp_docs"),
        "the server names are stored, so the rows prove the scan ran: {dump}"
    );
}

#[test]
fn a_malformed_config_yields_a_warning_and_keeps_the_other_agents() {
    let sandbox = Sandbox::new();
    seed_claude(&sandbox);
    sandbox.write_home(
        ".codex/config.toml",
        b"[mcp_servers.broken\ncommand = \"unterminated\"\n",
    );

    let report = sandbox.refresh();
    assert!(report.partial, "the partial scan is reported: {report:?}");
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.path == sandbox.root.join(".codex/config.toml"))
        .expect("the warning names the file that failed to parse");
    assert_eq!(warning.agent, AgentId::Codex);
    assert_eq!(warning.scope, Scope::User);

    let rows = sandbox.native_mcp_rows();
    assert!(
        rows.iter().all(|row| row.agent != "codex"),
        "a malformed document records no servers: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.agent == "claude_code").count(),
        2,
        "the readable agents are still scanned: {rows:?}"
    );
}

#[test]
fn a_five_hundred_server_document_stays_fast() {
    let sandbox = Sandbox::new();
    let mut servers = String::from("{\"mcpServers\": {");
    for index in 0..500 {
        if index > 0 {
            servers.push(',');
        }
        servers.push_str(&format!(
            "\"server_{index:03}\": {{\"command\": \"cmd-{index:03}\"}}"
        ));
    }
    servers.push_str("}}");
    sandbox.write_home(".omp/agent/mcp.json", servers.as_bytes());

    let started = Instant::now();
    let report = sandbox.refresh();
    let elapsed = started.elapsed();
    assert!(
        report.upserted >= 500,
        "every entry becomes a row: {report:?}"
    );
    assert_eq!(
        sandbox
            .native_mcp_rows()
            .iter()
            .filter(|row| row.agent == "omp")
            .count(),
        500
    );
    assert!(
        elapsed.as_secs() < 20,
        "500 servers stay well inside the bound; took {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_store_link_deployment_is_managed_and_never_becomes_a_native_row() {
    let sandbox = Sandbox::new();
    let layout = StoreLayout::for_scope(&sandbox.paths, Scope::User).expect("user store layout");
    layout.init().expect("initialize user store");
    let store_skill = layout.skills_dir().join("managed");
    fs::create_dir_all(&store_skill).expect("store skill folder");
    fs::write(
        store_skill.join("SKILL.md"),
        b"---\nname: managed\ndescription: Store deployment\n---\n# Managed\n",
    )
    .expect("store skill document");
    let loaded = StoreManifest::load(&layout).expect("load store manifest");
    let mut manifest = loaded.manifest;
    manifest
        .insert_item(at_store::manifest::ManifestItem::new(
            at_model::ItemId::new(),
            ItemKind::Skill,
            at_store::layout::Slug::parse("managed").expect("slug"),
            at_store::manifest::StorePath::parse("skills/managed").expect("item path"),
            at_store::manifest::hash_skill_folder(&store_skill).expect("content hash"),
        ))
        .expect("manifest item");
    manifest
        .save(
            &layout,
            at_fs::ExpectedState::Hash(loaded.hash.to_string()),
            at_fs::BackupPolicy::None,
        )
        .expect("save store manifest");

    let native_path = sandbox.root.join(".claude/skills/managed");
    fs::create_dir_all(native_path.parent().expect("skills root")).expect("skills root");
    std::os::unix::fs::symlink(&store_skill, &native_path).expect("store symlink");

    let scan = sandbox.scan();
    let item = scan
        .items
        .iter()
        .find(|item| item.path == native_path)
        .expect("the migration scan reports the store deployment");
    assert!(item.store_link, "the finding records the store link");
    assert_eq!(
        item.ownership,
        at_migrate::scan::Ownership::Managed,
        "a link into the store is classified Managed"
    );

    let report = sandbox.refresh();
    assert!(
        report.managed >= 1,
        "the refresh counts the store link as managed: {report:?}"
    );
    let rows = list_native_items(sandbox.database.connection(), &NativeItemFilter::default())
        .expect("native rows");
    assert!(
        !rows
            .iter()
            .any(|row| row.path == native_path.display().to_string()),
        "a managed deployment never becomes a native row"
    );
}
