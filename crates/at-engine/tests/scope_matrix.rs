//! Table-driven scope resolution matrix (TF-047).
//!
//! Every `tests/fixtures/scope-matrix/*.json` file is one case. The runner materializes the
//! case's store tree through the shared [`at_fs::testkit`] temp-home fixture, reads the
//! definitions back from the files it wrote, resolves them with the real engine and compares a
//! deterministic JSON projection of the result with the case's checked-in `expect` block.
//! Adding a case is adding a file: no Rust changes are needed.
//!
//! Regenerate every `expect` block after reviewing a deliberate behaviour change:
//!
//! ```sh
//! cargo test -p at-engine --test scope_matrix -- --ignored regenerate
//! ```

mod support;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use at_engine::local_scope::{GitignoreChange, LocalLayer, LocalResolution, LocalScope};
use at_engine::locks::{LockRecord, LockStore, LockStoreError, MemoryLockStore, lock};
use at_engine::resolve::ResolveDiagnostic;
use at_fs::testkit::TestHome;
use at_model::{Definition, ItemId, ItemKind, Scope, ScopeSnapshot};
use at_store::layout::{Slug, StoreLayout};
use at_store::local_store::LocalStorePaths;
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use at_store::skill::parse_skill_doc;
use serde::Deserialize;
use serde_json::{Value, json};
use support::{PathTokens, describe_differences, json_differences};

/// Case schema version this runner understands.
const SCHEMA_VERSION: u32 = 1;

/// The checked-in case directory.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope-matrix")
}

// ---------------------------------------------------------------------------------------------
// Case schema
// ---------------------------------------------------------------------------------------------

/// One matrix case file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    schema_version: u32,
    id: String,
    title: String,
    #[serde(default)]
    project: ProjectSpec,
    #[serde(default)]
    agents: Vec<AgentSpec>,
    #[serde(default)]
    stores: Vec<StoreSpec>,
    #[serde(default)]
    locks: Vec<LockSpec>,
    #[serde(default)]
    lock_store: LockStoreKind,
    #[serde(default)]
    tombstones: Vec<TombstoneSpec>,
    #[serde(default)]
    agent: Option<String>,
    expect: Value,
}

/// A tombstone to record in one scope's store.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TombstoneSpec {
    scope: LayerName,
    kind: KindName,
    target: String,
    #[serde(default)]
    agents: Option<Vec<String>>,
}

/// What kind of project the fixture creates.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectSpec {
    /// Whether `<project>/.git` exists, which decides Local-scope availability.
    git: bool,
}

impl Default for ProjectSpec {
    fn default() -> Self {
        Self { git: true }
    }
}

/// An agent installation to materialize, or deliberately leave missing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentSpec {
    /// Built-in agent name (`claude_code`, `codex`, `omp`, `cursor`) or custom agent name.
    name: String,
    /// Install a user-defined agent under `root` instead of a built-in layout.
    #[serde(default)]
    custom: bool,
    /// Root for a custom agent, relative to the temporary home.
    #[serde(default)]
    root: Option<String>,
    /// `false` leaves every agent path absent, exercising the "missing agent paths" fixture.
    #[serde(default = "installed_default")]
    installed: bool,
}

const fn installed_default() -> bool {
    true
}

/// One scope's store contents.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreSpec {
    scope: LayerName,
    /// `true` skips creating the store directory at all.
    #[serde(default)]
    absent: bool,
    #[serde(default)]
    skills: Vec<SkillSpec>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
    #[serde(default)]
    mcp: Vec<McpSpec>,
}

/// A `SKILL.md` to write, either generated or supplied verbatim.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillSpec {
    slug: String,
    /// Frontmatter `name`; defaults to `slug`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    body: Option<String>,
    /// Exact file contents, for malformed fixtures.
    #[serde(default)]
    raw: Option<String>,
    /// Create the skill folder without a `SKILL.md`, exercising the malformed-folder path.
    #[serde(default)]
    omit: bool,
}

/// A rule file to write.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    slug: String,
}

