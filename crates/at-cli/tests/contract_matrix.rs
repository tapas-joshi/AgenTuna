//! TU-033: the machine contract, enforced across every path of the production tree.
//!
//! The matrix walks the registry the shipped binary uses, so a command joins the contract
//! the moment an owning slice registers it: every runnable path must appear in the success
//! table or the refusal table, and every path answers `--help`, a usage error and the JSON
//! envelope. Each invocation asserts the same invariants:
//!
//! - exactly one JSON document on stdout, never ANSI, never a log line;
//! - `schema_version`, `command`, `ok`, `data`, `warnings` and `error` with the published
//!   types, and `command` equal to the path that was typed;
//! - on failure, an `error.code` from [`ErrorCode::ALL`] whose [`ErrorCode::exit_code`]
//!   matches the process status, so the exit-code table keeps one source;
//! - diagnostics on stderr only, and nothing on stdout but the envelope.
//!
//! Everything runs in a temporary AgenTuna home with the credential store switched to
//! memory, so no case reads or writes the developer's files, Keychain or network.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output, Stdio};

use at_cli::{ErrorCode, SCHEMA_VERSION, exit_code};
use at_db::Database;
use at_model::{AgentId, ItemId, ItemKind, ItemState, Scope, SyncLabel};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath};
use at_store::skill_package::SkillPackage;
use clap::Command;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use support::Sandbox;

// ---------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------

/// One command in the production tree, as clap declares it.
struct Node {
    /// Space-separated path below `agentuna`, for example `mcp secret set`.
    path: String,
    /// Every long flag the command declares, including the propagated global flags.
    long_flags: Vec<String>,
    /// Value names of the required positionals.
    required_positionals: Vec<String>,
    /// Whether the command declares subcommands of its own.
    has_subcommands: bool,
    /// Whether clap insists on a subcommand (a group) rather than accepting a bare run.
    subcommand_required: bool,
}

impl Node {
    /// Whether the path can be invoked without a subcommand.
    fn is_runnable(&self) -> bool {
        !self.has_subcommands || !self.subcommand_required
    }

    /// The path split into the argv words that select it.
    fn words(&self) -> Vec<String> {
        self.path.split(' ').map(str::to_owned).collect()
    }
}

/// Walks the production registry and returns every node below the root.
fn tree() -> Vec<Node> {
    let registry = at_cli::production_registry().expect("the production registry builds");
    let mut root = registry.command();
    root.build();
    let mut nodes = Vec::new();
    collect(&root, "", &mut nodes);
    assert!(!nodes.is_empty(), "the production tree is empty");
    nodes
}

/// Recursively records `command`'s children, skipping clap's generated `help`.
fn collect(command: &Command, prefix: &str, nodes: &mut Vec<Node>) {
    for subcommand in command.get_subcommands() {
        let name = subcommand.get_name();
        if name == "help" {
            continue;
        }
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix} {name}")
        };
        let mut long_flags = Vec::new();
        let mut required_positionals = Vec::new();
        for argument in subcommand.get_arguments() {
            if let Some(long) = argument.get_long() {
                long_flags.push(format!("--{long}"));
            }
            if argument.is_positional()
                && argument.is_required_set()
                && let Some(value_name) = argument
                    .get_value_names()
                    .and_then(|names| names.first())
                    .map(ToString::to_string)
            {
                required_positionals.push(value_name);
            }
        }
        let has_subcommands = subcommand
            .get_subcommands()
            .any(|child| child.get_name() != "help");
        nodes.push(Node {
            path: path.clone(),
            long_flags,
            required_positionals,
            has_subcommands,
            subcommand_required: subcommand.is_subcommand_required_set(),
        });
        collect(subcommand, &path, nodes);
    }
}

/// Leaves that must appear in the success or refusal table.
fn leaves() -> Vec<Node> {
    tree().into_iter().filter(Node::is_runnable).collect()
}

// ---------------------------------------------------------------------------------------
// Running the binary
// ---------------------------------------------------------------------------------------

/// Runs the shipped binary in `sandbox` and captures both streams.
fn run(sandbox: &Sandbox, argv: &[String], stdin: Option<&[u8]>) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_agentuna"));
    command
        .args(argv)
        .env("NO_COLOR", "1")
        .env("AGENTUNA_SECRET_STORE", "memory")
        .envs(sandbox.vars())
        .current_dir(sandbox.cwd());
    match stdin {
        Some(bytes) => {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn the agentuna binary");
            if let Some(mut pipe) = child.stdin.take() {
                pipe.write_all(bytes).expect("write the fixture stdin");
            }
            child
                .wait_with_output()
                .expect("collect the agentuna output")
        }
        None => command
            .stdin(Stdio::null())
            .output()
            .expect("run the agentuna binary"),
    }
}

/// UTF-8 stdout of a finished run.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// UTF-8 stderr of a finished run.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Exit status of a finished run.
fn exit_code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("the process exited with a code")
}

/// Parses stdout, requiring exactly one JSON document and no ANSI escapes.
fn one_json_document(output: &Output, argv: &[String]) -> Value {
    let text = stdout(output);
    assert!(
        !text.contains('\u{1b}'),
        "stdout must carry no ANSI escape: argv={argv:?} stdout={text:?}"
    );
    assert!(
        !stderr(output).contains('\u{1b}'),
        "NO_COLOR is set, so stderr must carry no ANSI escape: argv={argv:?}"
    );
    let mut stream = serde_json::Deserializer::from_str(&text).into_iter::<Value>();
    let first = stream
        .next()
        .unwrap_or_else(|| panic!("stdout held no JSON document: argv={argv:?} text={text:?}"))
        .unwrap_or_else(|error| {
            panic!(
                "stdout is not JSON ({error}): argv={argv:?} text={text:?} stderr={}",
                stderr(output)
            )
        });
    assert!(
        stream.next().is_none(),
        "JSON mode writes exactly one document: argv={argv:?} text={text:?}"
    );
    first
}

/// The published error class for a wire code, so the table stays the single source.
fn published_code(wire: &str) -> ErrorCode {
    ErrorCode::ALL
        .iter()
        .copied()
        .find(|code| code.as_str() == wire)
        .unwrap_or_else(|| panic!("`{wire}` is not a published error code"))
}

