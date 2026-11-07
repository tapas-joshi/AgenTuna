//! TA-030: golden-file tests for the folder-defined custom agent adapter.
//!
//! Fixtures are a JSON-format spec, a TOML-format spec, a spec with no MCP document and a
//! spec whose root folder disappears mid-session. Every assertion is behavioural: what the
//! adapter lists, what it reads, what it refuses and — for AC1 — that reading leaves the
//! tree byte-identical.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{
    AgentAdapter, AgentDeployment, AgentEnvironment, AgentError, DetectionState,
};
use at_adapters::custom_agent::{CustomAgentAdapter, CustomAgentSpec, CustomMcpFormat};
use at_adapters::generic_mcp::{GenericMcpFormat, GenericMcpWriter, NameQuoting};
use at_fs::sha256_hex;
use at_model::{ItemKind, Scope};
use tempfile::TempDir;

/// A complete fixture: agent root, home, project.
struct Fixture {
    dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("agent");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        fs::create_dir_all(&root).expect("agent root");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        Self {
            dir,
            root,
            home,
            project,
        }
    }

    fn env(&self) -> AgentEnvironment {
        AgentEnvironment {
            home: self.home.clone(),
            project: Some(self.project.clone()),
            variables: BTreeMap::new(),
        }
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(&path, content).expect("write fixture file");
    }

    /// The JSON-format goldens the story names.
    fn json_tool(&self) -> CustomAgentAdapter {
        self.write("skills/alpha/SKILL.md", "# Alpha\n");
        self.write("skills/beta/SKILL.md", "# Beta\n");
        self.write("RULES.md", "# Rules\nBe kind.\n");
        self.write(
            "mcp.json",
            "{\"mcpServers\":{\"demo\":{\"command\":\"x\"}}}\n",
        );
        CustomAgentAdapter::new(spec(&self.root, "mcp.json", CustomMcpFormat::Json))
            .expect("valid JSON spec")
    }

    fn dir_path(&self) -> &Path {
        self.dir.path()
    }
}

fn spec(root: &Path, mcp_file: &str, format: CustomMcpFormat) -> CustomAgentSpec {
    CustomAgentSpec {
        id: "golden-tool".to_owned(),
        display_name: "Golden Tool".to_owned(),
        root: root.to_path_buf(),
        skills_subfolder: PathBuf::from("skills"),
        mcp_file: Some(PathBuf::from(mcp_file)),
        mcp_format: format,
        rule_files: vec![PathBuf::from("RULES.md")],
        supports_symlink: false,
    }
}

/// Every file below `root` with its bytes hashed, so a write of any kind shows up.
fn snapshot(root: &Path) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("read fixture dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, found);
            } else {
                let bytes = fs::read(&path).expect("read fixture file");
                found.push((path.clone(), sha256_hex(&bytes)));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found
}

#[test]
fn json_fixture_reads_are_noop_and_listings_are_stable() {
    let fixture = Fixture::new();
    let adapter = fixture.json_tool();
    let env = fixture.env();
    let before = snapshot(&fixture.root);

    let first_skills = adapter
        .read_io(ItemKind::Skill, Scope::User, &env)
        .expect("skills listing");
    let first_rules = adapter
        .read_io(ItemKind::Rule, Scope::User, &env)
        .expect("rules read");
    let first_paths = adapter.paths(ItemKind::Skill, Scope::User, &env);

    let second_skills = adapter
        .read_io(ItemKind::Skill, Scope::User, &env)
        .expect("skills listing again");
    let second_rules = adapter
        .read_io(ItemKind::Rule, Scope::User, &env)
        .expect("rules read again");
    let second_paths = adapter.paths(ItemKind::Skill, Scope::User, &env);

    assert_eq!(
        first_skills.content, second_skills.content,
        "listings are identical"
    );
    assert_eq!(first_rules, second_rules, "rules bytes are identical");
    assert_eq!(
        first_paths, second_paths,
        "paths are identical across scans"
    );
    let listing = String::from_utf8(first_skills.content).expect("skill listing is UTF-8");
    assert!(
        listing.contains("alpha"),
        "the skills folder is listed: {listing}"
    );
    assert!(
        listing.contains("beta"),
        "the skills folder is listed: {listing}"
    );
    assert_eq!(
        String::from_utf8(first_rules.content).expect("rules are UTF-8"),
        "# Rules\nBe kind.\n"
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "reading skills and rules writes nothing"
    );
    assert!(
        !fixture
            .dir_path()
            .join("agent/skills")
            .join("SKILL.md")
            .exists(),
        "no listing file is written into the skills folder"
    );
}