/// One `mcp.json` server entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpSpec {
    slug: String,
    /// Declared server id; absent means the server merges by name.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// A Global lock to place on one loaded definition.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockSpec {
    kind: KindName,
    /// Declared name, or the MCP declared id.
    target: String,
    reason: String,
    locked_by: String,
    /// Lock an identity no definition uses, exercising the orphan-lock path.
    #[serde(default)]
    orphan: bool,
}

/// Scope selector in a case file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LayerName {
    Global,
    User,
    Project,
    Local,
}

impl LayerName {
    const fn scope(self) -> Scope {
        match self {
            Self::Global => Scope::Global,
            Self::User => Scope::User,
            Self::Project => Scope::Project,
            Self::Local => Scope::Local,
        }
    }
}

/// Item kind selector in a case file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KindName {
    Skill,
    McpServer,
    Rule,
}

impl KindName {
    const fn kind(self) -> ItemKind {
        match self {
            Self::Skill => ItemKind::Skill,
            Self::McpServer => ItemKind::McpServer,
            Self::Rule => ItemKind::Rule,
        }
    }
}

/// Which lock source the runner uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LockStoreKind {
    /// A working in-process lock store.
    #[default]
    Memory,
    /// A store whose reads fail, to exercise the diagnostic path.
    Failing,
}

// ---------------------------------------------------------------------------------------------
// Running one case
// ---------------------------------------------------------------------------------------------

/// A definition that could not be loaded, kept so the case can assert it was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedEntry {
    path: PathBuf,
    reason: &'static str,
}

/// Everything one materialized case produced.
struct CaseRun {
    _home: TestHome,
    home: PathBuf,
    project: PathBuf,
    global: PathBuf,
    resolution: LocalResolution,
    skipped: Vec<SkippedEntry>,
    gitignore: Option<GitignoreChange>,
}

/// A lock store whose reads always fail.
struct FailingStore;

impl FailingStore {
    fn failure() -> LockStoreError {
        LockStoreError::Backend {
            message: "fixture lock backend is offline".to_owned(),
        }
    }
}

impl LockStore for FailingStore {
    fn list(&self) -> Result<Vec<LockRecord>, LockStoreError> {
        Err(Self::failure())
    }

    fn get(&self, _item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        Err(Self::failure())
    }

    fn put(&self, _record: LockRecord) -> Result<(), LockStoreError> {
        Err(Self::failure())
    }

    fn remove(&self, _item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        Err(Self::failure())
    }
}

fn skill_bytes(spec: &SkillSpec) -> Vec<u8> {
    if let Some(raw) = &spec.raw {
        return raw.clone().into_bytes();
    }
    let name = spec.name.as_deref().unwrap_or(&spec.slug);
    let description = spec
        .description
        .as_deref()
        .unwrap_or("A fixture skill with one job.");
    let body = spec.body.as_deref().unwrap_or("# Fixture skill\n");
    format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}").into_bytes()
}

fn materialize_store(layout: &StoreLayout, spec: &StoreSpec) {
    if spec.absent {
        return;
    }
    for skill in &spec.skills {
        let slug = Slug::parse(&skill.slug).expect("fixture skill slug is valid");
        let path = layout.skill_file(&slug);
        fs::create_dir_all(path.parent().expect("skill path has a parent"))
            .expect("create skill folder");
        if !skill.omit {
            fs::write(&path, skill_bytes(skill)).expect("write SKILL.md");
        }
    }
    for rule in &spec.rules {
        let path = layout.rules_dir().join(format!("{}.md", rule.slug));
        fs::create_dir_all(path.parent().expect("rule path has a parent"))
            .expect("create rules folder");
        fs::write(&path, format!("# {}\n", rule.slug)).expect("write rule file");
    }
    if !spec.mcp.is_empty() {
        let mut servers = serde_json::Map::new();
        for server in &spec.mcp {
            let mut entry = serde_json::Map::new();
            if let Some(id) = &server.id {
                entry.insert("id".to_owned(), Value::String(id.clone()));
            }
            if let Some(command) = &server.command {
                entry.insert("command".to_owned(), Value::String(command.clone()));
            }
            if let Some(url) = &server.url {
                entry.insert("url".to_owned(), Value::String(url.clone()));
            }
            servers.insert(server.slug.clone(), Value::Object(entry));
        }
        let document = json!({ "schema_version": 1, "servers": servers });
        let path = layout.mcp_file();
        fs::create_dir_all(path.parent().expect("mcp path has a parent"))
            .expect("create store root");
        fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&document).expect("serialize mcp fixture")
            ),
        )
        .expect("write mcp.json");
    }
}