/// Asserts the version-1 envelope and the exit-code mapping for one run.
fn assert_contract(output: &Output, argv: &[String], path: &str, expected: i32) {
    let status = exit_code(output);
    assert_eq!(
        status,
        expected,
        "argv={argv:?} must exit {expected}; stdout={} stderr={}",
        stdout(output),
        stderr(output)
    );
    assert!(
        exit_code::is_documented(status),
        "argv={argv:?} exited with the undocumented code {status}"
    );

    let envelope = one_json_document(output, argv);
    let object = envelope
        .as_object()
        .unwrap_or_else(|| panic!("an envelope is a JSON object: {envelope}"));
    for field in at_cli::ENVELOPE_FIELDS {
        assert!(
            object.contains_key(*field),
            "the envelope lost `{field}`: argv={argv:?} envelope={envelope}"
        );
    }
    assert_eq!(
        envelope["schema_version"],
        json!(SCHEMA_VERSION),
        "argv={argv:?}"
    );
    assert!(
        envelope["command"].is_string(),
        "`command` is a string: argv={argv:?} envelope={envelope}"
    );
    assert_eq!(
        envelope["command"],
        json!(path),
        "the envelope names the path that was typed: argv={argv:?}"
    );
    assert_eq!(
        envelope["ok"],
        json!(status == exit_code::SUCCESS),
        "`ok` agrees with the exit status: argv={argv:?} envelope={envelope}"
    );

    let warnings = envelope["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("`warnings` is an array: argv={argv:?} envelope={envelope}"));
    for warning in warnings {
        assert!(
            warning["code"].is_string() && warning["message"].is_string(),
            "a warning carries a code and a message: argv={argv:?} warning={warning}"
        );
    }

    if status == exit_code::SUCCESS {
        assert!(
            envelope["error"].is_null(),
            "a successful run carries a null error: argv={argv:?} envelope={envelope}"
        );
        return;
    }

    let error = envelope["error"]
        .as_object()
        .unwrap_or_else(|| panic!("a failed run carries an error object: argv={argv:?}"));
    for field in at_cli::ERROR_FIELDS {
        assert!(
            error.contains_key(*field),
            "the error object lost `{field}`: argv={argv:?} envelope={envelope}"
        );
    }
    let wire = error["code"]
        .as_str()
        .unwrap_or_else(|| panic!("`error.code` is a string: argv={argv:?}"));
    let code = published_code(wire);
    assert_eq!(
        code.exit_code(),
        status,
        "the published table maps `{wire}` to {} but argv={argv:?} exited {status}",
        code.exit_code()
    );
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "`error.message` is non-empty: argv={argv:?} envelope={envelope}"
    );
    assert!(
        error["hint"]
            .as_str()
            .is_some_and(|hint| !hint.trim().is_empty()),
        "`error.hint` is non-empty: argv={argv:?} envelope={envelope}"
    );
    assert!(
        !stderr(output).is_empty(),
        "a failure writes a diagnostic to stderr: argv={argv:?}"
    );
    assert!(
        !stdout(output).contains("error:"),
        "stdout carries the envelope only: argv={argv:?} stdout={}",
        stdout(output)
    );
}

/// Asserts one invocation and returns its envelope.
fn invoke(
    sandbox: &Sandbox,
    path: &str,
    args: &[String],
    stdin: Option<&[u8]>,
    expected: i32,
) -> Value {
    let mut argv = path.split(' ').map(str::to_owned).collect::<Vec<_>>();
    argv.push("--json".to_owned());
    argv.extend(args.iter().cloned());
    let output = run(sandbox, &argv, stdin);
    assert_contract(&output, &argv, path, expected);
    one_json_document(&output, &argv)
}

// ---------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------

const SKILL_DOC: &str =
    "---\nname: matrix-skill\ndescription: Matrix fixture skill\n---\n\n# Matrix skill\n";
const INSTALL_DOC: &str =
    "---\nname: installed-skill\ndescription: Installed by the matrix\n---\n\n# Installed\n";
const NATIVE_MIGRATE_DOC: &str =
    "---\nname: native-migrate\ndescription: Native item the matrix migrates\n---\n\n# Migrate\n";
const NATIVE_ADOPT_DOC: &str =
    "---\nname: native-adopt\ndescription: Native item the matrix adopts\n---\n\n# Adopt\n";
const ALPHA_DOC: &str = "---\nname: alpha\ndescription: Preset member\n---\n\n# Alpha\n";
const BETA_DOC: &str = "---\nname: beta\ndescription: Preset member\n---\n\n# Beta\n";
const STORE_DOC: &str = "# store version\n";
const DISK_DOC: &str = "# edited on disk\n";
const MCP_EDIT_INPUT: &str = "{\"id\":\"missing-server\",\"definition\":{\"display_name\":\"x\",\"description\":null,\"transport\":{\"type\":\"stdio\",\"command\":\"x\",\"args\":[],\"cwd\":null,\"env\":{}}}}";

/// A populated sandbox every case can be resolved against.
struct Fixture {
    sandbox: Sandbox,
    project: PathBuf,
    extra_project: PathBuf,
    skill_source: PathBuf,
    install_source: PathBuf,
    native_migrate: PathBuf,
    native_adopt: PathBuf,
    home_rule: PathBuf,
    project_rule: PathBuf,
    mcp_input: String,
    preset: String,
    live_item: String,
    drift_adopt: String,
    drift_restore: String,
}

impl Fixture {
    /// Builds the fixture: projects, native items, a preset, drift events, a sync baseline
    /// and one MCP server definition to add.
    fn build() -> Self {
        let sandbox = Sandbox::new();
        sandbox.healthy();
        let home = sandbox.path("home");
        fs::create_dir_all(home.join(".claude/skills")).expect("agent skill root");
        // `rules new --scope user --type AGENTS.md` writes below the Codex home, so the
        // registered root must exist exactly as it does on a machine with Codex installed.
        fs::create_dir_all(home.join(".codex")).expect("codex home");
        fs::write(home.join("AGENTS.md"), "# Home rules\n").expect("home rule");

        let project = sandbox.path("home/Projects/matrix-project");
        fs::create_dir_all(&project).expect("project root");
        fs::write(project.join("AGENTS.md"), "# Project rules\n").expect("project rule");
        init_store(Scope::Project, project.join(".agentuna")).expect("project store");

        let extra_project = sandbox.path("home/Projects/extra-project");
        fs::create_dir_all(&extra_project).expect("extra project root");

        let skill_source = sandbox.path("fixtures/matrix-skill");
        write_skill_folder(&skill_source, SKILL_DOC);
        let install_source = sandbox.path("fixtures/installed-skill");
        write_skill_folder(&install_source, INSTALL_DOC);
        let native_migrate = home.join(".claude/skills/native-migrate");
        write_skill_folder(&native_migrate, NATIVE_MIGRATE_DOC);
        let native_adopt = home.join(".claude/skills/native-adopt");
        write_skill_folder(&native_adopt, NATIVE_ADOPT_DOC);

        let user_store = StoreLayout::new(Scope::User, sandbox.user_store());
        init_store(Scope::User, user_store.root()).expect("user store");
        let alpha = seed_stored_skill(&sandbox, &user_store, "alpha", ALPHA_DOC);
        let beta = seed_stored_skill(&sandbox, &user_store, "beta", BETA_DOC);
        let preset = {
            let mut database =
                Database::open_file(sandbox.paths().db_path()).expect("open the index");
            at_engine::presets::PresetService::new(&mut database)
                .create(
                    "daily",
                    Some("Daily skills"),
                    Scope::User,
                    &[(alpha, ItemKind::Skill), (beta, ItemKind::Skill)],
                )
                .expect("seed the preset")
        };
        let live_item = alpha.to_string();

        let drift_adopt = seed_drift(&sandbox, &user_store, "drift-adopt");
        let drift_restore = seed_drift(&sandbox, &user_store, "drift-restore");
        seed_sync(&sandbox, &project, &user_store);

        let server = fake_server(&sandbox);
        let mcp_input = json!({
            "id": "matrix-server",
            "definition": {
                "display_name": "Matrix Server",
                "description": null,
                "transport": {
                    "type": "stdio",
                    "command": server.display().to_string(),
                    "args": [],
                    "cwd": null,
                    "env": {}
                }
            }
        })
        .to_string();
        configure_remote(&sandbox);
        let project_rule = project.join("AGENTS.md");

        Self {
            sandbox,
            project,
            extra_project,
            skill_source,
            install_source,
            native_migrate,
            native_adopt,
            home_rule: home.join("AGENTS.md"),
            project_rule,
            mcp_input,
            preset,
            live_item,
            drift_adopt,
            drift_restore,
        }
    }