#[test]
fn toml_spec_delegates_mcp_to_the_generic_writer_without_parsing() {
    let fixture = Fixture::new();
    fixture.write("RULES.md", "# Rules\n");
    fixture.write("mcp.toml", "[mcp_servers.demo]\ncommand = \"x\"\n");
    let adapter = CustomAgentAdapter::new(spec(&fixture.root, "mcp.toml", CustomMcpFormat::Toml))
        .expect("valid TOML spec");
    let env = fixture.env();

    let candidate = adapter
        .paths(ItemKind::McpServer, Scope::User, &env)
        .into_iter()
        .next()
        .expect("an MCP candidate is declared");
    assert_eq!(candidate.path, fixture.root.join("mcp.toml"));
    assert_eq!(
        candidate.format.as_deref(),
        Some("toml"),
        "the candidate names TM-008's toml format"
    );

    // The adapter hands MCP to TM-008: it never parses the document itself, so an invalid
    // TOML body still produces a delegation error rather than a parse error.
    fixture.write("mcp.toml", "this is not toml [[[");
    assert!(matches!(
        adapter.read_io(ItemKind::McpServer, Scope::User, &env),
        Err(AgentError::Delegate { .. })
    ));
    assert!(matches!(
        adapter.write_io(&AgentDeployment {
            kind: ItemKind::McpServer,
            scope: Scope::User,
            path: candidate.path.clone(),
            content: b"{}".to_vec(),
        }),
        Err(AgentError::Unsupported { .. })
    ));

    // The TM-008 writer accepts the same path and format the candidate declares.
    let source = "[mcp_servers.demo]\ncommand = \"x\"\n";
    let mut writer = GenericMcpWriter::parse(
        GenericMcpFormat::Toml,
        candidate.path.clone(),
        vec!["mcp_servers".to_owned()],
        Vec::new(),
        NameQuoting::Bare,
        source,
    )
    .expect("TM-008 parses the TOML document");
    writer
        .set_server("added", &serde_json::json!({ "command": "y" }))
        .expect("TM-008 inserts a server");
    assert!(
        writer.text().contains("[mcp_servers.added]"),
        "the generic writer owns the edit: {}",
        writer.text()
    );
}

#[test]
fn format_none_returns_unsupported_for_mcp_and_still_lists_skills_and_rules() {
    let fixture = Fixture::new();
    fixture.write("skills/alpha/SKILL.md", "# Alpha\n");
    fixture.write("RULES.md", "# Rules\n");
    let mut none_spec = spec(&fixture.root, "unused.json", CustomMcpFormat::Json);
    none_spec.mcp_file = None;
    none_spec.mcp_format = CustomMcpFormat::None;
    let adapter = CustomAgentAdapter::new(none_spec).expect("valid no-MCP spec");
    let env = fixture.env();

    assert!(!adapter.supports_mcp());
    assert!(
        adapter
            .paths(ItemKind::McpServer, Scope::User, &env)
            .is_empty()
    );
    assert!(matches!(
        adapter.read_io(ItemKind::McpServer, Scope::User, &env),
        Err(AgentError::Unsupported { .. })
    ));
    assert!(matches!(
        adapter.write_io(&AgentDeployment {
            kind: ItemKind::McpServer,
            scope: Scope::User,
            path: fixture.root.join("mcp.json"),
            content: b"{}".to_vec(),
        }),
        Err(AgentError::Unsupported { .. })
    ));

    assert!(!adapter.paths(ItemKind::Skill, Scope::User, &env).is_empty());
    assert!(adapter.read_io(ItemKind::Skill, Scope::User, &env).is_ok());
    assert!(!adapter.paths(ItemKind::Rule, Scope::User, &env).is_empty());
    assert!(adapter.read_io(ItemKind::Rule, Scope::User, &env).is_ok());
}

#[test]
fn deleting_the_root_between_reads_reports_not_installed_with_the_last_path() {
    let fixture = Fixture::new();
    let adapter = fixture.json_tool();
    let env = fixture.env();
    adapter
        .read_io(ItemKind::Rule, Scope::User, &env)
        .expect("rules read before the root disappears");
    let last_known = fixture.root.join("RULES.md");

    fs::remove_dir_all(&fixture.root).expect("root folder disappears");

    let error = adapter
        .read_io(ItemKind::Rule, Scope::User, &env)
        .expect_err("a deleted root is a typed absence");
    match &error {
        AgentError::NotInstalled { path, .. } => assert_eq!(path, &last_known),
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    assert!(
        error
            .to_string()
            .contains(&last_known.display().to_string()),
        "the error names the last known path: {error}"
    );
    assert_eq!(adapter.detect(&env).state, DetectionState::NotInstalled);
}

#[test]
fn rule_file_names_match_case_insensitively_and_keep_the_on_disk_name() {
    let fixture = Fixture::new();
    fixture.write("skills/alpha/SKILL.md", "# Alpha\n");
    fixture.write("agents.md", "lower-case file\n");
    let mut lower_spec = spec(&fixture.root, "mcp.json", CustomMcpFormat::Json);
    lower_spec.rule_files = vec![PathBuf::from("AGENTS.md")];
    let adapter = CustomAgentAdapter::new(lower_spec).expect("valid spec");
    let env = fixture.env();

    let candidate = adapter
        .paths(ItemKind::Rule, Scope::User, &env)
        .into_iter()
        .next()
        .expect("rule candidate");
    assert_eq!(
        candidate.path.file_name().and_then(|name| name.to_str()),
        Some("agents.md"),
        "the on-disk name is preserved, not the declared case"
    );
    assert_eq!(
        adapter
            .read_io(ItemKind::Rule, Scope::User, &env)
            .expect("case-insensitive read")
            .content,
        b"lower-case file\n"
    );

    // The reverse direction: declared lower case, stored upper case.
    let upper_root = fixture.root.clone();
    fs::remove_file(upper_root.join("agents.md")).expect("remove lower-case fixture");
    fixture.write("AGENTS.md", "upper-case file\n");
    let mut upper_spec = spec(&upper_root, "mcp.json", CustomMcpFormat::Json);
    upper_spec.rule_files = vec![PathBuf::from("agents.md")];
    let upper = CustomAgentAdapter::new(upper_spec).expect("valid spec");
    assert_eq!(
        upper
            .paths(ItemKind::Rule, Scope::User, &env)
            .into_iter()
            .next()
            .expect("rule candidate")
            .path
            .file_name()
            .and_then(|name| name.to_str()),
        Some("AGENTS.md")
    );
    assert_eq!(
        upper
            .read_io(ItemKind::Rule, Scope::User, &env)
            .expect("case-insensitive read")
            .content,
        b"upper-case file\n"
    );
}