fn load_skills(layout: &StoreLayout, skipped: &mut Vec<SkippedEntry>) -> Vec<Definition> {
    let mut definitions = Vec::new();
    let Ok(entries) = fs::read_dir(layout.skills_dir()) else {
        return definitions;
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    for folder in folders {
        let file = folder.join("SKILL.md");
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) => {
                let reason = if error.kind() == io::ErrorKind::NotFound {
                    "skill_folder_without_skill_md"
                } else {
                    "unreadable_skill_document"
                };
                skipped.push(SkippedEntry { path: file, reason });
                continue;
            }
        };
        match parse_skill_doc(&bytes) {
            Ok(doc) => definitions.push(Definition {
                kind: ItemKind::Skill,
                name: doc.name,
                declared_id: None,
                item_id: Some(ItemId::new()),
                path: file,
            }),
            Err(_) => skipped.push(SkippedEntry {
                path: file,
                reason: "invalid_skill_document",
            }),
        }
    }
    definitions
}

fn load_rules(layout: &StoreLayout) -> Vec<Definition> {
    let mut definitions = Vec::new();
    let Ok(entries) = fs::read_dir(layout.rules_dir()) else {
        return definitions;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension() == Some(OsStr::new("md")))
        .collect();
    files.sort();
    for file in files {
        let Some(name) = file.file_stem().and_then(OsStr::to_str) else {
            continue;
        };
        definitions.push(Definition {
            kind: ItemKind::Rule,
            name: name.to_owned(),
            declared_id: None,
            item_id: Some(ItemId::new()),
            path: file,
        });
    }
    definitions
}

fn load_mcp(layout: &StoreLayout, skipped: &mut Vec<SkippedEntry>) -> Vec<Definition> {
    let path = layout.mcp_file();
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    let document: Value = match serde_json::from_slice(&bytes) {
        Ok(document) => document,
        Err(_) => {
            skipped.push(SkippedEntry {
                path,
                reason: "invalid_mcp_document",
            });
            return Vec::new();
        }
    };
    let Some(servers) = document.get("servers").and_then(Value::as_object) else {
        skipped.push(SkippedEntry {
            path,
            reason: "invalid_mcp_document",
        });
        return Vec::new();
    };
    let mut definitions = Vec::new();
    for (slug, entry) in servers {
        let declares_command = entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.is_empty());
        let declares_url = entry
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| !url.is_empty());
        if declares_command == declares_url {
            skipped.push(SkippedEntry {
                path: path.clone(),
                reason: "mcp_entry_missing_transport",
            });
            continue;
        }
        let declared_id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        definitions.push(Definition {
            kind: ItemKind::McpServer,
            name: slug.clone(),
            declared_id,
            item_id: Some(ItemId::new()),
            path: path.clone(),
        });
    }
    definitions
}

fn load_layer(layout: &StoreLayout, skipped: &mut Vec<SkippedEntry>) -> Vec<Definition> {
    let mut definitions = load_skills(layout, skipped);
    definitions.extend(load_rules(layout));
    definitions.extend(load_mcp(layout, skipped));
    definitions
}