    /// Values the case tables refer to as `{{name}}`.
    fn tokens(&self) -> BTreeMap<String, String> {
        let mut tokens = BTreeMap::new();
        for (name, path) in [
            ("project", &self.project),
            ("extra_project", &self.extra_project),
            ("skill_source", &self.skill_source),
            ("install_source", &self.install_source),
            ("native_migrate", &self.native_migrate),
            ("native_adopt", &self.native_adopt),
            ("home_rule", &self.home_rule),
            ("project_rule", &self.project_rule),
        ] {
            tokens.insert(name.to_owned(), text(path));
        }
        tokens.insert("mcp_input".to_owned(), self.mcp_input.clone());
        tokens.insert("preset".to_owned(), self.preset.clone());
        tokens.insert("live_item".to_owned(), self.live_item.clone());
        tokens.insert("drift_adopt".to_owned(), self.drift_adopt.clone());
        tokens.insert("drift_restore".to_owned(), self.drift_restore.clone());
        tokens
    }
}

/// Writes a skill folder with one `SKILL.md`.
fn write_skill_folder(folder: &Path, skill_md: &str) {
    fs::create_dir_all(folder).expect("create the skill folder");
    fs::write(folder.join("SKILL.md"), skill_md).expect("write SKILL.md");
}

/// Records one stored skill in the user store manifest and the item index.
///
/// The preset service resolves members against `items`, so a manifest entry alone would be
/// reported as a deleted member.
fn seed_stored_skill(
    sandbox: &Sandbox,
    store: &StoreLayout,
    slug_text: &str,
    skill_md: &str,
) -> ItemId {
    let slug = Slug::parse(slug_text).expect("valid slug");
    let folder = store.skill_dir(&slug);
    write_skill_folder(&folder, skill_md);
    let skill = SkillPackage::read(&folder).expect("read the package");
    let hash = skill.hash.to_string();
    let mut manifest = StoreManifest::load(store)
        .expect("load the manifest")
        .manifest;
    let item_id = ItemId::new();
    manifest
        .insert_item(ManifestItem::new(
            item_id,
            ItemKind::Skill,
            slug,
            StorePath::parse(&format!("skills/{slug_text}")).expect("store path"),
            skill.hash,
        ))
        .expect("manifest item");
    fs::write(
        store.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write the manifest");

    let source = text(&folder);
    let mut database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let connection = database.connection_mut();
    let transaction = connection
        .transaction()
        .expect("begin the item transaction");
    at_db::items::upsert_adopted_item(
        &transaction,
        &at_db::items::AdoptedItemSpec {
            proposed_id: item_id,
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: slug_text,
            name: slug_text,
            source_path: &source,
            content_hash: Some(hash.as_str()),
        },
    )
    .expect("record the item");
    transaction.commit().expect("commit the item");
    item_id
}

/// Seeds a user-scope skill whose deployment drifted from the store copy.
fn seed_drift(sandbox: &Sandbox, user_store: &StoreLayout, slug_text: &str) -> String {
    let slug = Slug::parse(slug_text).expect("valid slug");
    let store_skill = user_store.skill_dir(&slug);
    let disk_skill = sandbox.path("home/.claude/skills").join(slug_text);
    write_skill_folder(&store_skill, STORE_DOC);
    write_skill_folder(&disk_skill, DISK_DOC);
    let store_hash = at_store::hash_skill_folder(&store_skill)
        .expect("hash the store skill")
        .to_string();
    let disk_hash = at_store::hash_skill_folder(&disk_skill)
        .expect("hash the disk skill")
        .to_string();
    let store_text = text(&store_skill);
    let disk_text = text(&disk_skill);

    let mut database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let connection = database.connection_mut();
    at_db::agents::upsert_agent(
        connection,
        &at_model::Agent {
            id: AgentId::ClaudeCode,
            display_name: "Claude Code".to_owned(),
            detected: true,
            version: None,
        },
    )
    .expect("record the agent");
    let item_id = {
        let transaction = connection
            .transaction()
            .expect("begin the item transaction");
        let item_id = at_db::items::upsert_adopted_item(
            &transaction,
            &at_db::items::AdoptedItemSpec {
                proposed_id: ItemId::new(),
                kind: ItemKind::Skill,
                scope: Scope::User,
                project_id: None,
                slug: slug_text,
                name: slug_text,
                source_path: &store_text,
                content_hash: Some(store_hash.as_str()),
            },
        )
        .expect("record the item");
        transaction.commit().expect("commit the item");
        item_id
    };
    at_db::deployments::upsert_deployment(
        connection,
        &at_db::deployments::DeploymentSpec::applied(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
            Some("copy"),
            Some(&disk_text),
            at_db::deployments::DeploymentHashes::in_sync(&store_hash),
        ),
    )
    .expect("record the deployment");
    at_db::versions::record_skill_version(
        connection,
        &at_db::versions::NewVersion::new(&item_id, Scope::User, &store_hash),
    )
    .expect("record the store version");
    let item_text = item_id.to_string();
    at_db::drift::record_drift(
        connection,
        &at_db::drift::NewDriftEvent::new(&AgentId::ClaudeCode, Scope::User, &disk_text)
            .for_item(&item_text)
            .with_hashes(Some(&store_hash), Some(&disk_hash)),
    )
    .expect("record the drift event")
    .to_string()
}

/// Seeds two project skills with a sync baseline: one the Project side wins, one the
/// library side wins, so both `sync push` and `sync pull` have a permitted direction.
fn seed_sync(sandbox: &Sandbox, project: &Path, user_store: &StoreLayout) {
    let project_store = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    let project_id = {
        let database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
        at_db::projects::add_project(database.connection(), project, Some("matrix"), None)
            .expect("register the project")
            .id
    };
    {
        let database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
        database
            .connection()
            .execute(
                "INSERT OR IGNORE INTO agents (id, display_name, kind, version, installed) \
                 VALUES ('claude_code', 'Claude Code', 'claude_code', '2.0.0', 1)",
                [],
            )
            .expect("agent row");
    }
    for (slug_text, label) in [
        ("sync-skill", SyncLabel::ProjectNewer),
        ("sync-library-skill", SyncLabel::LibraryNewer),
    ] {
        seed_sync_item(
            sandbox,
            &project_store,
            user_store,
            &project_id,
            slug_text,
            label,
        );
    }
}

/// Records one skill in both stores with the project deployment row that carries the label.
fn seed_sync_item(
    sandbox: &Sandbox,
    project_store: &StoreLayout,
    user_store: &StoreLayout,
    project_id: &str,
    slug_text: &str,
    label: SyncLabel,
) {
    let item_id = ItemId::new();
    let project_hash = seed_manifest_skill(project_store, slug_text, "# project copy\n", item_id);
    let library_hash = seed_manifest_skill(user_store, slug_text, "# library copy\n", item_id);
    let baseline = if matches!(label, SyncLabel::LibraryNewer) {
        &library_hash
    } else {
        &project_hash
    };
    let target_text = text(&project_store.skill_dir(&Slug::parse(slug_text).expect("slug")));

    let database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
             VALUES (?1, 'skill', 'project', ?2, ?3, ?3, 'present', 1)",
            [
                item_id.to_string(),
                project_id.to_owned(),
                slug_text.to_owned(),
            ],
        )
        .expect("item row");
    at_db::deployments::upsert_deployment(
        connection,
        &at_db::deployments::DeploymentSpec::new(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::Project,
            ItemState::Linked,
        )
        .with_project(project_id)
        .with_mode("native")
        .with_target_path(&target_text)
        .with_hashes(Some(&project_hash), Some(&library_hash), Some(baseline))
        .with_sync_label(label),
    )
    .expect("deployment row");
}

/// Writes one manifest-tracked stored skill and returns its package hash.
fn seed_manifest_skill(
    store: &StoreLayout,
    slug_text: &str,
    skill_md: &str,
    item_id: ItemId,
) -> String {
    let slug = Slug::parse(slug_text).expect("valid slug");
    let folder = store.skill_dir(&slug);
    write_skill_folder(&folder, skill_md);
    let skill = SkillPackage::read(&folder).expect("read the package");
    let hash = skill.hash.to_string();
    let mut manifest = StoreManifest::load(store)
        .expect("load the manifest")
        .manifest;
    manifest
        .insert_item(ManifestItem::new(
            item_id,
            ItemKind::Skill,
            slug,
            StorePath::parse(&format!("skills/{slug_text}")).expect("store path"),
            skill.hash,
        ))
        .expect("manifest item");
    fs::write(
        store.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write the manifest");
    hash
}

/// Configures a remote URL so the credential check refuses before any network call.
fn configure_remote(sandbox: &Sandbox) {
    let database = Database::open_file(sandbox.paths().db_path()).expect("open the index");
    let mut state =
        at_db::remote_backup::remote_backup_state(database.connection()).expect("remote state");
    state.repo_url = Some("https://example.invalid/agentuna.git".to_owned());
    at_db::remote_backup::save_remote_backup_state(database.connection(), &state)
        .expect("save the remote state");
}

/// A stdio MCP server that answers `initialize` and `tools/list`, so `mcp test` runs offline.
fn fake_server(sandbox: &Sandbox) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = sandbox.path("fixtures/fake-mcp-server.sh");
    fs::create_dir_all(path.parent().expect("fixture directory")).expect("fixture directory");
    let script = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"matrix-fixture","version":"1.0.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
  esac
done
"#;
    fs::write(&path, script).expect("write the fake server");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod the fake server");
    path
}

/// UTF-8 text of a path.
fn text(path: &Path) -> String {
    path.to_str().expect("sandbox paths are UTF-8").to_owned()
}

// ---------------------------------------------------------------------------------------
// Token substitution
// ---------------------------------------------------------------------------------------

/// Replaces every `{{name}}` in `template`, panicking on a name the fixture does not define.
fn substitute(tokens: &BTreeMap<String, String>, template: &str) -> String {
    let mut resolved = template.to_owned();
    while let Some(start) = resolved.find("{{") {
        let end = resolved[start..]
            .find("}}")
            .map(|offset| start + offset)
            .unwrap_or_else(|| panic!("unterminated token in {template:?}"));
        let name = &resolved[start + 2..end];
        let value = tokens
            .get(name)
            .unwrap_or_else(|| panic!("unknown token `{{{{{name}}}}}` in {template:?}"));
        resolved.replace_range(start..end + 2, value);
    }
    resolved
}

// ---------------------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------------------

/// One invocation in a case table.
struct Case {
    /// The path that must appear in the envelope, for example `skills add`.
    path: &'static str,
    /// Arguments after `--json`, with `{{token}}` placeholders.
    args: Vec<String>,
    /// Optional stdin body, with `{{token}}` placeholders.
    stdin: Option<&'static str>,
    /// Expected process exit status.
    expected: i32,
    /// Optional `(token, JSON pointer)` capture the later cases reuse.
    capture: Option<(&'static str, &'static str)>,
}

impl Case {
    /// A case that must succeed.
    fn ok(path: &'static str, args: &[&str]) -> Self {
        Self::refusing(path, args, exit_code::SUCCESS)
    }

    /// A case that must exit with `expected`.
    fn refusing(path: &'static str, args: &[&str], expected: i32) -> Self {
        Self {
            path,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdin: None,
            expected,
            capture: None,
        }
    }

    /// Attaches a stdin body.
    fn stdin(mut self, body: &'static str) -> Self {
        self.stdin = Some(body);
        self
    }

    /// Attaches a capture.
    fn capture(mut self, token: &'static str, pointer: &'static str) -> Self {
        self.capture = Some((token, pointer));
        self
    }
}

/// Runs one case and records any capture it declares.
fn run_case(fixture: &Fixture, tokens: &mut BTreeMap<String, String>, case: &Case) {
    let args = case
        .args
        .iter()
        .map(|arg| substitute(tokens, arg))
        .collect::<Vec<_>>();
    let stdin = case.stdin.map(|body| substitute(tokens, body));
    let envelope = invoke(
        &fixture.sandbox,
        case.path,
        &args,
        stdin.as_deref().map(str::as_bytes),
        case.expected,
    );
    if let Some((token, pointer)) = case.capture {
        let value = envelope.pointer(pointer).unwrap_or_else(|| {
            panic!(
                "`{}` did not expose {pointer} for token `{token}`: {envelope}",
                case.path
            )
        });
        let rendered = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned);
        tokens.insert(token.to_owned(), rendered);
    }
}

// ---------------------------------------------------------------------------------------
// The success table
// ---------------------------------------------------------------------------------------

#[test]
fn every_additive_command_succeeds_on_a_populated_fixture() {
    let fixture = Fixture::build();
    let mut tokens = fixture.tokens();
    for case in additive_cases() {
        run_case(&fixture, &mut tokens, &case);
    }
}