fn find_locked_definition<'a>(definitions: &'a [Definition], spec: &LockSpec) -> &'a Definition {
    let kind = spec.kind.kind();
    definitions
        .iter()
        .find(|definition| {
            definition.kind == kind
                && (definition.declared_id.as_deref() == Some(spec.target.as_str())
                    || definition.name == spec.target)
        })
        .unwrap_or_else(|| {
            panic!(
                "lock target `{}` of kind {:?} is not defined by this case",
                spec.target, kind
            )
        })
}
fn find_tombstone_definition<'a>(
    definitions: &'a [Definition],
    spec: &TombstoneSpec,
) -> &'a Definition {
    let kind = spec.kind.kind();
    definitions
        .iter()
        .find(|definition| {
            definition.kind == kind
                && (definition.declared_id.as_deref() == Some(spec.target.as_str())
                    || definition.name == spec.target)
        })
        .unwrap_or_else(|| {
            panic!(
                "tombstone target `{}` of kind {:?} is not defined by this case",
                spec.target, kind
            )
        })
}

fn run_case(case: &Case) -> CaseRun {
    let home = TestHome::new().expect("temporary home");
    let home_path = home.home().to_path_buf();
    let project = home.project(&case.id).expect("temporary project fixture");
    let project_root = project.root().to_path_buf();

    if case.project.git {
        let git_dir = project_root.join(".git");
        fs::create_dir_all(git_dir.join("info")).expect("create fixture git directory");
        fs::write(git_dir.join("info/exclude"), "").expect("create fixture exclude file");
    }

    for agent in &case.agents {
        if !agent.installed {
            continue;
        }
        if agent.custom {
            let root = agent
                .root
                .clone()
                .unwrap_or_else(|| format!("agents/{}", agent.name));
            home.install_custom_agent(&agent.name, root)
                .expect("install custom agent fixture");
        } else {
            match agent.name.as_str() {
                "claude_code" => home.install_claude_code(),
                "codex" => home.install_codex(),
                "omp" => home.install_omp(),
                "cursor" => home.install_cursor(),
                other => panic!("unknown built-in agent fixture `{other}`"),
            }
            .expect("install agent fixture");
        }
    }

    let global_root = home.home().join("global");
    let paths = AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(home.home())
            .with_project_root(&project_root)
            .with_overrides(PathOverrides {
                global_store: Some(global_root.clone()),
                ..PathOverrides::default()
            }),
    )
    .expect("resolve fixture app paths");

    for store in &case.stores {
        let layout = StoreLayout::for_scope(&paths, store.scope.scope()).expect("store layout");
        materialize_store(&layout, store);
    }

    let mut skipped = Vec::new();
    let mut layers = Vec::new();
    for store in &case.stores {
        if store.scope.scope() == Scope::Local || store.absent {
            continue;
        }
        let layout = StoreLayout::for_scope(&paths, store.scope.scope()).expect("store layout");
        layers.push(ScopeSnapshot {
            scope: store.scope.scope(),
            definitions: load_layer(&layout, &mut skipped),
        });
    }

    let local_store = LocalStorePaths::for_paths(&paths).expect("local store layout");
    let local_scope = LocalScope::detect(project_root.clone(), local_store.clone());
    let local_spec = case
        .stores
        .iter()
        .find(|store| store.scope == LayerName::Local);
    let local_definitions = match local_spec {
        Some(store) if !store.absent => load_layer(local_store.layout(), &mut skipped),
        _ => Vec::new(),
    };
    let local_layer = LocalLayer::new(local_scope.clone(), local_definitions)
        .expect("local definitions live inside the local store");
    let gitignore = match local_spec {
        Some(store) if !store.absent && case.project.git => {
            Some(local_scope.ensure_project_gitignore().expect("gitignore"))
        }
        _ => None,
    };

    let memory = MemoryLockStore::new();
    let failing = FailingStore;
    let store: &dyn LockStore = match case.lock_store {
        LockStoreKind::Memory => &memory,
        LockStoreKind::Failing => &failing,
    };
    if case.lock_store == LockStoreKind::Memory {
        let mut all: Vec<Definition> = layers
            .iter()
            .flat_map(|layer| layer.definitions.iter().cloned())
            .collect();
        all.extend(local_layer.definitions().iter().cloned());
        for spec in &case.locks {
            let item_id = if spec.orphan {
                ItemId::new()
            } else {
                find_locked_definition(&all, spec)
                    .item_id
                    .expect("loaded definitions carry an id")
            };
            lock(
                &memory,
                Scope::Global,
                item_id,
                &spec.reason,
                &spec.locked_by,
            )
            .expect("place fixture lock");
        }
    }
    let tombstones = at_engine::tombstones::MemoryTombstones::new();
    let mut has_tombstones = false;
    if !case.tombstones.is_empty() {
        has_tombstones = true;
        let mut all: Vec<Definition> = layers
            .iter()
            .flat_map(|layer| layer.definitions.iter().cloned())
            .collect();
        all.extend(local_layer.definitions().iter().cloned());

        for spec in &case.tombstones {
            let def = find_tombstone_definition(&all, spec);
            let item_id = def.item_id.expect("loaded definitions carry an id");
            let agents = match &spec.agents {
                None => at_store::manifest::DisabledAgents::All,
                Some(list) => {
                    let parsed: Vec<at_model::AgentId> = list
                        .iter()
                        .map(|name| name.parse().expect("valid agent id in tombstone spec"))
                        .collect();
                    at_store::manifest::DisabledAgents::Only(parsed)
                }
            };
            tombstones
                .add(
                    spec.scope.scope(),
                    at_store::manifest::DisabledEntry {
                        item_id,
                        kind: spec.kind.kind(),
                        agents,
                    },
                )
                .expect("add tombstone");
        }
    }

    let agent = case
        .agent
        .as_deref()
        .map(|s| s.parse().expect("valid agent in case"));

    skipped.sort_by(|left, right| left.path.cmp(&right.path));
    let resolution = at_engine::local_scope::resolve_with_local_and_tombstones(
        &layers,
        &local_layer,
        Some(store),
        if has_tombstones {
            Some(&tombstones)
        } else {
            None
        },
        agent.as_ref(),
    );

    CaseRun {
        _home: home,
        home: home_path,
        project: project_root,
        global: global_root,
        resolution,
        skipped,
        gitignore,
    }
}