/// The success table, ordered so every case sees the state it needs.
fn additive_cases() -> Vec<Case> {
    vec![
        Case::ok("projects add", &["{{extra_project}}"]),
        Case::ok("projects list", &[]),
        Case::ok("projects use", &["{{extra_project}}"]),
        Case::ok("scan", &[]),
        Case::ok("doctor", &[]),
        Case::ok("doctor", &["--fix"]),
        Case::ok("config", &[]),
        Case::ok("version", &[]),
        Case::ok("completions", &["bash"]),
        Case::ok("completions", &["zsh"]),
        Case::ok("completions", &["fish"]),
        Case::ok("completions", &["elvish"]),
        Case::ok("completions", &["powershell"]),
        Case::ok("agents list", &[]),
        Case::ok("agents inspect", &["claude_code"]),
        Case::ok("skills install", &["{{install_source}}"]),
        Case::ok("skills add", &["--from", "{{skill_source}}"]),
        Case::ok("skills list", &[]),
        Case::ok("skills show", &["matrix-skill"]),
        Case::ok("skills enable", &["matrix-skill", "--agent", "claude_code"]),
        Case::ok("skills deploy", &["matrix-skill", "--agent", "claude_code"]),
        Case::ok(
            "skills disable",
            &["matrix-skill", "--agent", "claude_code"],
        ),
        Case::ok("skills preset list", &[]),
        Case::ok(
            "skills preset apply",
            &["{{preset}}", "--agent", "claude_code"],
        ),
        Case::ok("mcp add", &[]).stdin("{{mcp_input}}"),
        Case::ok("mcp list", &[]),
        Case::ok("mcp show", &["matrix-server"]),
        Case::ok("mcp test", &["matrix-server"]),
        Case::ok("mcp enable", &["matrix-server", "--agent", "claude_code"]),
        Case::ok("mcp disable", &["matrix-server", "--agent", "claude_code"]),
        Case::ok(
            "mcp copy",
            &[
                "matrix-server",
                "--to",
                "project",
                "--yes",
                "--project",
                "{{project}}",
            ],
        ),
        Case::ok("mcp secret list", &[]),
        Case::ok("mcp secret set", &["MATRIX_TOKEN", "--dry-run"]).stdin("matrix-value"),
        Case::ok("mcp secret rm", &["MATRIX_TOKEN", "--force", "--dry-run"]),
        Case::ok("migrate scan", &[]),
        Case::ok("migrate plan", &["--select", "{{native_migrate}}"])
            .capture("plan", "/data/plan_id"),
        Case::ok("migrate apply", &["--plan", "{{plan}}", "--yes"])
            .capture("operation", "/data/operation_id"),
        Case::ok("migrate status", &[]),
        Case::ok("adopt", &["{{native_adopt}}", "--yes"]),
        Case::ok("drift list", &[]),
        Case::ok("drift adopt", &["{{drift_adopt}}", "--yes"]),
        Case::ok("drift restore", &["{{drift_restore}}", "--yes"]),
        Case::ok("backups list", &[]),
        Case::ok("sync status", &["--project", "{{project}}"]),
        Case::ok(
            "sync push",
            &["sync-skill", "--project", "{{project}}", "--yes"],
        ),
        Case::ok(
            "sync pull",
            &["sync-library-skill", "--project", "{{project}}", "--yes"],
        ),
        Case::ok("remote status", &[]),
        Case::ok("effective", &["--scope", "user"]),
        // Without --scope the command answers for the discovered project or User and says
        // so in `data.scope`; a test sandbox has no project, so User answers.
        Case::ok("effective", &[]),
        Case::ok(
            "effective lock",
            &["{{live_item}}", "--reason", "matrix", "--scope", "global"],
        ),
        Case::ok("effective unlock", &["{{live_item}}", "--scope", "global"]),
        Case::ok("why", &["{{live_item}}", "--scope", "user"]),
        Case::ok("overview", &["--scope", "user"]),
        Case::ok("rules list", &["--scope", "user"]),
        Case::ok("rules show", &["{{home_rule}}", "--scope", "user"]),
        Case::ok("rules new", &["--scope", "user", "--type", "AGENTS.md"]),
        Case::ok(
            "rules save",
            &[
                "{{project_rule}}",
                "--scope",
                "project",
                "--project",
                "{{project}}",
                "--stdin",
            ],
        )
        .stdin("# Project rules\n"),
    ]
}

#[test]
fn every_reverting_command_succeeds_on_a_populated_fixture() {
    // Each case destroys state a later case would need, so each gets its own fixture and
    // runs its own setup first.
    let cases: Vec<(Vec<Case>, Case)> = vec![
        (
            vec![Case::ok("skills add", &["--from", "{{skill_source}}"])],
            Case::ok("skills remove", &["matrix-skill", "--yes"]),
        ),
        (
            vec![
                Case::ok("mcp add", &[]).stdin("{{mcp_input}}"),
                // Removing a definition that was never deployed has no native target to
                // delete; the shipped command reports that as not_found.
                Case::ok("mcp enable", &["matrix-server", "--agent", "claude_code"]),
            ],
            Case::ok("mcp remove", &["matrix-server", "--yes"]),
        ),
        (
            vec![
                Case::ok("migrate plan", &["--select", "{{native_migrate}}"])
                    .capture("plan", "/data/plan_id"),
                Case::ok("migrate apply", &["--plan", "{{plan}}", "--yes"])
                    .capture("operation", "/data/operation_id"),
            ],
            Case::ok("migrate undo", &["{{operation}}", "--yes"]),
        ),
        (
            vec![
                Case::ok("migrate plan", &["--select", "{{native_migrate}}"])
                    .capture("plan", "/data/plan_id"),
                Case::ok("migrate apply", &["--plan", "{{plan}}", "--yes"]),
                Case::ok("backups list", &[]).capture("backup", "/data/backups/0/id"),
            ],
            Case::ok("backups restore", &["{{backup}}", "--yes"]),
        ),
        (
            vec![Case::ok("projects add", &["{{extra_project}}"])],
            Case::ok("projects remove", &["{{extra_project}}"]),
        ),
    ];
    assert_eq!(cases.len(), 5, "every reverting command needs a case");
    for (setup, case) in cases {
        let fixture = Fixture::build();
        let mut tokens = fixture.tokens();
        for step in &setup {
            run_case(&fixture, &mut tokens, step);
        }
        run_case(&fixture, &mut tokens, &case);
    }
}

// ---------------------------------------------------------------------------------------
// Usage errors
// ---------------------------------------------------------------------------------------

#[test]
fn every_runnable_path_answers_a_usage_error_with_the_usage_exit_code() {
    let fixture = Fixture::build();
    let mut checked = 0_usize;
    for node in leaves() {
        // An unknown flag is a usage error for every command, whatever its arguments.
        let mut argv = vec!["--json".to_owned()];
        argv.extend(node.words());
        argv.push("--definitely-not-a-flag".to_owned());
        let output = run(&fixture.sandbox, &argv, None);
        assert_contract(&output, &argv, &node.path, exit_code::USAGE);
        checked += 1;

        // A missing required positional is the same class of failure.
        if !node.required_positionals.is_empty() {
            let mut argv = vec!["--json".to_owned()];
            argv.extend(node.words());
            let output = run(&fixture.sandbox, &argv, None);
            assert_contract(&output, &argv, &node.path, exit_code::USAGE);
            checked += 1;
        }
    }
    // A group that declares subcommands must refuse a bare run the same way, instead of
    // reaching its dispatcher and reporting an internal error.
    let mut groups = 0_usize;
    for node in tree() {
        if !(node.has_subcommands && node.subcommand_required) {
            continue;
        }
        let mut argv = vec!["--json".to_owned()];
        argv.extend(node.words());
        let output = run(&fixture.sandbox, &argv, None);
        assert_contract(&output, &argv, &node.path, exit_code::USAGE);
        groups += 1;
    }
    assert!(groups > 0, "the tree declares no subcommand-only group");
    assert!(checked >= leaves().len(), "the usage matrix skipped paths");
}

// ---------------------------------------------------------------------------------------
// Typed refusals
// ---------------------------------------------------------------------------------------