// ---------------------------------------------------------------------------------------------
// Deterministic projection
// ---------------------------------------------------------------------------------------------

fn scope_json(scope: Scope) -> Value {
    serde_json::to_value(scope).expect("scope serializes")
}

fn item_json(
    tokens: &PathTokens<'_>,
    resolution: &LocalResolution,
    item: &at_engine::resolve::ResolvedItem,
) -> Value {
    let mut trace: Vec<(u8, Value)> = resolution
        .resolved
        .shadowed
        .iter()
        .filter(|shadowed| shadowed.logical_id == item.logical_id)
        .map(|shadowed| {
            (
                shadowed.scope.precedence(),
                json!({
                    "scope": scope_json(shadowed.scope),
                    "outcome": if shadowed.is_blocked() { "blocked_by_lock" } else { "shadowed" },
                }),
            )
        })
        .collect();
    trace.sort_by_key(|(precedence, _)| std::cmp::Reverse(*precedence));
    let mut steps: Vec<Value> = trace.into_iter().map(|(_, step)| step).collect();
    steps.push(json!({ "scope": scope_json(item.scope), "outcome": "winner" }));

    json!({
        "kind": serde_json::to_value(item.logical_id.kind).expect("kind serializes"),
        "key": serde_json::to_value(&item.logical_id.key).expect("key serializes"),
        "name": item.definition.name,
        "scope": scope_json(item.scope),
        "path": tokens.tokenize(&item.definition.path),
        "locked": item.is_locked(),
        "lock": item.lock.as_ref().map(|mark| json!({
            "reason": mark.reason,
            "locked_by": mark.locked_by,
            "scope": scope_json(mark.locked_by_scope),
        })),
        "trace": steps,
    })
}