#[test]
fn every_typed_refusal_uses_its_published_code() {
    let fixture = Fixture::build();
    let mut tokens = fixture.tokens();
    // The refusal table reads the same populated fixture the success table does.
    run_case(
        &fixture,
        &mut tokens,
        &Case::ok("skills add", &["--from", "{{skill_source}}"]),
    );
    run_case(
        &fixture,
        &mut tokens,
        &Case::ok("mcp add", &[]).stdin("{{mcp_input}}"),
    );
    for case in refusal_cases() {
        run_case(&fixture, &mut tokens, &case);
    }

    // A reviewed plan whose source changed after review is a stale plan, not a bad argument.
    run_case(
        &fixture,
        &mut tokens,
        &Case::ok("migrate plan", &["--select", "{{native_migrate}}"])
            .capture("plan", "/data/plan_id"),
    );
    fs::write(
        fixture.native_migrate.join("SKILL.md"),
        "# rewritten after the plan was reviewed\n",
    )
    .expect("rewrite the reviewed source");
    let envelope = invoke(
        &fixture.sandbox,
        "migrate apply",
        &[
            "--plan".to_owned(),
            tokens["plan"].clone(),
            "--yes".to_owned(),
        ],
        None,
        exit_code::CONFLICT,
    );
    assert_eq!(
        envelope["error"]["code"],
        json!("stale_plan"),
        "a source change after review is the typed stale-plan refusal: {envelope}"
    );
}