fn expected_json(run: &CaseRun) -> Value {
    let tokens = PathTokens {
        home: &run.home,
        project: &run.project,
        global: &run.global,
    };
    let items: Vec<Value> = run
        .resolution
        .resolved
        .items
        .iter()
        .map(|item| item_json(&tokens, &run.resolution, item))
        .collect();
    let shadowed: Vec<Value> = run
        .resolution
        .resolved
        .shadowed
        .iter()
        .map(|entry| {
            json!({
                "kind": serde_json::to_value(entry.logical_id.kind).expect("kind serializes"),
                "key": serde_json::to_value(&entry.logical_id.key).expect("key serializes"),
                "name": entry.definition.name,
                "scope": scope_json(entry.scope),
                "path": tokens.tokenize(&entry.definition.path),
                "shadowed_by": {
                    "scope": scope_json(entry.shadowed_by.scope),
                    "path": tokens.tokenize(&entry.shadowed_by.path),
                },
                "blocked": entry.is_blocked(),
            })
        })
        .collect();
    let diagnostics: Vec<Value> = run
        .resolution
        .resolved
        .diagnostics
        .iter()
        .map(|diagnostic| match diagnostic {
            ResolveDiagnostic::LockReadFailed { message } => {
                json!({ "kind": "lock_read_failed", "message": message })
            }
            _ => json!({ "kind": "unknown" }),
        })
        .collect();
    let skipped: Vec<Value> = run
        .skipped
        .iter()
        .map(|entry| {
            json!({
                "path": tokens.tokenize(&entry.path),
                "reason": entry.reason,
            })
        })
        .collect();
    let local = if run.resolution.local.is_available() {
        json!({ "status": "available", "reason": null })
    } else {
        json!({
            "status": "not_available",
            "reason": run
                .resolution
                .local
                .reason()
                .map(|reason| reason.as_str()),
        })
    };
    let gitignore = run.gitignore.map(|change| match change {
        GitignoreChange::Created => "created",
        GitignoreChange::Appended => "appended",
        GitignoreChange::Unchanged => "unchanged",
        _ => "unknown",
    });
    let mut root = json!({
        "items": items,
        "shadowed": shadowed,
        "diagnostics": diagnostics,
        "skipped": skipped,
        "local": local,
        "gitignore": gitignore,
    });
    if !run.resolution.resolved.disabled.is_empty() {
        let disabled: Vec<Value> = run
            .resolution
            .resolved
            .disabled
            .iter()
            .map(|entry| {
                json!({
                    "kind": serde_json::to_value(entry.logical_id.kind).expect("kind serializes"),
                    "key": serde_json::to_value(&entry.logical_id.key).expect("key serializes"),
                    "name": entry.definition.name,
                    "scope": scope_json(entry.scope),
                    "agents": match &entry.agents {
                        at_store::manifest::DisabledAgents::All => Value::Null,
                        at_store::manifest::DisabledAgents::Only(list) => {
                            let mut names: Vec<String> = list.iter().map(|a| a.to_string()).collect();
                            names.sort();
                            json!(names)
                        }
                    },
                })
            })
            .collect();
        root["disabled"] = json!(disabled);
    }
    root
}

// ---------------------------------------------------------------------------------------------
// Discovery, validation and comparison
// ---------------------------------------------------------------------------------------------

/// One case file plus the parsed case.
#[derive(Debug)]
struct LoadedCase {
    path: PathBuf,
    case: Case,
}

fn validate_case(path: &Path, case: &Case) -> Result<(), String> {
    let shown = path.display();
    if case.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "{shown}: unsupported case schema_version {}; expected {SCHEMA_VERSION}",
            case.schema_version
        ));
    }
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or_default();
    if case.id != stem {
        return Err(format!(
            "{shown}: case id `{}` does not match the file name `{stem}`",
            case.id
        ));
    }
    if case.title.trim().is_empty() {
        return Err(format!("{shown}: case title must not be empty"));
    }
    let mut scopes = BTreeSet::new();
    for store in &case.stores {
        if !scopes.insert(store.scope) {
            return Err(format!(
                "{shown}: store scope `{:?}` is declared more than once",
                store.scope
            ));
        }
        if store.absent
            && (!store.skills.is_empty() || !store.rules.is_empty() || !store.mcp.is_empty())
        {
            return Err(format!(
                "{shown}: store scope `{:?}` is absent but declares entries",
                store.scope
            ));
        }
    }
    if case.lock_store == LockStoreKind::Failing && !case.locks.is_empty() {
        return Err(format!(
            "{shown}: a failing lock store cannot carry lock entries"
        ));
    }
    Ok(())
}

fn discover_cases(dir: &Path) -> Result<Vec<LoadedCase>, String> {
    let entries = fs::read_dir(dir).map_err(|error| {
        format!(
            "{}: cannot read the scope matrix fixture directory: {error}",
            dir.display()
        )
    })?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("json")))
        .collect();
    paths.sort();
    let mut cases = Vec::with_capacity(paths.len());
    for path in paths {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("{}: cannot read case file: {error}", path.display()))?;
        let case: Case = serde_json::from_str(&text)
            .map_err(|error| format!("{}: invalid case schema: {error}", path.display()))?;
        validate_case(&path, &case)?;
        cases.push(LoadedCase { path, case });
    }
    if cases.is_empty() {
        return Err(format!(
            "{}: no case files found; expected at least one `*.json`",
            dir.display()
        ));
    }
    Ok(cases)
}

fn load_named_case<'a>(cases: &'a [LoadedCase], id: &str) -> &'a Case {
    &cases
        .iter()
        .find(|loaded| loaded.case.id == id)
        .unwrap_or_else(|| panic!("scope matrix fixture `{id}` is missing"))
        .case
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn every_scope_matrix_case_matches_its_checked_in_expectation() {
    let started = Instant::now();
    let cases = discover_cases(&fixtures_dir()).expect("scope matrix fixtures are valid");
    assert!(
        cases.len() >= 25,
        "the matrix must keep at least 25 cases; found {}",
        cases.len()
    );
    let mut ids = BTreeSet::new();
    for loaded in &cases {
        assert!(
            ids.insert(loaded.case.id.clone()),
            "duplicate case id `{}` in {}",
            loaded.case.id,
            loaded.path.display()
        );
    }
    for loaded in &cases {
        println!("scope matrix case: {}", loaded.case.id);
        let run = run_case(&loaded.case);
        let actual = expected_json(&run);
        let differences = json_differences(&loaded.case.expect, &actual);
        assert!(
            differences.is_empty(),
            "case `{}` ({}) does not match {}:\n{}",
            loaded.case.id,
            loaded.case.title,
            loaded.path.display(),
            describe_differences(&differences)
        );
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the matrix took {elapsed:?}; the budget is 5s"
    );
}

#[test]
fn four_scope_precedence_records_the_winning_scope_in_the_trace() {
    let cases = discover_cases(&fixtures_dir()).expect("fixtures");
    let case = load_named_case(&cases, "skill-precedence-all-scopes");
    let run = run_case(case);
    let winners = &run.resolution.resolved.items;
    assert_eq!(winners.len(), 1, "one logical id resolves");
    let winner = &winners[0];
    assert_eq!(winner.scope, Scope::Local, "Local outranks Project");
    assert_eq!(winner.definition.name, "code-review");
    let shadowed_scopes: Vec<Scope> = run
        .resolution
        .resolved
        .shadowed
        .iter()
        .map(|entry| entry.scope)
        .collect();
    assert_eq!(
        shadowed_scopes,
        vec![Scope::Global, Scope::User, Scope::Project],
        "every lower scope is reported once"
    );
    assert!(
        run.resolution
            .resolved
            .shadowed
            .iter()
            .all(|entry| entry.shadowed_by.scope == Scope::Local)
    );
}

#[test]
fn a_global_lock_beats_every_lower_scope_including_local() {
    let cases = discover_cases(&fixtures_dir()).expect("fixtures");
    let case = load_named_case(&cases, "lock-global-pins-skill-over-local");
    let run = run_case(case);
    let winners = &run.resolution.resolved.items;
    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].scope, Scope::Global);
    assert!(winners[0].is_locked(), "the winner carries the lock mark");
    assert_eq!(
        winners[0].lock_reason(),
        Some("organisation-wide review policy")
    );
    let blocked: Vec<Scope> = run
        .resolution
        .resolved
        .shadowed
        .iter()
        .filter(|entry| entry.is_blocked())
        .map(|entry| entry.scope)
        .collect();
    assert_eq!(blocked, vec![Scope::User, Scope::Project, Scope::Local]);
}