/// The typed-refusal table: not found, validation, conflict, locked and network.
fn refusal_cases() -> Vec<Case> {
    vec![
        Case::refusing("skills show", &["missing-skill"], exit_code::NOT_FOUND),
        Case::refusing(
            "skills enable",
            &["missing-skill", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "skills disable",
            &["missing-skill", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "skills deploy",
            &["missing-skill", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "skills remove",
            &["missing-skill", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "skills preset apply",
            &["missing-preset", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "skills enable",
            &["matrix-skill", "--agent", "not-an-agent"],
            exit_code::VALIDATION,
        ),
        Case::refusing("mcp show", &["missing-server"], exit_code::NOT_FOUND),
        Case::refusing("mcp edit", &["missing-server"], exit_code::NOT_FOUND).stdin(MCP_EDIT_INPUT),
        Case::refusing("mcp test", &["missing-server"], exit_code::NOT_FOUND),
        Case::refusing(
            "mcp enable",
            &["missing-server", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "mcp disable",
            &["missing-server", "--agent", "claude_code"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "mcp copy",
            &[
                "missing-server",
                "--to",
                "project",
                "--yes",
                "--project",
                "{{project}}",
            ],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "mcp remove",
            &["missing-server", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing("mcp add", &[], exit_code::VALIDATION).stdin("not json"),
        Case::refusing("mcp add", &[], exit_code::CONFLICT).stdin("{{mcp_input}}"),
        // The Global store is administrator-owned on macOS: an unprivileged CLI cannot write it.
        Case::refusing(
            "mcp copy",
            &["matrix-server", "--to", "global", "--yes"],
            exit_code::LOCKED,
        ),
        Case::refusing("mcp secret set", &["MATRIX_TOKEN"], exit_code::VALIDATION),
        // Without --plan the refusal stays a usage error and names the persisted ids.
        Case::refusing("migrate apply", &[], exit_code::USAGE),
        Case::refusing(
            "migrate apply",
            &["--plan", "missing-plan", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "migrate undo",
            &["missing-operation", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "migrate plan",
            &["--select", "/missing/native/path"],
            exit_code::VALIDATION,
        ),
        Case::refusing(
            "adopt",
            &["/missing/native/path", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing("drift adopt", &["999", "--yes"], exit_code::NOT_FOUND),
        Case::refusing("drift restore", &["999", "--yes"], exit_code::NOT_FOUND),
        Case::refusing(
            "backups restore",
            &["bkp_missing", "--yes"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing("sync status", &[], exit_code::VALIDATION),
        Case::refusing(
            "sync push",
            &["missing-item", "--project", "{{project}}", "--yes"],
            exit_code::NOT_FOUND,
        ),
        // The memory credential store is empty, so the remote refuses before any network call.
        Case::refusing("remote backup", &["--yes"], exit_code::NETWORK_REQUIRED),
        Case::refusing(
            "remote restore",
            &["0123456789abcdef0123456789abcdef01234567", "--yes"],
            exit_code::NETWORK_REQUIRED,
        ),
        Case::refusing("agents inspect", &["not-an-agent"], exit_code::NOT_FOUND),
        Case::refusing(
            "effective lock",
            &[
                "itm_01a1b4f6-0000-7000-8000-000000000000",
                "--reason",
                "matrix",
                "--scope",
                "global",
            ],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "effective unlock",
            &[
                "itm_01a1b4f6-0000-7000-8000-000000000000",
                "--scope",
                "global",
            ],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "why",
            &[
                "itm_01a1b4f6-0000-7000-8000-000000000000",
                "--scope",
                "user",
            ],
            exit_code::NOT_FOUND,
        ),
        // Names are accepted now; an unknown name is not found rather than a bad id.
        Case::refusing(
            "why",
            &["not-an-item", "--scope", "user"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing("overview", &[], exit_code::VALIDATION),
        Case::refusing("rules list", &[], exit_code::VALIDATION),
        Case::refusing(
            "rules show",
            &["/missing/rule/file.md", "--scope", "user"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "rules save",
            &["/missing/rule/file.md", "--scope", "user", "--stdin"],
            exit_code::NOT_FOUND,
        ),
        Case::refusing(
            "rules new",
            &["--scope", "user", "--type", "BOGUS.md"],
            exit_code::VALIDATION,
        ),
        Case::refusing("projects remove", &["not-a-project"], exit_code::NOT_FOUND),
        Case::refusing("projects use", &["not-a-project"], exit_code::NOT_FOUND),
    ]
}

// ---------------------------------------------------------------------------------------
// Confirmation and read-only gates
// ---------------------------------------------------------------------------------------

#[test]
fn every_destructive_command_without_yes_exits_with_confirmation_required() {
    let fixture = Fixture::build();
    let mut tokens = fixture.tokens();
    // The subjects exist, so the only thing that stops each write is the missing --yes.
    for setup in [
        Case::ok("skills add", &["--from", "{{skill_source}}"]),
        Case::ok("mcp add", &[]).stdin("{{mcp_input}}"),
        Case::ok("migrate plan", &["--select", "{{native_migrate}}"])
            .capture("plan", "/data/plan_id"),
        Case::ok("migrate apply", &["--plan", "{{plan}}", "--yes"])
            .capture("operation", "/data/operation_id"),
        Case::ok("backups list", &[]).capture("backup", "/data/backups/0/id"),
    ] {
        run_case(&fixture, &mut tokens, &setup);
    }

    let cases = destructive_cases();
    for case in &cases {
        // A refused confirmation still logs the run, so the digest covers the stores only.
        let before = tree_hash(&fixture.sandbox.path(""), false);
        run_case(&fixture, &mut tokens, case);
        assert_eq!(
            tree_hash(&fixture.sandbox.path(""), false),
            before,
            "`{}` wrote something before the confirmation gate: {:?}",
            case.path,
            case.args
        );
    }
}

/// Every command that writes files and must refuse without `--yes` or `--force`.
fn destructive_cases() -> Vec<Case> {
    vec![
        Case::refusing(
            "skills remove",
            &["matrix-skill"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "mcp remove",
            &["matrix-server"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "mcp secret rm",
            &["MATRIX_TOKEN"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "mcp copy",
            &[
                "matrix-server",
                "--to",
                "project",
                "--project",
                "{{project}}",
            ],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "migrate apply",
            &["--plan", "{{plan}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "migrate undo",
            &["{{operation}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "adopt",
            &["{{native_adopt}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "drift adopt",
            &["{{drift_adopt}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "drift restore",
            &["{{drift_restore}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "backups restore",
            &["{{backup}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "sync push",
            &["sync-skill", "--project", "{{project}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing(
            "sync pull",
            &["sync-library-skill", "--project", "{{project}}"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
        Case::refusing("remote backup", &[], exit_code::CONFIRMATION_REQUIRED),
        Case::refusing(
            "remote restore",
            &["0123456789abcdef0123456789abcdef01234567"],
            exit_code::CONFIRMATION_REQUIRED,
        ),
    ]
}

#[test]
fn every_mutating_command_refuses_read_only_and_writes_nothing() {
    let fixture = Fixture::build();
    let mut tokens = fixture.tokens();
    // The read-only gate fires before any id lookup, so these placeholders are never read.
    for token in ["plan", "operation", "backup"] {
        tokens.insert(token.to_owned(), "missing".to_owned());
    }
    run_case(
        &fixture,
        &mut tokens,
        &Case::ok("skills add", &["--from", "{{skill_source}}"]),
    );
    run_case(
        &fixture,
        &mut tokens,
        &Case::ok("mcp add", &[]).stdin("{{mcp_input}}"),
    );

    let cases = mutating_cases();
    for case in &cases {
        // `--read-only` also disables file logging, so the digest covers the support
        // directory and its log folder too: a run that writes anything at all fails.
        let before = tree_hash(&fixture.sandbox.path(""), true);
        run_case(&fixture, &mut tokens, case);
        assert_eq!(
            tree_hash(&fixture.sandbox.path(""), true),
            before,
            "`{}` changed the sandbox under --read-only: {:?}",
            case.path,
            case.args
        );
    }
}

/// One `--read-only` invocation per write-shaped command.
fn mutating_cases() -> Vec<Case> {
    let table: &[(&'static str, &[&str])] = &[
        ("projects add", &["{{extra_project}}"]),
        ("projects remove", &["{{extra_project}}"]),
        ("projects use", &["{{extra_project}}"]),
        ("doctor", &["--fix"]),
        ("mcp add", &[]),
        ("mcp edit", &["matrix-server"]),
        ("mcp remove", &["matrix-server", "--force"]),
        ("mcp enable", &["matrix-server", "--agent", "claude_code"]),
        ("mcp disable", &["matrix-server", "--agent", "claude_code"]),
        ("mcp secret set", &["MATRIX_TOKEN"]),
        ("mcp secret rm", &["MATRIX_TOKEN", "--force"]),
        ("skills install", &["{{install_source}}"]),
        ("skills add", &["--from", "{{skill_source}}"]),
        ("skills enable", &["matrix-skill", "--agent", "claude_code"]),
        ("skills deploy", &["matrix-skill", "--agent", "claude_code"]),
        (
            "skills disable",
            &["matrix-skill", "--agent", "claude_code"],
        ),
        ("skills remove", &["matrix-skill", "--yes"]),
        (
            "skills preset apply",
            &["{{preset}}", "--agent", "claude_code"],
        ),
        ("migrate plan", &["--select", "{{native_migrate}}"]),
        ("migrate apply", &["--plan", "{{plan}}", "--yes"]),
        ("migrate undo", &["{{operation}}", "--yes"]),
        ("adopt", &["{{native_adopt}}", "--yes"]),
        ("drift adopt", &["{{drift_adopt}}", "--yes"]),
        ("drift restore", &["{{drift_restore}}", "--yes"]),
        ("backups restore", &["{{backup}}", "--yes"]),
        (
            "sync push",
            &["sync-skill", "--project", "{{project}}", "--yes"],
        ),
        (
            "sync pull",
            &["sync-library-skill", "--project", "{{project}}", "--yes"],
        ),
        ("remote backup", &["--yes"]),
        (
            "remote restore",
            &["0123456789abcdef0123456789abcdef01234567", "--yes"],
        ),
        (
            "effective lock",
            &["{{live_item}}", "--reason", "matrix", "--scope", "global"],
        ),
        ("effective unlock", &["{{live_item}}", "--scope", "global"]),
        (
            "rules save",
            &[
                "{{project_rule}}",
                "--scope",
                "project",
                "--project",
                "{{project}}",
                "--stdin",
            ],
        ),
        ("rules new", &["--scope", "user", "--type", "AGENTS.md"]),
    ];
    table
        .iter()
        .map(|(path, args)| {
            let mut with_read_only = args.to_vec();
            with_read_only.push("--read-only");
            Case::refusing(path, &with_read_only, exit_code::LOCKED)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Help, completions and the documentation commands
// ---------------------------------------------------------------------------------------

#[test]
fn every_path_renders_help_with_every_flag_it_declares() {
    let fixture = Fixture::build();
    let nodes = tree();
    for node in &nodes {
        let mut argv = vec!["--json".to_owned()];
        argv.extend(node.words());
        argv.push("--help".to_owned());
        let output = run(&fixture.sandbox, &argv, None);
        assert_contract(&output, &argv, &node.path, exit_code::SUCCESS);
        let envelope = one_json_document(&output, &argv);
        let help = envelope["data"]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("`{} --help` carries its text in data.text", node.path));
        assert!(
            help.contains("Usage:"),
            "`{} --help` renders a usage line: {help}",
            node.path
        );
        for flag in &node.long_flags {
            assert!(
                help.contains(flag.as_str()),
                "`{} --help` omits the declared flag {flag}: {help}",
                node.path
            );
        }
        for positional in &node.required_positionals {
            assert!(
                help.contains(positional.as_str()),
                "`{} --help` omits the required argument {positional}: {help}",
                node.path
            );
        }
    }
    assert!(
        nodes.len() > leaves().len(),
        "the help matrix only covered leaves"
    );
}

#[test]
fn every_completion_shell_names_every_top_level_command() {
    let fixture = Fixture::build();
    let nodes = tree();
    let mut top_level = nodes
        .iter()
        .filter(|node| !node.path.contains(' '))
        .map(|node| node.path.clone())
        .collect::<Vec<_>>();
    top_level.sort();
    top_level.dedup();

    let shells = completion_shells();
    assert!(
        shells.len() >= 2,
        "`completions` must offer every supported shell"
    );
    for shell in &shells {
        let envelope = invoke(
            &fixture.sandbox,
            "completions",
            std::slice::from_ref(shell),
            None,
            0,
        );
        assert_eq!(envelope["data"]["shell"], json!(shell));
        let script = envelope["data"]["script"]
            .as_str()
            .unwrap_or_else(|| panic!("completions {shell} emits a script"));
        assert!(!script.is_empty(), "completions {shell} emitted nothing");
        for name in &top_level {
            assert!(
                script.contains(name.as_str()),
                "completions {shell} does not name `{name}`"
            );
        }
    }
}

/// The shell names `completions` accepts, read from its own value parser.
fn completion_shells() -> Vec<String> {
    let registry = at_cli::production_registry().expect("the production registry builds");
    let mut root = registry.command();
    root.build();
    let completions = root
        .get_subcommands()
        .find(|command| command.get_name() == "completions")
        .expect("`completions` is registered");
    let shells = completions
        .get_arguments()
        .find(|argument| argument.get_id() == "shell")
        .expect("`completions` declares a shell argument")
        .get_possible_values()
        .into_iter()
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();
    assert!(!shells.is_empty(), "`completions` declares no shell values");
    shells
}

#[test]
fn version_and_doctor_follow_the_envelope() {
    let fixture = Fixture::build();
    let version = invoke(&fixture.sandbox, "version", &[], None, exit_code::SUCCESS);
    assert_eq!(version["ok"], json!(true));
    assert_eq!(version["data"]["name"], json!("agentuna"));
    assert!(
        version["data"]["version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "`version` reports a version: {version}"
    );

    let doctor = invoke(&fixture.sandbox, "doctor", &[], None, exit_code::SUCCESS);
    assert_eq!(doctor["ok"], json!(true));
    let findings = doctor["data"]["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("`doctor` reports findings: {doctor}"));
    assert!(
        !findings.is_empty(),
        "`doctor` reports at least one finding"
    );
    for finding in findings {
        assert!(
            finding["check"].is_string()
                && finding["code"].is_string()
                && finding["severity"].is_string()
                && finding["message"].is_string()
                && finding["fix"].is_string(),
            "a finding carries check, code, severity, message and fix: {finding}"
        );
        assert!(
            matches!(
                finding["severity"].as_str(),
                Some("ok" | "warn" | "error" | "not_applicable")
            ),
            "a finding severity is one of ok, warn, error or not_applicable: {finding}"
        );
    }
    assert!(
        doctor["data"]["summary"]["not_applicable"].is_u64(),
        "the summary counts not-applicable checks: {doctor}"
    );
}

#[test]
fn a_doctor_failure_still_carries_its_report_and_exit_code() {
    let fixture = Fixture::build();
    // A corrupt index is the documented doctor failure: the payload travels with the error.
    fs::write(fixture.sandbox.paths().db_path(), b"not a database")
        .expect("corrupt the sandbox index");
    let envelope = invoke(
        &fixture.sandbox,
        "doctor",
        &[],
        None,
        exit_code::DOCTOR_FAILURE,
    );
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], json!("doctor_failed"));
    assert!(
        envelope["data"]["findings"].is_array(),
        "a failed doctor still carries the findings: {envelope}"
    );
}

#[test]
fn json_flag_is_accepted_before_and_after_the_subcommand() {
    let fixture = Fixture::build();
    let path = "projects list";
    let before = [
        "--json".to_owned(),
        "projects".to_owned(),
        "list".to_owned(),
    ];
    let after = [
        "projects".to_owned(),
        "list".to_owned(),
        "--json".to_owned(),
    ];
    let leading = run(&fixture.sandbox, &before, None);
    let trailing = run(&fixture.sandbox, &after, None);
    assert_contract(&leading, &before, path, exit_code::SUCCESS);
    assert_contract(&trailing, &after, path, exit_code::SUCCESS);

    // A failure behaves the same in both positions.
    let before = [
        "--json".to_owned(),
        "skills".to_owned(),
        "show".to_owned(),
        "missing-skill".to_owned(),
    ];
    let after = [
        "skills".to_owned(),
        "show".to_owned(),
        "missing-skill".to_owned(),
        "--json".to_owned(),
    ];
    let leading = run(&fixture.sandbox, &before, None);
    let trailing = run(&fixture.sandbox, &after, None);
    assert_contract(&leading, &before, "skills show", exit_code::NOT_FOUND);
    assert_contract(&trailing, &after, "skills show", exit_code::NOT_FOUND);
    assert_eq!(
        one_json_document(&leading, &before)["error"]["code"],
        one_json_document(&trailing, &after)["error"]["code"]
    );
}

#[test]
fn no_color_is_honoured_without_the_environment_variable() {
    let fixture = Fixture::build();
    let argv = [
        "skills".to_owned(),
        "show".to_owned(),
        "missing-skill".to_owned(),
        "--json".to_owned(),
        "--no-color".to_owned(),
    ];
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_agentuna"));
    command
        .args(&argv)
        .env_remove("NO_COLOR")
        .env("AGENTUNA_SECRET_STORE", "memory")
        .envs(fixture.sandbox.vars())
        .current_dir(fixture.sandbox.cwd())
        .stdin(Stdio::null());
    let output = command.output().expect("run the agentuna binary");
    assert_contract(&output, &argv, "skills show", exit_code::NOT_FOUND);
    assert!(
        !stderr(&output).contains('\u{1b}'),
        "`--no-color` strips ANSI even without NO_COLOR: {:?}",
        stderr(&output)
    );
}

// ---------------------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------------------

#[test]
fn the_matrix_covers_every_runnable_path_in_the_production_tree() {
    let leaves = leaves();
    let paths = leaves
        .iter()
        .map(|node| node.path.as_str())
        .collect::<BTreeSet<_>>();
    assert!(!paths.is_empty(), "the tree has no runnable path");

    let mut covered = BTreeSet::new();
    for case in additive_cases() {
        covered.insert(case.path);
    }
    for case in refusal_cases() {
        covered.insert(case.path);
    }
    for case in destructive_cases() {
        covered.insert(case.path);
    }
    for case in mutating_cases() {
        covered.insert(case.path);
    }
    for path in [
        "skills remove",
        "mcp remove",
        "migrate undo",
        "backups restore",
        "projects remove",
    ] {
        covered.insert(path);
    }

    let missing = paths
        .iter()
        .copied()
        .filter(|path| !covered.contains(path))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "these runnable commands have no success or refusal case in the matrix: {missing:?}"
    );
    for path in covered {
        assert!(
            paths.contains(path),
            "the matrix names `{path}`, which is not a runnable path in the production tree"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Sandbox hashing
// ---------------------------------------------------------------------------------------

/// Digest of every file below `root`, so a command that claims not to write is held to it.
///
/// SQLite `-wal`/`-shm` files are bookkeeping. `include_logs` is false for runs that may
/// log by design; a `--read-only` run must not write there either, so it passes true.
fn tree_hash(root: &Path, include_logs: bool) -> [u8; 32] {
    fn visit(current: &Path, digest: &mut Sha256, include_logs: bool) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            if name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal") {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            digest.update(path.to_string_lossy().as_bytes());
            if metadata.file_type().is_symlink() {
                digest.update(b"symlink");
                if let Ok(target) = fs::read_link(&path) {
                    digest.update(target.to_string_lossy().as_bytes());
                }
            } else if metadata.is_dir() {
                if name == "logs" && !include_logs {
                    continue;
                }
                digest.update(b"dir");
                visit(&path, digest, include_logs);
            } else if let Ok(bytes) = fs::read(&path) {
                digest.update(Sha256::digest(&bytes));
            }
        }
    }

    let mut digest = Sha256::new();
    visit(root, &mut digest, include_logs);
    digest.finalize().into()
}