#[test]
fn a_malformed_skill_file_is_skipped_instead_of_aborting_resolution() {
    let cases = discover_cases(&fixtures_dir()).expect("fixtures");
    let case = load_named_case(&cases, "malformed-skill-is-skipped");
    let run = run_case(case);
    assert_eq!(run.skipped.len(), 1, "exactly one malformed file");
    assert_eq!(run.skipped[0].reason, "invalid_skill_document");
    let winner = run
        .resolution
        .resolved
        .items
        .first()
        .expect("the valid definition still resolves");
    assert_eq!(winner.scope, Scope::Project);
}

#[test]
fn agent_installations_and_absent_agent_paths_do_not_affect_resolution() {
    let cases = discover_cases(&fixtures_dir()).expect("fixtures");
    let case = load_named_case(&cases, "custom-agent-and-missing-paths");
    let run = run_case(case);
    let installed = run._home.installed();
    assert_eq!(
        installed.len(),
        1,
        "only the installed agent fixture exists"
    );
    assert_eq!(installed[0].agent.to_string(), "custom:studio");
    assert!(
        !run._home.path(".claude").exists() && !run._home.path(".codex").exists(),
        "the fixture leaves built-in agent paths missing"
    );
    assert_eq!(run.resolution.resolved.items.len(), 1);
    assert_eq!(run.resolution.resolved.items[0].scope, Scope::Project);
}

#[test]
fn a_project_without_a_work_tree_reports_local_as_not_available() {
    let cases = discover_cases(&fixtures_dir()).expect("fixtures");
    let case = load_named_case(&cases, "local-not-available-without-git");
    let run = run_case(case);
    assert!(!run.resolution.local.is_available());
    let winner = run.resolution.resolved.items.first().expect("a winner");
    assert_eq!(winner.scope, Scope::Project);
}

#[test]
fn a_new_case_file_is_discovered_and_an_invalid_file_names_its_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let valid = r#"{
        "schema_version": 1,
        "id": "hand-written",
        "title": "A case added without touching Rust",
        "stores": [{ "scope": "user", "skills": [{ "slug": "alpha" }] }],
        "expect": {}
    }"#;
    fs::write(directory.path().join("hand-written.json"), valid).expect("write case");
    let cases = discover_cases(directory.path()).expect("a valid directory discovers");
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].case.id, "hand-written");

    fs::write(
        directory.path().join("broken.json"),
        "{ \"schema_version\": 1,",
    )
    .expect("write broken case");
    let message = discover_cases(directory.path()).expect_err("invalid JSON is rejected");
    assert!(
        message.contains("broken.json") && message.contains("invalid case schema"),
        "the error names the file and the schema failure: {message}"
    );

    fs::remove_file(directory.path().join("broken.json")).expect("remove broken case");
    let unknown_field = r#"{
        "schema_version": 1,
        "id": "unknown-field",
        "title": "Has a field the schema does not define",
        "unexpected": true,
        "expect": {}
    }"#;
    fs::write(directory.path().join("unknown-field.json"), unknown_field).expect("write case");
    let message = discover_cases(directory.path()).expect_err("unknown fields are rejected");
    assert!(
        message.contains("unknown-field.json") && message.contains("unknown field"),
        "the error names the file and the unknown field: {message}"
    );
}

#[test]
#[ignore = "rewrites checked-in expectations; run manually after reviewing a behaviour change"]
fn regenerate() {
    for loaded in discover_cases(&fixtures_dir()).expect("fixtures") {
        let run = run_case(&loaded.case);
        let actual = expected_json(&run);
        let text = fs::read_to_string(&loaded.path).expect("read case");
        let mut document: Value = serde_json::from_str(&text).expect("case is JSON");
        document["expect"] = actual;
        fs::write(
            &loaded.path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&document).expect("serialize case")
            ),
        )
        .expect("write case");
    }
}
