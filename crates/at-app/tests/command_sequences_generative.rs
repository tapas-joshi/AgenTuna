//! Stateful, generative sequence tests over the reviewed-operation and undo engine.
//!
//! Every case builds a fresh sandbox, applies a seeded random sequence of reviewed operations
//! through the same command bodies the webview and CLI call, then undoes them newest-first and
//! asserts the user-visible tree and the index rows are exactly what they were before the
//! sequence. The other properties interrupt the flow with an external edit, a typed refusal, or
//! a replayed preview and assert the same guarantees: no clobbering, no drift, no half-written
//! history.
//!
//! The generator is the shared deterministic `Rng` from `at_fs::testkit`; a failing case prints
//! its seed and its step list, so the sequence is replayed exactly without a clock or the
//! environment.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tempfile::TempDir;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
use at_app::app::bulk_skill::BulkSkillApplyRequest;
use at_app::app::commands::{CommandError, CommandErrorCode};
use at_app::app::flow_harness::{self, skill_agent_apply, skill_agent_preview};
use at_app::app::mcp_copy::{McpCopyApplyRequest, McpCopyPreviewRequest};
use at_app::app::mcp_form::{McpPlanPreviewRequest, McpSaveRequest};
use at_app::app::mcp_servers::{McpServerActionOutcome, McpServerActionRequest};
use at_app::app::migrate::{
    MigrateApplyRequest, MigratePlanRequest, MigrateResolutionView, migrate_apply_inner,
    migrate_apply_result_inner, migrate_plan_inner, migrate_scan_inner, operation_undo_check_inner,
};
use at_app::app::overwrite::operation_undo_inner;
use at_app::app::rule_create::InstructionCreateRequest;
use at_app::app::skill_agent::SkillAgentPreviewRequest;
use at_app::app::skill_delete::{SkillDeleteApplyRequest, SkillDeletePreviewRequest};
use at_app::app::skill_transfer::{SkillTransferApplyRequest, SkillTransferPreviewRequest};
use at_app::app::state::AppState;
use at_app::app::workflows::InstructionSaveRequest;
use at_db::Database;
use at_db::deployments::{DeploymentHashes, DeploymentSpec, upsert_deployment};
use at_fs::testkit::generative::Rng;
use at_model::{AgentId, ItemId, ItemKind, ItemState, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath, hash_mcp_entry};
use at_store::mcp_store::McpStore;
use at_store::paths::AppPaths;
use support::{Sandbox, TestResult};

/// Number of independent sandboxes per property.
const SEQUENCES: u64 = 150;

/// Sandboxes run in parallel so 150 stateful cases finish inside the debug budget.
const WORKERS: u64 = 8;

/// The canonical body every fixture skill uses, so a deletion restores recognisable bytes.
const SKILL_MD: &[u8] = b"---\nname: Sequence Skill\ndescription: sandbox only\n---\n\nBody.\n";

/// Slug of the skill deployed before the baseline; its drift event is the pre-sequence edit.
const DRIFT_SKILL: &str = "sequence-drift";

fn scope_name(scope: Scope) -> &'static str {
    scope.wire_name()
}

/// One skill package the sequence may enable, transfer or delete.
#[derive(Clone)]
struct SkillSlot {
    item_id: ItemId,
    slug: String,
    scope: Scope,
    live: bool,
    /// The fixture deploys this one and manufactures a drift event before the baseline.
    dedicated_drift: bool,
}

/// One canonical MCP server the sequence may edit, toggle, copy or delete.
#[derive(Clone)]
struct McpSlot {
    item_id: ItemId,
    scope: Scope,
    name: String,
    agent: AgentId,
    live: bool,
}

/// One rule file the sequence may save.
#[derive(Clone)]
struct RuleSlot {
    item_id: String,
    path: String,
    scope: Scope,
    live: bool,
}

/// Where the pre-baseline external edit lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftMode {
    /// No deployment is prepared; the property does not need one.
    None,
    /// The drift skill is deployed and healthy.
    Deployed,
    /// The drift skill is deployed and then edited outside AgenTuna, opening one event.
    PreOpened,
}

/// Collects bridge job events so a failed migration can report its typed reason.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<JobEvent>>,
}

impl EventSink for RecordingSink {
    fn emit(&self, event: JobEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

/// One sandbox plus the live state a generated sequence needs to address its items.
struct World {
    sandbox: Sandbox,
    database: Database,
    state: AppState,
    /// Created on the first migration step: most sequences never migrate.
    migration_state: Option<AppState>,
    migration_events: Arc<RecordingSink>,
    paths: AppPaths,
    project_id: String,
    skills: Vec<SkillSlot>,
    mcps: Vec<McpSlot>,
    rules: Vec<RuleSlot>,
    drift_event: Option<i64>,
    migration_used: bool,
    next_name: u32,
    steps: Vec<String>,
}

/// One generated step: the reviewed intents are resolved from the live world at execution time.
#[derive(Debug, Clone)]
enum Step {
    SkillToggle {
        skill: usize,
        agent: AgentId,
        enabled: bool,
    },
    BulkToggle {
        skills: Vec<usize>,
        agent: AgentId,
        enabled: bool,
    },
    Transfer {
        skill: usize,
        target: Scope,
        transfer: &'static str,
        resolution: Option<String>,
        new_slug: Option<String>,
    },
    DeleteSkill {
        skill: usize,
    },
    McpAdd {
        scope: Scope,
        agent: AgentId,
    },
    McpEdit {
        mcp: usize,
        rename: bool,
    },
    McpToggle {
        mcp: usize,
        enabled: bool,
    },
    McpDelete {
        mcp: usize,
    },
    McpCopy {
        mcp: usize,
        target: Scope,
    },
    RuleCreate {
        scope: Scope,
        file_type: &'static str,
    },
    RuleSave {
        rule: usize,
    },
    Migrate,
    DriftResolve {
        adopt: bool,
    },
}

impl Step {
    fn describe(&self, world: &World) -> String {
        match self {
            Self::SkillToggle {
                skill,
                agent,
                enabled,
            } => format!(
                "skill {} enabled={enabled} for {agent}",
                world.skills[*skill].slug
            ),
            Self::BulkToggle {
                skills,
                agent,
                enabled,
            } => format!(
                "bulk enabled={enabled} [{}] for {agent}",
                skills
                    .iter()
                    .map(|index| world.skills[*index].slug.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Transfer {
                skill,
                target,
                transfer,
                ..
            } => format!(
                "{transfer} {} to {}",
                world.skills[*skill].slug,
                scope_name(*target)
            ),
            Self::DeleteSkill { skill } => format!("delete skill {}", world.skills[*skill].slug),
            Self::McpAdd { scope, agent } => {
                format!("add mcp to {} via {agent}", scope_name(*scope))
            }
            Self::McpEdit { mcp, rename } => {
                format!("edit mcp {} rename={rename}", world.mcps[*mcp].name)
            }
            Self::McpToggle { mcp, enabled } => {
                format!("mcp {} enabled={enabled}", world.mcps[*mcp].name)
            }
            Self::McpDelete { mcp } => format!("delete mcp {}", world.mcps[*mcp].name),
            Self::McpCopy { mcp, target } => format!(
                "copy mcp {} to {}",
                world.mcps[*mcp].name,
                scope_name(*target)
            ),
            Self::RuleCreate { scope, file_type } => {
                format!("create {file_type} in {}", scope_name(*scope))
            }
            Self::RuleSave { rule } => format!("save rule {}", world.rules[*rule].path),
            Self::Migrate => String::from("migrate native skill"),
            Self::DriftResolve { adopt } => format!("resolve drift adopt={adopt}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------------

fn write_bytes(path: &Path, bytes: &[u8]) -> TestResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn project_arg(scope: Scope, project_id: &str) -> Option<String> {
    (scope == Scope::Project).then(|| project_id.to_owned())
}

/// Installs one fixture skill and returns its index.
fn install(world: &mut World, scope: Scope, slug: &str, name: &str) -> TestResult<usize> {
    let project = project_arg(scope, &world.project_id);
    let installed = world.sandbox.install_skill(
        &world.database,
        scope,
        slug,
        name,
        SKILL_MD,
        project.as_deref(),
    )?;
    world.skills.push(SkillSlot {
        item_id: installed.item_id,
        slug: installed.slug,
        scope,
        live: true,
        dedicated_drift: false,
    });
    Ok(world.skills.len() - 1)
}

fn mcp_request(
    name: &str,
    scope: Scope,
    agent: &AgentId,
    project_id: Option<&str>,
    item_id: Option<String>,
    args: Vec<String>,
) -> McpPlanPreviewRequest {
    McpPlanPreviewRequest {
        name: name.to_owned(),
        transport: "stdio".to_owned(),
        command: "npx".to_owned(),
        args,
        url: String::new(),
        headers: Vec::new(),
        env: Vec::new(),
        scope: scope_name(scope).to_owned(),
        agents: vec![agent.to_string()],
        project_id: project_id.map(str::to_owned),
        item_id,
    }
}

fn item_id_by_name(database: &Database, kind: &str, name: &str) -> TestResult<ItemId> {
    let text: String = database.connection().query_row(
        "SELECT id FROM items WHERE kind = ?1 AND name = ?2 LIMIT 1",
        rusqlite::params![kind, name],
        |row| row.get(0),
    )?;
    Ok(text.parse()?)
}

fn item_id_by_source(database: &Database, path: &str) -> TestResult<String> {
    Ok(database.connection().query_row(
        "SELECT id FROM items WHERE source_path = ?1 LIMIT 1",
        [path],
        |row| row.get(0),
    )?)
}

fn mcp_definition(name: &str) -> McpServerDef {
    McpServerDef {
        display_name: name.to_owned(),
        description: None,
        transport: McpTransport::Stdio {
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "demo-server".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    }
}

/// Writes one scope's MCP store, manifest records and item rows directly.
///
/// Setup is not the behaviour under test: seeding the canonical files and the projection
/// costs a few writes instead of several journaled commands per case, which keeps a
/// 150-sequence property inside the debug runtime budget.
fn seed_mcp_store(
    world: &mut World,
    scope: Scope,
    seeds: &[(&str, &str, AgentId)],
) -> TestResult<()> {
    let layout = StoreLayout::for_scope(&world.paths, scope)?;
    let mut loaded = McpStore::load(&layout)?;
    let mut manifest = StoreManifest::load(&layout)?.manifest;
    for (name, slug, agent) in seeds {
        let definition = mcp_definition(name);
        let slug_value = Slug::parse(slug)?;
        let value = serde_json::to_value(&definition)?;
        let hash = hash_mcp_entry(&value)?;
        loaded.store.set(*slug, definition.clone());
        let item_id = ItemId::new();
        manifest.insert_item(
            ManifestItem::new(
                item_id,
                ItemKind::McpServer,
                slug_value,
                StorePath::parse("mcp.json")?,
                hash.clone(),
            )
            .with_name(*name),
        )?;
        world.database.connection().execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, \
             content_hash, state, managed) VALUES (?1, 'mcp_server', ?2, ?3, ?4, ?5, \
             'mcp.json', ?6, 'present', 1)",
            rusqlite::params![
                item_id.to_string(),
                scope.wire_name(),
                project_arg(scope, &world.project_id),
                slug,
                name,
                hash.to_string(),
            ],
        )?;
        world.mcps.push(McpSlot {
            item_id,
            scope,
            name: (*name).to_owned(),
            agent: agent.clone(),
            live: true,
        });
    }
    fs::write(layout.mcp_file(), loaded.store.to_bytes()?)?;
    fs::write(layout.manifest_path(), manifest.to_bytes()?)?;
    Ok(())
}

/// Records the native projection row after the native documents are written.
fn seed_mcp_deployment(world: &World, slot: &McpSlot) -> TestResult<()> {
    let layout = StoreLayout::for_scope(&world.paths, slot.scope)?;
    let native = world
        .sandbox
        .agent_path(&slot.agent, ItemKind::McpServer, slot.scope)?;
    let store_hash = at_fs::sha256_hex(&fs::read(layout.mcp_file())?);
    let target_hash = at_fs::sha256_hex(&fs::read(&native)?);
    let target_text = native.to_string_lossy().into_owned();
    let hashes = DeploymentHashes::new(Some(&store_hash), Some(&target_hash), Some(&target_hash));
    let mut spec = DeploymentSpec::applied(
        &slot.item_id,
        &slot.agent,
        slot.scope,
        ItemState::Linked,
        Some("native"),
        Some(&target_text),
        hashes,
    );
    if slot.scope == Scope::Project {
        spec = spec.with_project(&world.project_id);
    }
    upsert_deployment(world.database.connection(), &spec)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Process-wide fixture template
// ---------------------------------------------------------------------------------------------

/// A fully built fixture kept for the life of the test process.
///
/// Building the sandbox (stores, manifests, native documents and index rows) costs more than a
/// whole generated sequence. The template is built once per drift mode; every case clones it
/// with a plain recursive copy ([`Sandbox::clone_tree`]) and rewrites the absolute paths the
/// copied index recorded, so nothing points back at the template.
struct Template {
    /// Owns the template directory until the process exits.
    _dir: TempDir,
    /// Canonical template root; the recorded paths start with it.
    root: PathBuf,
    project_id: String,
    skills: Vec<SkillSlot>,
    mcps: Vec<McpSlot>,
    rules: Vec<RuleSlot>,
    drift_event: Option<i64>,
}

static TEMPLATE_NONE: OnceLock<Result<Template, String>> = OnceLock::new();
static TEMPLATE_DEPLOYED: OnceLock<Result<Template, String>> = OnceLock::new();
static TEMPLATE_PREOPENED: OnceLock<Result<Template, String>> = OnceLock::new();

/// Returns the template for `drift`, building it on first use.
///
/// The first caller also attaches the testkit fast-sync switch so generated commands do not
/// spend their time flushing backups to the device. The switch exists only with the `testkit`
/// cargo feature, so production builds cannot enable it.
fn template(drift: DriftMode) -> TestResult<&'static Template> {
    static FAST_SYNC: std::sync::Once = std::sync::Once::new();
    FAST_SYNC.call_once(|| at_fs::testkit::set_fast_sync(true));
    let slot = match drift {
        DriftMode::None => &TEMPLATE_NONE,
        DriftMode::Deployed => &TEMPLATE_DEPLOYED,
        DriftMode::PreOpened => &TEMPLATE_PREOPENED,
    };
    let built = slot.get_or_init(|| build_template(drift).map_err(|error| error.to_string()));
    match built {
        Ok(template) => Ok(template),
        Err(error) => Err(format!("fixture template: {error}").into()),
    }
}

/// Builds the template once, dropping every index handle so the copied file has no WAL.
fn build_template(drift: DriftMode) -> TestResult<Template> {
    let world = fixture_uncached(drift)?;
    let World {
        sandbox,
        database,
        state,
        migration_state,
        migration_events,
        paths,
        project_id,
        skills,
        mcps,
        rules,
        drift_event,
        ..
    } = world;
    // Closing the handles checkpoints their WAL; a copied template must be one file.
    drop((database, state, migration_state, migration_events, paths));
    let dir = sandbox.into_dir();
    let root = fs::canonicalize(dir.path())?;
    for sibling in ["agentuna.db-wal", "agentuna.db-shm"] {
        match fs::remove_file(root.join("support").join(sibling)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Template {
        _dir: dir,
        root,
        project_id,
        skills,
        mcps,
        rules,
        drift_event,
    })
}

/// One fresh sandbox cloned from the process-wide template.
fn fixture(drift: DriftMode) -> TestResult<World> {
    let template = template(drift)?;
    let sandbox = Sandbox::clone_tree(&template.root)?;
    let database = sandbox.database()?;
    let target_root = sandbox
        .home
        .parent()
        .ok_or("the sandbox home has no parent")?;
    rewrite_absolute_paths(&database, &template.root, target_root)?;
    let paths = sandbox.paths()?;
    let migration_events = Arc::new(RecordingSink::default());
    let state = flow_harness::test_state(sandbox.database()?, paths.clone());
    let mut rules = template.rules.clone();
    for rule in &mut rules {
        if let Ok(relative) = Path::new(&rule.path).strip_prefix(&template.root) {
            rule.path = target_root.join(relative).to_string_lossy().into_owned();
        }
    }
    Ok(World {
        sandbox,
        database,
        state,
        migration_state: None,
        migration_events,
        paths,
        project_id: template.project_id.clone(),
        skills: template.skills.clone(),
        mcps: template.mcps.clone(),
        rules,
        drift_event: template.drift_event,
        migration_used: false,
        next_name: 0,
        steps: Vec::new(),
    })
}

/// Rewrites every text cell carrying the template root so recorded paths point at the clone.
///
/// Items, deployments, drift events and history store absolute paths. The copy is otherwise
/// byte-identical, so replacing the one prefix in every text column keeps the index consistent
/// without hard-coding each table's schema.
fn rewrite_absolute_paths(database: &Database, from: &Path, to: &Path) -> TestResult<()> {
    let from = from
        .to_str()
        .ok_or("the template root is not valid UTF-8")?;
    let to = to.to_str().ok_or("the sandbox root is not valid UTF-8")?;
    if from == to {
        return Ok(());
    }
    let connection = database.connection();
    let tables: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for table in tables {
        let columns: Vec<String> = {
            let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for column in columns {
            connection.execute(
                &format!(
                    "UPDATE \"{table}\" SET \"{column}\" = replace(\"{column}\", ?1, ?2) \
                     WHERE typeof(\"{column}\") = 'text' AND instr(\"{column}\", ?1) > 0"
                ),
                rusqlite::params![from, to],
            )?;
        }
    }
    Ok(())
}

/// Builds one complete fixture. `drift` selects the pre-baseline external edit.
fn fixture_uncached(drift: DriftMode) -> TestResult<World> {
    let sandbox = Sandbox::new()?;
    let mut setup = sandbox.database()?;
    for agent in [AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp] {
        sandbox.register_agent(&setup, agent)?;
    }
    let project_id = sandbox.register_project(&mut setup)?;
    drop(setup);

    let state = sandbox.state()?;
    let paths = sandbox.paths()?;
    let database = sandbox.database()?;
    let migration_events = Arc::new(RecordingSink::default());
    let mut world = World {
        sandbox,
        database,
        state,
        migration_state: None,
        migration_events,
        paths,
        project_id,
        skills: Vec::new(),
        mcps: Vec::new(),
        rules: Vec::new(),
        drift_event: None,
        migration_used: false,
        next_name: 0,
        steps: Vec::new(),
    };

    for scope in [Scope::User, Scope::Global, Scope::Project] {
        let layout = StoreLayout::for_scope(&world.paths, scope)?;
        at_store::layout::init_store(scope, layout.root())?;
    }

    install(&mut world, Scope::User, "sequence-user-one", "Seq User One")?;
    install(&mut world, Scope::User, "sequence-user-two", "Seq User Two")?;
    install(
        &mut world,
        Scope::Global,
        "sequence-global-one",
        "Seq Global One",
    )?;
    install(
        &mut world,
        Scope::Project,
        "sequence-project-one",
        "Seq Project One",
    )?;
    let drift_slot = install(&mut world, Scope::User, DRIFT_SKILL, "Seq Drift")?;
    world.skills[drift_slot].dedicated_drift = true;

    // Native, unmanaged skill the migration step adopts; the frontmatter name has a space so
    // the plan has to normalize it to a store slug.
    write_bytes(
        &world
            .sandbox
            .home
            .join(".claude/skills/native-migrate/SKILL.md"),
        b"---\nname: Native Migrate\ndescription: native\n---\n\nNative body.\n",
    )?;

    seed_mcp_store(
        &mut world,
        Scope::User,
        &[
            ("Seq Alpha", "seq-alpha", AgentId::ClaudeCode),
            ("Seq Beta", "seq-beta", AgentId::Codex),
        ],
    )?;
    seed_mcp_store(
        &mut world,
        Scope::Project,
        &[("Seq Gamma", "seq-gamma", AgentId::Omp)],
    )?;

    // Format-preserving native documents with user comments and unrelated keys.
    write_bytes(
        &world.sandbox.home.join(".claude.json"),
        br#"{
  // kept by hand
  "theme": "dark",
  "mcpServers": {
    "seq-alpha": { "command": "npx", "args": ["-y", "demo-server"] }
  }
}
"#,
    )?;
    write_bytes(
        &world.sandbox.home.join(".codex/config.toml"),
        b"# kept by hand\n[profile.default]\nmodel = \"sandbox\"\n\n[mcp_servers.seq-beta]\ncommand = \"npx\"\nargs = [\"-y\", \"demo-server\"]\n",
    )?;
    write_bytes(
        &world.sandbox.project.join("mcp.json"),
        b"{\n  \"mcpServers\": {\n    \"seq-gamma\": { \"command\": \"npx\", \"args\": [\"-y\", \"demo-server\"] }\n  }\n}\n",
    )?;

    let slots = world.mcps.clone();
    for slot in &slots {
        seed_mcp_deployment(&world, slot)?;
    }

    // Two managed rule files, indexed directly like a store install leaves them.
    for (scope, path, slug, name, body) in [
        (
            Scope::User,
            world.sandbox.home.join(".claude/CLAUDE.md"),
            "claude",
            "CLAUDE.md",
            "Initial user rule body.",
        ),
        (
            Scope::Project,
            world.sandbox.project.join("AGENTS.md"),
            "agents",
            "AGENTS.md",
            "Initial project rule body.",
        ),
    ] {
        let bytes = at_fs::managed_block::render_managed_block(&path, None, body.as_bytes())?;
        write_bytes(&path, &bytes)?;
        let item_id = ItemId::new();
        let hash = at_fs::sha256_hex(&bytes);
        world.database.connection().execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, \
             content_hash, state, managed) VALUES (?1, 'rule', ?2, NULL, ?3, ?4, ?5, ?6, \
             'present', 1)",
            rusqlite::params![
                item_id.to_string(),
                scope.wire_name(),
                slug,
                name,
                path.to_string_lossy().as_ref(),
                hash,
            ],
        )?;
        world.rules.push(RuleSlot {
            item_id: item_id.to_string(),
            path: path.to_string_lossy().into_owned(),
            scope,
            live: true,
        });
    }

    if drift != DriftMode::None {
        let store = world
            .sandbox
            .layout(Scope::User)?
            .skill_dir(&Slug::parse(DRIFT_SKILL)?);
        let target = world
            .sandbox
            .agent_skills_dir(&AgentId::ClaudeCode, Scope::User)?
            .join(DRIFT_SKILL);
        std::os::unix::fs::symlink(&store, &target)?;
        let store_hash = at_store::manifest::hash_skill_folder(&store)?.to_string();
        let target_hash = at_fs::hash_node(&target)?.ok_or("the fixture link is unreadable")?;
        let target_text = target.to_string_lossy().into_owned();
        let store_text = store.to_string_lossy().into_owned();
        let hashes =
            DeploymentHashes::new(Some(&store_hash), Some(&target_hash), Some(&store_hash));
        let spec = DeploymentSpec::applied(
            &world.skills[drift_slot].item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            ItemState::Linked,
            Some("symlink"),
            Some(&target_text),
            hashes,
        )
        .with_link_target(&store_text);
        upsert_deployment(world.database.connection(), &spec)?;

        if drift == DriftMode::PreOpened {
            fs::remove_file(&target)?;
            fs::create_dir_all(&target)?;
            write_bytes(
                &target.join("SKILL.md"),
                b"---\nname: Seq Drift\n---\n\nEdited outside AgenTuna.\n",
            )?;
            let notices = flow_harness::reconcile_drift(&world.database)?;
            assert_eq!(notices.len(), 1, "the fixture edit opens exactly one event");
            world.drift_event = Some(flow_harness::open_drift_events(&world.database)?[0].id);
            // The external tool replaced the link with a folder; the deployment row that
            // described the old link is gone, exactly as a rescan leaves it.
            world.database.connection().execute(
                "DELETE FROM deployments WHERE item_id = ?1",
                [world.skills[drift_slot].item_id.to_string()],
            )?;
        }
    }

    Ok(world)
}

// ---------------------------------------------------------------------------------------------
// Index and tree snapshots
// ---------------------------------------------------------------------------------------------

/// Every user table with its rows rendered in a stable order.
fn db_fingerprint(database: &Database) -> TestResult<String> {
    let connection = database.connection();
    let mut tables = Vec::new();
    {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            tables.push(row?);
        }
    }
    let mut out = String::new();
    for table in tables {
        let mut statement = connection.prepare(&format!("SELECT * FROM \"{table}\""))?;
        let width = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(width);
                for index in 0..width {
                    let value: rusqlite::types::Value = row.get(index)?;
                    cells.push(format!("{value:?}"));
                }
                Ok(cells.join("|"))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.sort();
        out.push_str(&format!("[{table}]\n"));
        for row in rows {
            out.push_str(&row);
            out.push('\n');
        }
    }
    Ok(out)
}

/// One table's rows, minus volatile bookkeeping columns, in a stable order.
fn normalized_rows(
    database: &Database,
    table: &str,
    omit: &[&str],
    filter: &str,
) -> TestResult<Vec<String>> {
    let connection = database.connection();
    let mut columns = Vec::new();
    {
        let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            let name = row?;
            if !omit.contains(&name.as_str()) {
                columns.push(name);
            }
        }
    }
    let where_clause = if filter.is_empty() {
        String::new()
    } else {
        format!(" WHERE {filter}")
    };
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM \"{table}\"{where_clause}",
        columns.join(", ")
    ))?;
    let mut rows = statement
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                let value: rusqlite::types::Value = row.get(index)?;
                cells.push(format!("{value:?}"));
            }
            Ok(cells.join("|"))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort();
    Ok(rows)
}

fn item_rows(database: &Database) -> TestResult<Vec<String>> {
    normalized_rows(
        database,
        "items",
        &["created_at", "updated_at", "last_scanned_at"],
        "",
    )
}

/// Deployment rows that materialise something. An `off` row is the app's tombstone after an
/// undo removed a link; it is not a deployment, so it does not belong in the before/after
/// comparison.
fn deployment_rows(database: &Database) -> TestResult<Vec<String>> {
    normalized_rows(
        database,
        "deployments",
        &["id", "updated_at", "last_verified_at", "operation_id"],
        "state <> 'off'",
    )
}

/// Operation ids the sequence created, newest first; undo rows are excluded.
fn sequence_operations(database: &Database, baseline: i64) -> TestResult<Vec<String>> {
    let connection = database.connection();
    let mut statement = connection.prepare(
        "SELECT id FROM operations WHERE rowid > ?1 AND status = 'succeeded' \
         AND undo_of IS NULL ORDER BY rowid DESC",
    )?;
    let rows = statement.query_map([baseline], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

fn max_operation_rowid(database: &Database) -> TestResult<i64> {
    Ok(database.connection().query_row(
        "SELECT COALESCE(MAX(rowid), 0) FROM operations",
        [],
        |row| row.get(0),
    )?)
}

fn operation_paths(database: &Database, operation_id: &str) -> TestResult<BTreeSet<PathBuf>> {
    let connection = database.connection();
    let mut statement = connection
        .prepare("SELECT path FROM operation_files WHERE operation_id = ?1 AND path IS NOT NULL")?;
    let rows = statement.query_map([operation_id], |row| row.get::<_, String>(0))?;
    let mut paths = BTreeSet::new();
    for row in rows {
        paths.insert(PathBuf::from(row?));
    }
    Ok(paths)
}

// ---------------------------------------------------------------------------------------------
// Step execution
// ---------------------------------------------------------------------------------------------

/// Refusal codes a reviewed command may return without being a product failure.
fn is_typed_refusal(code: CommandErrorCode) -> bool {
    matches!(
        code,
        CommandErrorCode::InvalidArgument
            | CommandErrorCode::NotFound
            | CommandErrorCode::RefusedOverwrite
            | CommandErrorCode::Conflict
            | CommandErrorCode::ExternalWriteConflict
            | CommandErrorCode::Locked
            | CommandErrorCode::Unavailable
            | CommandErrorCode::Bridge
            | CommandErrorCode::CliInstall
    )
}

/// Applies one step. A typed refusal is recorded and does not abort the sequence.
async fn run_step(world: &mut World, step: &Step) -> TestResult<()> {
    let description = step.describe(world);
    let outcome = match step {
        Step::SkillToggle {
            skill,
            agent,
            enabled,
        } => skill_toggle(world, *skill, agent, *enabled),
        Step::BulkToggle {
            skills,
            agent,
            enabled,
        } => bulk_toggle(world, skills, agent, *enabled),
        Step::Transfer {
            skill,
            target,
            transfer: transfer_kind,
            resolution,
            new_slug,
        } => transfer(
            world,
            *skill,
            *target,
            transfer_kind,
            resolution.clone(),
            new_slug.clone(),
        ),
        Step::DeleteSkill { skill } => delete_skill(world, *skill),
        Step::McpAdd { scope, agent } => add_step(world, *scope, agent.clone()),
        Step::McpEdit { mcp, rename } => edit_mcp(world, *mcp, *rename),
        Step::McpToggle { mcp, enabled } => toggle_mcp(world, *mcp, *enabled),
        Step::McpDelete { mcp } => delete_mcp(world, *mcp),
        Step::McpCopy { mcp, target } => copy_mcp(world, *mcp, *target),
        Step::RuleCreate { scope, file_type } => create_rule(world, *scope, file_type),
        Step::RuleSave { rule } => save_rule_step(world, *rule),
        Step::Migrate => migrate_step(world).await,
        Step::DriftResolve { adopt } => drift_step(world, *adopt),
    };
    match outcome {
        Ok(()) => {
            world.steps.push(format!("{description} => ok"));
            Ok(())
        }
        Err(error) if is_typed_refusal(error.code) => {
            world.steps.push(format!(
                "{description} => refused {:?}: {}",
                error.code, error.message
            ));
            Ok(())
        }
        Err(error) => Err(format!("{description} failed: {error:?}").into()),
    }
}

fn skill_toggle(
    world: &World,
    skill: usize,
    agent: &AgentId,
    enabled: bool,
) -> Result<(), CommandError> {
    let slot = &world.skills[skill];
    let preview = skill_agent_preview(
        world.database.connection(),
        &world.paths,
        SkillAgentPreviewRequest {
            item_id: slot.item_id.to_string(),
            scope: scope_name(slot.scope).to_owned(),
            agent_id: agent.to_string(),
            enabled,
            project_id: project_arg(slot.scope, &world.project_id),
        },
    )?;
    skill_agent_apply(&world.state, &preview.preview_id)?;
    Ok(())
}

fn bulk_toggle(
    world: &World,
    skills: &[usize],
    agent: &AgentId,
    enabled: bool,
) -> Result<(), CommandError> {
    let mut preview_ids = Vec::new();
    for index in skills {
        let slot = &world.skills[*index];
        let preview = skill_agent_preview(
            world.database.connection(),
            &world.paths,
            SkillAgentPreviewRequest {
                item_id: slot.item_id.to_string(),
                scope: scope_name(slot.scope).to_owned(),
                agent_id: agent.to_string(),
                enabled,
                project_id: project_arg(slot.scope, &world.project_id),
            },
        )?;
        if !preview.no_op {
            preview_ids.push(preview.preview_id);
        }
    }
    flow_harness::bulk_skill_apply(
        &world.state,
        BulkSkillApplyRequest {
            preview_ids,
            label: "generative sequence".to_owned(),
        },
    )?;
    Ok(())
}

fn transfer(
    world: &mut World,
    skill: usize,
    target: Scope,
    kind: &str,
    resolution: Option<String>,
    new_slug: Option<String>,
) -> Result<(), CommandError> {
    let slot = &world.skills[skill];
    let request = SkillTransferPreviewRequest {
        item_id: slot.item_id.to_string(),
        target_scope: scope_name(target).to_owned(),
        transfer: kind.to_owned(),
        resolution,
        new_slug,
        project_id: (slot.scope == Scope::Project || target == Scope::Project)
            .then(|| world.project_id.clone()),
    };
    let preview = flow_harness::skill_transfer_preview(&world.state, request)?;
    let Some(preview_id) = preview.preview_id else {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the transfer awaits an explicit collision decision",
        ));
    };
    flow_harness::skill_transfer_apply(&world.state, SkillTransferApplyRequest { preview_id })?;
    // The transfer may change the row's scope (move) or its slug (rename); keep the slot in
    // sync with the index so later steps address the item by its actual scope.
    let current = world
        .database
        .connection()
        .query_row(
            "SELECT scope, slug FROM items WHERE id = ?1",
            [slot.item_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok();
    if let Some((scope_text, slug)) = current {
        let slot = &mut world.skills[skill];
        slot.scope = scope_text.parse().unwrap_or(target);
        slot.slug = slug;
    } else {
        // The row is gone: a move removed it, and a promote may have re-homed the identity.
        world.skills[skill].live = false;
    }
    Ok(())
}

fn delete_skill(world: &mut World, skill: usize) -> Result<(), CommandError> {
    let slot = &world.skills[skill];
    let preview = flow_harness::skill_delete_preview(
        &world.state,
        SkillDeletePreviewRequest {
            item_id: slot.item_id.to_string(),
            project_id: project_arg(slot.scope, &world.project_id),
        },
    )?;
    flow_harness::skill_delete_apply(
        &world.state,
        SkillDeleteApplyRequest {
            preview_id: preview.preview_id,
            confirmed_name: preview.name,
        },
    )?;
    world.skills[skill].live = false;
    Ok(())
}

fn add_step(world: &mut World, scope: Scope, agent: AgentId) -> Result<(), CommandError> {
    world.next_name += 1;
    let name = format!("Seq Server {}", world.next_name);
    let project = project_arg(scope, &world.project_id);
    let request = mcp_request(
        &name,
        scope,
        &agent,
        project.as_deref(),
        None,
        vec!["-y".to_owned(), "demo-server".to_owned()],
    );
    let preview = flow_harness::mcp_plan_preview(&world.state, request)?;
    flow_harness::mcp_save(
        &world.state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec![agent.to_string()],
            acknowledge_unsupported: false,
        },
    )?;
    let slot = McpSlot {
        item_id: item_id_by_name(&world.database, "mcp_server", &name)
            .map_err(|_| CommandError::internal("the saved MCP item row is missing"))?,
        scope,
        name,
        agent,
        live: true,
    };
    world.mcps.push(slot);
    Ok(())
}

fn edit_mcp(world: &mut World, mcp: usize, rename: bool) -> Result<(), CommandError> {
    let slot = &world.mcps[mcp];
    let name = if rename {
        format!("{} Renamed", slot.name)
    } else {
        slot.name.clone()
    };
    let project = project_arg(slot.scope, &world.project_id);
    let request = mcp_request(
        &name,
        slot.scope,
        &slot.agent,
        project.as_deref(),
        Some(slot.item_id.to_string()),
        vec!["-y".to_owned(), "edited-server".to_owned()],
    );
    let preview = flow_harness::mcp_plan_preview(&world.state, request)?;
    flow_harness::mcp_save(
        &world.state,
        McpSaveRequest {
            preview_id: preview.preview_id,
            selected_agents: vec![slot.agent.to_string()],
            acknowledge_unsupported: false,
        },
    )?;
    if rename {
        world.mcps[mcp].name = name;
    }
    Ok(())
}

fn action_request(world: &World, mcp: usize) -> McpServerActionRequest {
    let slot = &world.mcps[mcp];
    McpServerActionRequest {
        item_id: slot.item_id.to_string(),
        scope: scope_name(slot.scope).to_owned(),
        project_id: project_arg(slot.scope, &world.project_id),
        agent: Some(slot.agent.to_string()),
        in_repo: false,
        apply: false,
        preview_id: None,
    }
}

fn toggle_mcp(world: &World, mcp: usize, enabled: bool) -> Result<(), CommandError> {
    let base = action_request(world, mcp);
    let preview = if enabled {
        flow_harness::mcp_server_enable(&world.state, base.clone())?
    } else {
        flow_harness::mcp_server_disable(&world.state, base.clone())?
    };
    let McpServerActionOutcome::Preview { preview } = preview else {
        return Ok(());
    };
    let mut apply = base;
    apply.apply = true;
    apply.preview_id = Some(preview.preview_id);
    if enabled {
        flow_harness::mcp_server_enable(&world.state, apply)?;
    } else {
        flow_harness::mcp_server_disable(&world.state, apply)?;
    }
    Ok(())
}

fn delete_mcp(world: &mut World, mcp: usize) -> Result<(), CommandError> {
    let mut base = action_request(world, mcp);
    base.agent = None;
    let preview = flow_harness::mcp_server_delete(&world.state, base.clone())?;
    let McpServerActionOutcome::Preview { preview } = preview else {
        return Ok(());
    };
    base.apply = true;
    base.preview_id = Some(preview.preview_id);
    flow_harness::mcp_server_delete(&world.state, base)?;
    world.mcps[mcp].live = false;
    Ok(())
}

fn copy_mcp(world: &World, mcp: usize, target: Scope) -> Result<(), CommandError> {
    let slot = &world.mcps[mcp];
    let preview = flow_harness::mcp_server_copy_preview(
        &world.state,
        McpCopyPreviewRequest {
            item_id: slot.item_id.to_string(),
            scope: scope_name(slot.scope).to_owned(),
            project_id: (slot.scope == Scope::Project || target == Scope::Project)
                .then(|| world.project_id.clone()),
            target_scope: scope_name(target).to_owned(),
            on_collision: None,
            new_slug: None,
        },
    )?;
    let Some(preview_id) = preview.preview_id else {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the copy needs a collision decision",
        ));
    };
    flow_harness::mcp_server_copy_apply(&world.state, McpCopyApplyRequest { preview_id })?;
    Ok(())
}

fn create_rule(world: &mut World, scope: Scope, file_type: &str) -> Result<(), CommandError> {
    let project = project_arg(scope, &world.project_id);
    let path = flow_harness::instruction_create(
        &world.state,
        InstructionCreateRequest {
            file_type: file_type.to_owned(),
            scope: scope_name(scope).to_owned(),
        },
        project.as_deref(),
    )?;
    let item_id = item_id_by_source(&world.database, &path)
        .map_err(|_| CommandError::internal("rule item missing"))?;
    world.rules.push(RuleSlot {
        item_id,
        path,
        scope,
        live: true,
    });
    Ok(())
}

fn save_rule_step(world: &World, rule: usize) -> Result<(), CommandError> {
    let slot = &world.rules[rule];
    let current = fs::read(&slot.path)
        .map_err(|_| CommandError::new(CommandErrorCode::Unavailable, "rule file unreadable"))?;
    let rendered = at_fs::managed_block::render_managed_block(
        Path::new(&slot.path),
        Some(&current),
        format!("Saved by the sequence at step {}.", world.steps.len()).as_bytes(),
    )
    .map_err(|_| CommandError::internal("managed block failed"))?;
    let outcome = flow_harness::instruction_save(
        &world.state,
        InstructionSaveRequest {
            item_id: slot.item_id.clone(),
            path: slot.path.clone(),
            scope: scope_name(slot.scope).to_owned(),
            expected_hash: at_fs::sha256_hex(&current),
            content: String::from_utf8_lossy(&rendered).into_owned(),
            project_id: project_arg(slot.scope, &world.project_id),
            force: false,
        },
    )?;
    if outcome.status == "saved" {
        Ok(())
    } else {
        Err(CommandError::new(
            CommandErrorCode::Conflict,
            format!("rule save returned {}", outcome.status),
        ))
    }
}

async fn migrate_step(world: &mut World) -> Result<(), CommandError> {
    if world.migration_used {
        return Err(CommandError::invalid_argument(
            "selected_paths",
            "the native fixture item was already migrated",
        ));
    }
    if world.migration_state.is_none() {
        let database = world
            .sandbox
            .database()
            .map_err(|_| CommandError::internal("the migration index could not be opened"))?;
        world.migration_state = Some(
            AppState::new(
                Arc::new(JobBridge::new(
                    world.migration_events.clone(),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(world.paths.clone()),
        );
    }
    let migration_state = world
        .migration_state
        .as_ref()
        .ok_or_else(|| CommandError::internal("the migration state was not created"))?;
    let scan = migrate_scan_inner(migration_state, None).await?;
    let native = scan
        .items
        .iter()
        .find(|item| item.name == "Native Migrate")
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "the native fixture skill was not scanned",
            )
        })?;
    let selected = vec![native.path.clone()];
    let request = MigratePlanRequest {
        project_id: None,
        selected_paths: selected.clone(),
        resolutions: Vec::<MigrateResolutionView>::new(),
        strategy: "replace_with_links".to_owned(),
    };
    let plan = migrate_plan_inner(migration_state, request).await?;
    let job = migrate_apply_inner(
        migration_state,
        MigrateApplyRequest {
            project_id: None,
            selected_paths: selected,
            resolutions: Vec::new(),
            strategy: "replace_with_links".to_owned(),
            plan_hash: plan.plan_hash,
        },
    )
    .await?;
    let mut result = None;
    for _ in 0..4000 {
        match migrate_apply_result_inner(migration_state, job.as_str()) {
            Ok(view) => {
                result = Some(view);
                break;
            }
            Err(error)
                if matches!(
                    error.code,
                    CommandErrorCode::Conflict | CommandErrorCode::NotFound
                ) =>
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
    world.migration_used = true;
    result.map(|_| ()).ok_or_else(|| {
        let events = world
            .migration_events
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        CommandError::internal(format!(
            "the migration job did not finish; events: {events:?}"
        ))
    })
}

fn drift_step(world: &mut World, adopt: bool) -> Result<(), CommandError> {
    let event = world.drift_event.take().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "the fixture drift event was already resolved",
        )
    })?;
    if adopt {
        flow_harness::drift_adopt(&world.state, event)
    } else {
        flow_harness::drift_restore(&world.state, event)
    }
}

// ---------------------------------------------------------------------------------------------
// Sequence generation
// ---------------------------------------------------------------------------------------------

fn live_skill_indices(world: &World) -> Vec<usize> {
    world
        .skills
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.live && !slot.dedicated_drift)
        .map(|(index, _)| index)
        .collect()
}

fn live_mcp_indices(world: &World) -> Vec<usize> {
    world
        .mcps
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.live)
        .map(|(index, _)| index)
        .collect()
}

fn scope_agents(scope: Scope) -> Vec<AgentId> {
    match scope {
        Scope::Project => vec![AgentId::ClaudeCode, AgentId::Omp],
        _ => vec![AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp],
    }
}

/// Picks one applicable step from the live world.
fn choose_step(world: &World, rng: &mut Rng) -> Option<Step> {
    let skills = live_skill_indices(world);
    let mcps = live_mcp_indices(world);
    let mut choices: Vec<Step> = Vec::new();
    if !skills.is_empty() {
        let skill = skills[rng.below(skills.len())];
        let scope = world.skills[skill].scope;
        if matches!(scope, Scope::User | Scope::Project) {
            let agents = scope_agents(scope);
            let agent = agents[rng.below(agents.len())].clone();
            choices.push(Step::SkillToggle {
                skill,
                agent: agent.clone(),
                enabled: rng.chance(2),
            });
            let enabled = rng.chance(2);
            choices.push(Step::BulkToggle {
                skills: skills
                    .iter()
                    .copied()
                    .filter(|index| {
                        actual_scope(world, &world.skills[*index])
                            .unwrap_or(world.skills[*index].scope)
                            == scope
                    })
                    .take(2)
                    .collect(),
                agent,
                enabled,
            });
        }
        choices.push(Step::DeleteSkill { skill });
        let transfer = *rng.pick(&["promote", "move", "duplicate"]);
        let targets: Vec<Scope> = [Scope::Global, Scope::User, Scope::Project]
            .into_iter()
            .filter(|target| *target != scope)
            .collect();
        if !targets.is_empty() {
            let target = targets[rng.below(targets.len())];
            let resolution = rng
                .chance(2)
                .then(|| (*rng.pick(&["replace", "rename"])).to_owned());
            let new_slug = (transfer == "duplicate" || resolution.as_deref() == Some("rename"))
                .then(|| format!("{}-{}-{}", world.skills[skill].slug, transfer, rng.name(4)));
            choices.push(Step::Transfer {
                skill,
                target,
                transfer,
                resolution,
                new_slug,
            });
        }
    }
    if !mcps.is_empty() {
        let mcp = mcps[rng.below(mcps.len())];
        choices.push(Step::McpEdit {
            mcp,
            rename: rng.chance(2),
        });
        choices.push(Step::McpToggle {
            mcp,
            enabled: rng.chance(2),
        });
        choices.push(Step::McpDelete { mcp });
        let target = if world.mcps[mcp].scope == Scope::Project {
            Scope::User
        } else {
            Scope::Project
        };
        choices.push(Step::McpCopy { mcp, target });
    }
    choices.push(Step::McpAdd {
        scope: *rng.pick(&[Scope::User, Scope::Project]),
        agent: rng.pick(&[AgentId::ClaudeCode, AgentId::Omp]).clone(),
    });
    choices.push(Step::RuleCreate {
        scope: *rng.pick(&[Scope::User, Scope::Project]),
        file_type: rng.pick(&["AGENTS.md", "CLAUDE.md"]),
    });
    let live_rules: Vec<usize> = world
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule.live)
        .map(|(index, _)| index)
        .collect();
    if !live_rules.is_empty() {
        choices.push(Step::RuleSave {
            rule: live_rules[rng.below(live_rules.len())],
        });
    }
    if !world.migration_used {
        choices.push(Step::Migrate);
    }
    if world.drift_event.is_some() {
        choices.push(Step::DriftResolve {
            adopt: rng.chance(2),
        });
    }
    if choices.is_empty() {
        None
    } else {
        Some(choices[rng.below(choices.len())].clone())
    }
}

/// Runs one random sequence and returns the operations it created, newest first.
async fn run_sequence(world: &mut World, rng: &mut Rng, steps: usize) -> TestResult<Vec<String>> {
    let baseline = max_operation_rowid(&world.database)?;
    for _ in 0..steps {
        let Some(step) = choose_step(world, rng) else {
            break;
        };
        run_step(world, &step).await?;
    }
    sequence_operations(&world.database, baseline)
}

fn failure_context(seed: u64, world: &World) -> String {
    format!(
        "seed {seed}\nsteps:\n{}",
        world
            .steps
            .iter()
            .map(|step| format!("  - {step}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

// ---------------------------------------------------------------------------------------------
// Shared sequence body
// ---------------------------------------------------------------------------------------------

async fn undo_all(world: &World, operations: &[String], seed: u64) -> TestResult<()> {
    for operation in operations {
        let check = operation_undo_check_inner(&world.state, operation)?;
        if check.already_undone {
            continue;
        }
        operation_undo_inner(&world.state, operation, false).map_err(|error| {
            format!(
                "{}: undoing {operation} failed: {error:?}",
                failure_context(seed, world)
            )
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Parallel case runner
// ---------------------------------------------------------------------------------------------

fn seed_chunks() -> Vec<std::ops::RangeInclusive<u64>> {
    let per = SEQUENCES.div_ceil(WORKERS);
    (0..WORKERS)
        .map(|worker| {
            let start = worker * per + 1;
            let end = ((worker + 1) * per).min(SEQUENCES);
            start..=end
        })
        .filter(|range| !range.is_empty())
        .collect()
}

fn run_parallel_sync<F>(run: F) -> TestResult<()>
where
    F: Fn(u64) -> TestResult<()> + Sync,
{
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for range in seed_chunks() {
            let run = &run;
            handles.push(scope.spawn(move || -> Result<(), String> {
                for seed in range {
                    run(seed).map_err(|error| error.to_string())?;
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().map_err(|_| "worker panicked".to_owned())??;
        }
        Ok(())
    })
}

type BoxedCase = std::pin::Pin<Box<dyn std::future::Future<Output = TestResult<()>>>>;

fn run_parallel_async<F>(run: F) -> TestResult<()>
where
    F: Fn(u64) -> BoxedCase + Sync,
{
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for range in seed_chunks() {
            let run = &run;
            handles.push(scope.spawn(move || -> Result<(), String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    for seed in range {
                        run(seed).await.map_err(|error| error.to_string())?;
                    }
                    Ok(())
                })
            }));
        }
        for handle in handles {
            handle.join().map_err(|_| "worker panicked".to_owned())??;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------------------------
// Property 1: undo in reverse order restores the exact original tree and index
// ---------------------------------------------------------------------------------------------

/// Compares two row sets and prints which rows are missing or extra.
fn assert_row_set(before: &[String], after: &[String], table: &str, seed: u64, world: &World) {
    let missing: Vec<&String> = before.iter().filter(|row| !after.contains(row)).collect();
    let extra: Vec<&String> = after.iter().filter(|row| !before.contains(row)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{table} rows must match the pre-sequence index\n{}\nmissing:\n{}\nextra:\n{}",
        failure_context(seed, world),
        missing
            .iter()
            .map(|row| row.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        extra
            .iter()
            .map(|row| row.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

async fn undo_property_case(seed: u64, steps: usize, with_drift: bool) -> TestResult<()> {
    let mut rng = Rng::new(seed);
    let mut world = fixture(if with_drift {
        DriftMode::PreOpened
    } else {
        DriftMode::None
    })?;
    let before_tree = world.sandbox.snapshot()?;
    let before_items = item_rows(&world.database)?;
    let before_deployments = deployment_rows(&world.database)?;

    let baseline = max_operation_rowid(&world.database)?;
    let mut operations = run_sequence(&mut world, &mut rng, steps).await?;
    if operations.is_empty() {
        // Generated steps may all be typed refusals or no-ops (disabling a skill that was never
        // deployed). Keep the property's precondition with one reviewed regular-file write.
        save_rule_step(&world, 0)?;
        operations = sequence_operations(&world.database, baseline)?;
    }
    assert!(
        !operations.is_empty(),
        "every generated sequence must create at least one operation\n{}",
        failure_context(seed, &world)
    );

    undo_all(&world, &operations, seed).await?;

    let after_tree = world.sandbox.snapshot()?;
    let difference = before_tree.difference(&after_tree);
    assert!(
        difference.is_empty(),
        "undo of every operation must restore the exact tree\n{}\ntree diff:\n{difference}",
        failure_context(seed, &world)
    );
    let after_items = item_rows(&world.database)?;
    assert_row_set(&before_items, &after_items, "items", seed, &world);
    let after_deployments = deployment_rows(&world.database)?;
    assert_row_set(
        &before_deployments,
        &after_deployments,
        "deployments",
        seed,
        &world,
    );
    Ok(())
}

#[test]
fn undo_in_reverse_order_restores_the_exact_original_tree() -> TestResult<()> {
    run_parallel_async(|seed| {
        Box::pin(undo_property_case(
            seed,
            3 + (seed as usize % 6),
            seed % 5 == 0,
        ))
    })
}

// ---------------------------------------------------------------------------------------------
// Property 2: an external edit between an operation and its undo is never clobbered
// ---------------------------------------------------------------------------------------------

/// A path the undo conflict check can actually refuse on: a regular file that is not one of the
/// shared store documents the undo pipeline deliberately merges entry by entry.
fn eligible_external_path(path: &Path, home: &Path, project: &Path, global: &Path) -> bool {
    if !(path.starts_with(home) || path.starts_with(project) || path.starts_with(global)) {
        return false;
    }
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "store.json" | "mcp.json" | "manifest.json"))
    {
        return false;
    }
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Writes content a human or another tool could plausibly write, keeping JSON/TOML valid.
fn external_edit(path: &Path) -> TestResult<Vec<u8>> {
    let current = fs::read(path)?;
    let edited = match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => {
            let mut value: serde_json::Value =
                serde_json::from_slice(&current).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert(String::from("__external_edit"), serde_json::json!(true));
            }
            serde_json::to_vec_pretty(&value)?
        }
        Some("toml") => {
            let mut edited = current.clone();
            edited.extend_from_slice(b"\n# edited outside AgenTuna\n");
            edited
        }
        _ => {
            let mut edited = current.clone();
            edited.extend_from_slice(b"\n\nEdited outside AgenTuna.\n");
            edited
        }
    };
    fs::write(path, &edited)?;
    Ok(edited)
}

/// Every operation, newest first, that wrote a surviving regular file an external edit can hit.
fn inspectable_files(world: &World, operations: &[String]) -> TestResult<Vec<(usize, PathBuf)>> {
    let mut candidates: Vec<(usize, PathBuf)> = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, operation) in operations.iter().enumerate() {
        for path in operation_paths(&world.database, operation)? {
            if !seen.insert(path.clone()) {
                continue;
            }
            if eligible_external_path(
                &path,
                &world.sandbox.home,
                &world.sandbox.project,
                &world.sandbox.global,
            ) {
                candidates.push((index, path));
            }
        }
    }
    Ok(candidates)
}

async fn external_edit_case(seed: u64, steps: usize) -> TestResult<()> {
    let mut rng = Rng::new(seed ^ 0x51eed);
    let mut world = fixture(DriftMode::None)?;
    let baseline = max_operation_rowid(&world.database)?;
    let mut operations = run_sequence(&mut world, &mut rng, steps).await?;
    if operations.is_empty() {
        skill_toggle(&world, 0, &AgentId::ClaudeCode, true)?;
        operations = sequence_operations(&world.database, baseline)?;
    }
    // Guarantee at least one surviving regular file the undo can refuse on: a rule save writes
    // the managed document, unlike an enable/disable that only links a directory.
    if operations.is_empty() {
        save_rule_step(&world, 0)?;
        operations = sequence_operations(&world.database, baseline)?;
    }
    assert!(!operations.is_empty());

    // The newest operation that wrote each surviving regular file, with the file that op wrote.
    let mut candidates = inspectable_files(&world, &operations)?;
    if candidates.is_empty() {
        // A generated sequence may only have linked directories or been refused; one reviewed
        // rule save provides the regular-file write this property needs.
        save_rule_step(&world, 0)?;
        operations = sequence_operations(&world.database, baseline)?;
        candidates = inspectable_files(&world, &operations)?;
    }
    assert!(
        !candidates.is_empty(),
        "the sequence must have written at least one inspectable file\n{}",
        failure_context(seed, &world)
    );
    let (newest_index, edited_path) = candidates[rng.below(candidates.len())].clone();
    let edited_bytes = external_edit(&edited_path)?;

    // The undo that wrote that path must refuse and name it, leaving every byte alone.
    let before_attempt = world.sandbox.snapshot()?;
    let before_db = db_fingerprint(&world.database)?;
    let error = operation_undo_inner(&world.state, &operations[newest_index], false)
        .expect_err("an external edit after the operation must refuse the undo");
    assert_eq!(error.code, CommandErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains(&edited_path.display().to_string()),
        "the refusal must name the changed path\n{}\nerror: {error:?}",
        failure_context(seed, &world)
    );
    before_attempt.assert_unchanged(
        &world.sandbox.snapshot()?,
        "a refused undo with an external edit",
    );
    assert_eq!(
        db_fingerprint(&world.database)?,
        before_db,
        "a refused undo must not write the index\n{}",
        failure_context(seed, &world)
    );
    assert_eq!(
        fs::read(&edited_path)?,
        edited_bytes,
        "the external edit must survive the refusal\n{}",
        failure_context(seed, &world)
    );

    // An older operation whose files no newer operation still owns undoes cleanly and leaves
    // the external edit intact: the refusal is about the conflicting path, not a dead end.
    for index in (newest_index + 1)..operations.len() {
        let paths = operation_paths(&world.database, &operations[index])?;
        if paths.contains(&edited_path) {
            continue;
        }
        let mut newer_paths = BTreeSet::new();
        for operation in operations.iter().take(index) {
            newer_paths.extend(operation_paths(&world.database, operation)?);
        }
        if !paths.is_disjoint(&newer_paths) {
            continue;
        }
        operation_undo_inner(&world.state, &operations[index], false).map_err(|error| {
            format!(
                "{}: undoing independent operation {} failed: {error:?}",
                failure_context(seed, &world),
                operations[index]
            )
        })?;
        assert_eq!(
            fs::read(&edited_path)?,
            edited_bytes,
            "undoing an unrelated operation must not touch the external edit\n{}",
            failure_context(seed, &world)
        );
        break;
    }
    Ok(())
}

#[test]
fn an_external_edit_between_an_operation_and_its_undo_is_never_clobbered() -> TestResult<()> {
    run_parallel_async(|seed| Box::pin(external_edit_case(seed, 3 + (seed as usize % 6))))
}

// ---------------------------------------------------------------------------------------------
// Property 3: no open drift events; a genuine later edit is detected exactly once
// ---------------------------------------------------------------------------------------------

async fn drift_case(seed: u64) -> TestResult<()> {
    let mut rng = Rng::new(seed ^ 0x5eed);
    let mut world = fixture(DriftMode::Deployed)?;
    assert!(
        flow_harness::reconcile_drift(&world.database)?.is_empty(),
        "the fixture commands left drift open\n{}",
        failure_context(seed, &world)
    );
    let baseline = max_operation_rowid(&world.database)?;

    let steps = 3 + (seed as usize % 6);
    for _ in 0..steps {
        let Some(step) = choose_step(&world, &mut rng) else {
            break;
        };
        let description = step.describe(&world);
        if let Err(error) = run_step(&mut world, &step).await {
            return Err(format!(
                "{}\nstep {description} failed: {error}",
                failure_context(seed, &world)
            )
            .into());
        }
        let notices = flow_harness::reconcile_drift(&world.database)?;
        assert!(
            notices.is_empty(),
            "a successful command opened drift\n{}\nnotices: {notices:?}",
            failure_context(seed, &world)
        );
        assert!(
            flow_harness::open_drift_events(&world.database)?.is_empty(),
            "a successful command left an open drift event\n{}",
            failure_context(seed, &world)
        );
    }

    let operations = sequence_operations(&world.database, baseline)?;
    for operation in &operations {
        let check = operation_undo_check_inner(&world.state, operation)?;
        if check.already_undone {
            continue;
        }
        operation_undo_inner(&world.state, operation, false).map_err(|error| {
            format!(
                "{}: undoing {operation} failed: {error:?}",
                failure_context(seed, &world)
            )
        })?;
        let notices = flow_harness::reconcile_drift(&world.database)?;
        assert!(
            notices.is_empty(),
            "undo of {operation} opened drift\n{}\nnotices: {notices:?}",
            failure_context(seed, &world)
        );
        assert!(
            flow_harness::open_drift_events(&world.database)?.is_empty(),
            "undo of {operation} left an open drift event\n{}",
            failure_context(seed, &world)
        );
    }

    // A genuinely new external edit after the sequence is detected exactly once.
    let target = world
        .sandbox
        .agent_skills_dir(&AgentId::ClaudeCode, Scope::User)?
        .join(DRIFT_SKILL);
    fs::remove_file(&target)?;
    fs::create_dir_all(&target)?;
    write_bytes(
        &target.join("SKILL.md"),
        b"---\nname: Seq Drift\n---\n\nArbitrary later edit.\n",
    )?;
    let notices = flow_harness::reconcile_drift(&world.database)?;
    assert_eq!(
        notices.len(),
        1,
        "a later genuine edit opens exactly one event\n{}",
        failure_context(seed, &world)
    );
    let open = flow_harness::open_drift_events(&world.database)?;
    assert_eq!(open.len(), 1, "the event is stored once");
    let again = flow_harness::reconcile_drift(&world.database)?;
    assert!(
        again.is_empty(),
        "a reconciled edit must not open a second event: {again:?}"
    );
    Ok(())
}

#[test]
fn no_drift_survives_any_sequence_and_a_later_edit_is_detected_once() -> TestResult<()> {
    run_parallel_async(|seed| Box::pin(drift_case(seed)))
}

// ---------------------------------------------------------------------------------------------
// Property 4: refusals are free
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum RefusalKind {
    Locked,
    StalePreview,
    UnmanagedCollision,
    ReadOnly,
    MissingConfirmation,
}

/// Performs one deliberately-refused command. The caller snapshotted the tree and index after
/// the setup mutation; a refusal must leave both byte-identical.
fn refusal(result: Result<(), CommandError>, what: &str) -> TestResult<CommandError> {
    match result {
        Err(error) => Ok(error),
        Ok(()) => Err(CommandError::internal(format!("{what} did not refuse")).into()),
    }
}

fn perform_refusal(world: &mut World, kind: RefusalKind) -> TestResult<CommandError> {
    match kind {
        RefusalKind::Locked => {
            let slot = &world.skills[0];
            at_db::locks::upsert_lock(
                world.database.connection(),
                &at_db::locks::LockSpec {
                    item_id: &slot.item_id,
                    scope: Scope::Global,
                    reason: Some("pinned by policy"),
                    locked_by: Some("sandbox"),
                },
            )
            .map_err(|_| CommandError::internal("lock insert failed"))?;
            let result = skill_agent_preview(
                world.database.connection(),
                &world.paths,
                SkillAgentPreviewRequest {
                    item_id: slot.item_id.to_string(),
                    scope: "user".to_owned(),
                    agent_id: "claude_code".to_owned(),
                    enabled: true,
                    project_id: None,
                },
            )
            .map(|_| ());
            at_db::locks::remove_lock(world.database.connection(), &slot.item_id, Scope::Global)
                .map_err(|_| CommandError::internal("lock cleanup failed"))?;
            refusal(result, "the lock")
        }
        RefusalKind::StalePreview => {
            let slot = &world.skills[1];
            let preview = skill_agent_preview(
                world.database.connection(),
                &world.paths,
                SkillAgentPreviewRequest {
                    item_id: slot.item_id.to_string(),
                    scope: "user".to_owned(),
                    agent_id: "claude_code".to_owned(),
                    enabled: true,
                    project_id: None,
                },
            )?;
            let package = world
                .sandbox
                .layout(Scope::User)?
                .skill_dir(&Slug::parse(&slot.slug)?)
                .join("SKILL.md");
            let original = fs::read(&package)?;
            fs::write(
                &package,
                b"---\nname: Seq User Two\n---\n\nChanged after review.\n",
            )?;
            let result = skill_agent_apply(&world.state, &preview.preview_id).map(|_| ());
            fs::write(&package, &original)?;
            refusal(result, "the stale skill preview")
        }
        RefusalKind::UnmanagedCollision => {
            let slot = &world.skills[0];
            let target = world
                .sandbox
                .agent_skills_dir(&AgentId::ClaudeCode, Scope::User)?
                .join(&slot.slug);
            fs::create_dir_all(&target)?;
            fs::write(
                target.join("SKILL.md"),
                b"---\nname: Not Managed\n---\n\nUnmanaged body.\n",
            )?;
            let result = skill_agent_preview(
                world.database.connection(),
                &world.paths,
                SkillAgentPreviewRequest {
                    item_id: slot.item_id.to_string(),
                    scope: "user".to_owned(),
                    agent_id: "claude_code".to_owned(),
                    enabled: true,
                    project_id: None,
                },
            )
            .map(|_| ());
            fs::remove_dir_all(&target)?;
            refusal(result, "an unmanaged target")
        }
        RefusalKind::ReadOnly => {
            let slot = &world.mcps[0];
            let native =
                world
                    .sandbox
                    .agent_path(&AgentId::ClaudeCode, ItemKind::McpServer, Scope::User)?;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&native, fs::Permissions::from_mode(0o444))?;
            let project = project_arg(slot.scope, &world.project_id);
            let request = mcp_request(
                &slot.name,
                slot.scope,
                &slot.agent,
                project.as_deref(),
                Some(slot.item_id.to_string()),
                vec!["-y".to_owned(), "read-only-edit".to_owned()],
            );
            let result =
                flow_harness::mcp_plan_preview(&world.state, request).and_then(|preview| {
                    flow_harness::mcp_save(
                        &world.state,
                        McpSaveRequest {
                            preview_id: preview.preview_id,
                            selected_agents: vec![slot.agent.to_string()],
                            acknowledge_unsupported: false,
                        },
                    )
                    .map(|_| ())
                });
            fs::set_permissions(&native, fs::Permissions::from_mode(0o644))?;
            refusal(result, "a read-only native config")
        }
        RefusalKind::MissingConfirmation => {
            let slot = &world.skills[1];
            let preview = flow_harness::skill_delete_preview(
                &world.state,
                SkillDeletePreviewRequest {
                    item_id: slot.item_id.to_string(),
                    project_id: None,
                },
            )?;
            let result = flow_harness::skill_delete_apply(
                &world.state,
                SkillDeleteApplyRequest {
                    preview_id: preview.preview_id,
                    confirmed_name: "Definitely Not The Name".to_owned(),
                },
            )
            .map(|_| ());
            refusal(result, "the missing confirmation")
        }
    }
}

fn expected_refusal_codes(kind: RefusalKind) -> &'static [CommandErrorCode] {
    match kind {
        RefusalKind::Locked => &[CommandErrorCode::Locked],
        RefusalKind::StalePreview => &[CommandErrorCode::Conflict],
        RefusalKind::UnmanagedCollision => &[
            CommandErrorCode::RefusedOverwrite,
            CommandErrorCode::Conflict,
        ],
        RefusalKind::ReadOnly => &[
            CommandErrorCode::InvalidArgument,
            CommandErrorCode::RefusedOverwrite,
            CommandErrorCode::Conflict,
        ],
        RefusalKind::MissingConfirmation => &[CommandErrorCode::InvalidArgument],
    }
}

fn refusal_case(seed: u64) -> TestResult<()> {
    let kinds = [
        RefusalKind::Locked,
        RefusalKind::StalePreview,
        RefusalKind::UnmanagedCollision,
        RefusalKind::ReadOnly,
        RefusalKind::MissingConfirmation,
    ];
    let mut rng = Rng::new(seed ^ 0x2ef5);
    let mut world = fixture(DriftMode::None)?;
    let steps = 3 + (seed as usize % 6);
    for _ in 0..steps {
        let kind = kinds[rng.below(kinds.len())];
        let before_tree = world.sandbox.snapshot()?;
        let before_db = db_fingerprint(&world.database)?;
        let error = perform_refusal(&mut world, kind).unwrap_or_else(|error| {
            panic!(
                "seed {seed} refusal {kind:?} did not refuse cleanly: {error:?}\nsteps:\n{}",
                world.steps.join("\n")
            )
        });
        assert!(
            expected_refusal_codes(kind).contains(&error.code),
            "seed {seed} refusal {kind:?} returned an unexpected code: {error:?}\nsteps:\n{}",
            world.steps.join("\n")
        );
        before_tree.assert_unchanged(
            &world.sandbox.snapshot()?,
            &format!("seed {seed}: a refused {kind:?} command"),
        );
        assert_eq!(
            db_fingerprint(&world.database)?,
            before_db,
            "seed {seed}: a refused {kind:?} command wrote the index"
        );
    }
    Ok(())
}

#[test]
fn every_typed_refusal_leaves_the_tree_and_index_byte_identical() -> TestResult<()> {
    run_parallel_sync(refusal_case)
}

// ---------------------------------------------------------------------------------------------
// Property 5: double apply and double undo are safe
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Replay {
    Skill(String),
    Bulk(Vec<String>),
    Transfer(String),
    Delete {
        preview_id: String,
        name: String,
    },
    McpSave {
        preview_id: String,
        agents: Vec<String>,
    },
    McpCopy(String),
    McpAction {
        enable: bool,
        request: McpServerActionRequest,
        preview_id: String,
    },
}

impl Replay {
    fn apply_again(&self, world: &World) -> Result<(), CommandError> {
        match self {
            Self::Skill(preview_id) => skill_agent_apply(&world.state, preview_id).map(|_| ()),
            Self::Bulk(preview_ids) => flow_harness::bulk_skill_apply(
                &world.state,
                BulkSkillApplyRequest {
                    preview_ids: preview_ids.clone(),
                    label: "replayed batch".to_owned(),
                },
            )
            .map(|_| ()),
            Self::Transfer(preview_id) => flow_harness::skill_transfer_apply(
                &world.state,
                SkillTransferApplyRequest {
                    preview_id: preview_id.clone(),
                },
            )
            .map(|_| ()),
            Self::Delete { preview_id, name } => flow_harness::skill_delete_apply(
                &world.state,
                SkillDeleteApplyRequest {
                    preview_id: preview_id.clone(),
                    confirmed_name: name.clone(),
                },
            )
            .map(|_| ()),
            Self::McpSave { preview_id, agents } => flow_harness::mcp_save(
                &world.state,
                McpSaveRequest {
                    preview_id: preview_id.clone(),
                    selected_agents: agents.clone(),
                    acknowledge_unsupported: false,
                },
            )
            .map(|_| ()),
            Self::McpCopy(preview_id) => flow_harness::mcp_server_copy_apply(
                &world.state,
                McpCopyApplyRequest {
                    preview_id: preview_id.clone(),
                },
            )
            .map(|_| ()),
            Self::McpAction {
                enable,
                request,
                preview_id,
            } => {
                let mut request = request.clone();
                request.apply = true;
                request.preview_id = Some(preview_id.clone());
                if *enable {
                    flow_harness::mcp_server_enable(&world.state, request).map(|_| ())
                } else {
                    flow_harness::mcp_server_disable(&world.state, request).map(|_| ())
                }
            }
        }
    }
}

fn actual_scope(world: &World, slot: &SkillSlot) -> Option<Scope> {
    world
        .database
        .connection()
        .query_row(
            "SELECT scope FROM items WHERE id = ?1",
            [slot.item_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|text| text.parse().ok())
}

fn pick_live(world: &World, rng: &mut Rng) -> Option<usize> {
    let live = live_skill_indices(world);
    if live.is_empty() {
        None
    } else {
        Some(live[rng.below(live.len())])
    }
}

fn replayable_step(world: &mut World, rng: &mut Rng) -> TestResult<Option<Replay>> {
    let mut kinds = vec![0u8, 1, 2, 3, 4, 5, 6];
    let rotations = rng.below(kinds.len());
    kinds.rotate_left(rotations);
    for kind in kinds {
        world.steps.push(format!("replay kind {kind}"));
        match kind {
            0 => {
                let Some(skill) = pick_live(world, rng) else {
                    continue;
                };
                let slot = &world.skills[skill];
                let scope = actual_scope(world, slot).unwrap_or(slot.scope);
                if !matches!(scope, Scope::User | Scope::Project) {
                    continue;
                }
                let agents = scope_agents(scope);
                let agent = agents[rng.below(agents.len())].clone();
                let preview = skill_agent_preview(
                    world.database.connection(),
                    &world.paths,
                    SkillAgentPreviewRequest {
                        item_id: slot.item_id.to_string(),
                        scope: scope_name(slot.scope).to_owned(),
                        agent_id: agent.to_string(),
                        enabled: rng.chance(2),
                        project_id: project_arg(slot.scope, &world.project_id),
                    },
                )?;
                if preview.no_op {
                    continue;
                }
                let preview_id = preview.preview_id;
                skill_agent_apply(&world.state, &preview_id)?;
                return Ok(Some(Replay::Skill(preview_id)));
            }
            1 => {
                let Some(skill) = pick_live(world, rng) else {
                    continue;
                };
                let slot = &world.skills[skill];
                let actual_scope: Option<String> = world
                    .database
                    .connection()
                    .query_row(
                        "SELECT scope FROM items WHERE id = ?1",
                        [slot.item_id.to_string()],
                        |row| row.get(0),
                    )
                    .ok();
                if slot.scope == Scope::Global || actual_scope.as_deref() == Some("global") {
                    continue;
                }
                let preview = flow_harness::skill_delete_preview(
                    &world.state,
                    SkillDeletePreviewRequest {
                        item_id: slot.item_id.to_string(),
                        project_id: project_arg(slot.scope, &world.project_id),
                    },
                )?;
                let name = preview.name.clone();
                let preview_id = preview.preview_id;
                flow_harness::skill_delete_apply(
                    &world.state,
                    SkillDeleteApplyRequest {
                        preview_id: preview_id.clone(),
                        confirmed_name: name.clone(),
                    },
                )?;
                world.skills[skill].live = false;
                return Ok(Some(Replay::Delete { preview_id, name }));
            }
            2 => {
                let Some(skill) = pick_live(world, rng) else {
                    continue;
                };
                let slot = &world.skills[skill];
                let scope = actual_scope(world, slot).unwrap_or(slot.scope);
                if !matches!(scope, Scope::User | Scope::Project) {
                    continue;
                }
                let agents = scope_agents(scope);
                let agent = agents[rng.below(agents.len())].clone();
                let enabled = rng.chance(2);
                let mut preview_ids = Vec::new();
                for index in live_skill_indices(world)
                    .into_iter()
                    .filter(|index| {
                        actual_scope(world, &world.skills[*index])
                            .unwrap_or(world.skills[*index].scope)
                            == scope
                    })
                    .take(2)
                {
                    let slot = &world.skills[index];
                    let preview = skill_agent_preview(
                        world.database.connection(),
                        &world.paths,
                        SkillAgentPreviewRequest {
                            item_id: slot.item_id.to_string(),
                            scope: scope_name(scope).to_owned(),
                            agent_id: agent.to_string(),
                            enabled,
                            project_id: project_arg(scope, &world.project_id),
                        },
                    )?;
                    if !preview.no_op {
                        preview_ids.push(preview.preview_id);
                    }
                }
                if preview_ids.is_empty() {
                    continue;
                }
                flow_harness::bulk_skill_apply(
                    &world.state,
                    BulkSkillApplyRequest {
                        preview_ids: preview_ids.clone(),
                        label: "replayable batch".to_owned(),
                    },
                )?;
                return Ok(Some(Replay::Bulk(preview_ids)));
            }
            3 => {
                let Some(skill) = pick_live(world, rng) else {
                    continue;
                };
                let slot = &world.skills[skill];
                let scope = actual_scope(world, slot).unwrap_or(slot.scope);
                if scope == Scope::Global {
                    continue;
                }
                let target = Scope::Global;
                let request = SkillTransferPreviewRequest {
                    item_id: slot.item_id.to_string(),
                    target_scope: scope_name(target).to_owned(),
                    transfer: "promote".to_owned(),
                    resolution: None,
                    new_slug: None,
                    project_id: (slot.scope == Scope::Project || target == Scope::Project)
                        .then(|| world.project_id.clone()),
                };
                let preview = flow_harness::skill_transfer_preview(&world.state, request)?;
                let Some(preview_id) = preview.preview_id else {
                    continue;
                };
                flow_harness::skill_transfer_apply(
                    &world.state,
                    SkillTransferApplyRequest {
                        preview_id: preview_id.clone(),
                    },
                )?;
                return Ok(Some(Replay::Transfer(preview_id)));
            }
            4 => {
                let mcps = live_mcp_indices(world);
                if mcps.is_empty() {
                    continue;
                }
                let mcp = mcps[rng.below(mcps.len())];
                let slot = &world.mcps[mcp];
                let project = project_arg(slot.scope, &world.project_id);
                let request = mcp_request(
                    &slot.name.clone(),
                    slot.scope,
                    &slot.agent,
                    project.as_deref(),
                    Some(slot.item_id.to_string()),
                    vec!["-y".to_owned(), "replayed-server".to_owned()],
                );
                let preview = flow_harness::mcp_plan_preview(&world.state, request)?;
                let preview_id = preview.preview_id;
                let agents = vec![slot.agent.to_string()];
                flow_harness::mcp_save(
                    &world.state,
                    McpSaveRequest {
                        preview_id: preview_id.clone(),
                        selected_agents: agents.clone(),
                        acknowledge_unsupported: false,
                    },
                )?;
                return Ok(Some(Replay::McpSave { preview_id, agents }));
            }
            5 => {
                let mcps = live_mcp_indices(world);
                if mcps.is_empty() {
                    continue;
                }
                let mcp = mcps[rng.below(mcps.len())];
                let slot = &world.mcps[mcp];
                let target = if slot.scope == Scope::Project {
                    Scope::User
                } else {
                    Scope::Project
                };
                // Copying the same slug twice into one scope is a duplicate id, not a reviewable
                // collision; skip that destination for this case.
                let slug: Option<String> = world
                    .database
                    .connection()
                    .query_row(
                        "SELECT slug FROM items WHERE id = ?1",
                        [slot.item_id.to_string()],
                        |row| row.get(0),
                    )
                    .ok();
                let Some(slug) = slug else {
                    continue;
                };
                let duplicates: i64 = world
                    .database
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM items WHERE kind = 'mcp_server' AND scope = ?1 \
                         AND slug = ?2",
                        rusqlite::params![scope_name(target), slug],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if duplicates > 0 {
                    continue;
                }
                let preview = flow_harness::mcp_server_copy_preview(
                    &world.state,
                    McpCopyPreviewRequest {
                        item_id: slot.item_id.to_string(),
                        scope: scope_name(slot.scope).to_owned(),
                        project_id: (slot.scope == Scope::Project || target == Scope::Project)
                            .then(|| world.project_id.clone()),
                        target_scope: scope_name(target).to_owned(),
                        on_collision: None,
                        new_slug: None,
                    },
                )?;
                let Some(preview_id) = preview.preview_id else {
                    continue;
                };
                flow_harness::mcp_server_copy_apply(
                    &world.state,
                    McpCopyApplyRequest {
                        preview_id: preview_id.clone(),
                    },
                )?;
                return Ok(Some(Replay::McpCopy(preview_id)));
            }
            _ => {
                let mcps = live_mcp_indices(world);
                if mcps.is_empty() {
                    continue;
                }
                let mcp = mcps[rng.below(mcps.len())];
                let base = action_request(world, mcp);
                let state: Option<String> = world
                    .database
                    .connection()
                    .query_row(
                        "SELECT state FROM deployments WHERE item_id = ?1 AND agent_id = ?2 \
                         AND scope = ?3 AND COALESCE(project_id, '') = COALESCE(?4, '')",
                        rusqlite::params![
                            world.mcps[mcp].item_id.to_string(),
                            world.mcps[mcp].agent.to_string(),
                            scope_name(world.mcps[mcp].scope),
                            project_arg(world.mcps[mcp].scope, &world.project_id),
                        ],
                        |row| row.get(0),
                    )
                    .ok();
                if state.as_deref() != Some("linked") {
                    continue;
                }
                let preview = flow_harness::mcp_server_disable(&world.state, base.clone())?;
                let McpServerActionOutcome::Preview { preview } = preview else {
                    continue;
                };
                let mut apply = base.clone();
                apply.apply = true;
                apply.preview_id = Some(preview.preview_id.clone());
                flow_harness::mcp_server_disable(&world.state, apply)?;
                return Ok(Some(Replay::McpAction {
                    enable: false,
                    request: base,
                    preview_id: preview.preview_id,
                }));
            }
        }
    }
    Ok(None)
}

fn replay_case(seed: u64) -> TestResult<()> {
    let mut rng = Rng::new(seed ^ 0x4a11);
    let mut world = fixture(DriftMode::None)?;
    let baseline = max_operation_rowid(&world.database)?;
    let mut applied = 0;
    let steps = 3 + (seed as usize % 6);
    while applied < steps {
        let Some(replay) = replayable_step(&mut world, &mut rng).map_err(|error| {
            format!(
                "{}\nseed {seed} applied {applied}: {error}",
                failure_context(seed, &world)
            )
        })?
        else {
            break;
        };
        let before_retry = world.sandbox.snapshot()?;
        let before_db = db_fingerprint(&world.database)?;
        let error = replay
            .apply_again(&world)
            .expect_err("a consumed preview must be rejected on replay");
        assert!(
            is_typed_refusal(error.code),
            "seed {seed}: a replayed apply returned an internal error: {error:?}\nsteps:\n{}",
            world.steps.join("\n")
        );
        before_retry.assert_unchanged(
            &world.sandbox.snapshot()?,
            &format!("seed {seed}: a replayed apply"),
        );
        assert_eq!(
            db_fingerprint(&world.database)?,
            before_db,
            "seed {seed}: a replayed apply wrote the index"
        );
        applied += 1;
    }

    let operations = sequence_operations(&world.database, baseline)?;
    assert!(
        !operations.is_empty(),
        "seed {seed}: the replay property must create operations"
    );
    undo_all_sync(&world, &operations)?;

    // Undoing a second time is a typed refusal and writes nothing.
    let before_retry = world.sandbox.snapshot()?;
    let before_db = db_fingerprint(&world.database)?;
    let error = operation_undo_inner(&world.state, &operations[0], false)
        .expect_err("a second undo must be refused");
    assert_eq!(error.code, CommandErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains("already undone"),
        "the double undo must be the typed already-undone refusal: {error:?}"
    );
    before_retry.assert_unchanged(
        &world.sandbox.snapshot()?,
        "a second undo of the same operation",
    );
    assert_eq!(
        db_fingerprint(&world.database)?,
        before_db,
        "a second undo must not write the index"
    );
    Ok(())
}

#[test]
fn repeating_an_apply_and_an_undo_is_rejected_without_touching_files() -> TestResult<()> {
    run_parallel_sync(replay_case)
}

fn undo_all_sync(world: &World, operations: &[String]) -> TestResult<()> {
    for operation in operations {
        operation_undo_inner(&world.state, operation, false)?;
    }
    Ok(())
}

#[test]
fn mcp_copy_out_of_project_does_not_stamp_the_source_project() -> TestResult<()> {
    let world = fixture(DriftMode::None)?;
    let gamma = world
        .mcps
        .iter()
        .position(|slot| slot.name == "Seq Gamma")
        .expect("project server");
    copy_mcp(&world, gamma, Scope::User)?;
    let copied_project: Option<String> = world
        .database
        .connection()
        .query_row(
            "SELECT project_id FROM items WHERE kind = 'mcp_server' AND scope = 'user' \
             AND slug = 'seq-gamma'",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(copied_project, None);
    let operations = sequence_operations(&world.database, 0)?;
    undo_all_sync(&world, &operations)?;
    Ok(())
}

#[test]
fn mcp_rename_preserves_item_identity_and_deployment_rows() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    let alpha = world
        .mcps
        .iter()
        .position(|slot| slot.name == "Seq Alpha")
        .expect("user server");
    let before_id = world.mcps[alpha].item_id;
    let before_items = item_rows(&world.database)?;
    let before_deployments = deployment_rows(&world.database)?;

    edit_mcp(&mut world, alpha, true)?;

    let (id, slug): (String, String) = world.database.connection().query_row(
        "SELECT id, slug FROM items WHERE kind = 'mcp_server' AND scope = 'user' \
             AND name = 'Seq Alpha Renamed'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(id, before_id.to_string(), "a rename keeps the item id");
    assert_eq!(slug, "seq-alpha-renamed");
    let old_rows: i64 = world.database.connection().query_row(
        "SELECT COUNT(*) FROM items WHERE kind = 'mcp_server' AND scope = 'user' \
         AND slug = 'seq-alpha'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(old_rows, 0, "the old slug does not keep a second item row");
    let deployments: i64 = world.database.connection().query_row(
        "SELECT COUNT(*) FROM deployments WHERE item_id = ?1",
        [before_id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(
        deployments, 1,
        "the deployment stays attached to the identity"
    );

    let operations = sequence_operations(&world.database, 0)?;
    undo_all_sync(&world, &operations)?;
    assert_row_set(
        &before_items,
        &item_rows(&world.database)?,
        "items",
        0,
        &world,
    );
    assert_row_set(
        &before_deployments,
        &deployment_rows(&world.database)?,
        "deployments",
        0,
        &world,
    );
    Ok(())
}

#[test]
fn mcp_enable_records_native_mode() -> TestResult<()> {
    let world = fixture(DriftMode::None)?;
    let gamma = world
        .mcps
        .iter()
        .position(|slot| slot.name == "Seq Gamma")
        .expect("project server");
    toggle_mcp(&world, gamma, false)?;
    toggle_mcp(&world, gamma, true)?;
    let (state, mode): (String, String) = world.database.connection().query_row(
        "SELECT state, mode FROM deployments WHERE item_id = ?1 AND agent_id = 'omp' \
         AND scope = 'project'",
        [world.mcps[gamma].item_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(state, "linked");
    assert_eq!(mode, "native", "MCP entries are native, never symlinks");
    Ok(())
}

#[test]
fn rule_save_updates_the_item_hash_and_create_undo_removes_the_row() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    save_rule_step(&world, 0)?;
    let path = world.rules[0].path.clone();
    let current_hash = at_fs::sha256_hex(&fs::read(&path)?);
    let stored: Option<String> = world
        .database
        .connection()
        .query_row(
            "SELECT content_hash FROM items WHERE kind = 'rule' AND source_path = ?1",
            [path.as_str()],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(stored.as_deref(), Some(current_hash.as_str()));

    let preview =
        flow_harness::instruction_create_preview(&world.state, "AGENTS.md", "user", None)?;
    let new_path = preview.path.expect("a user AGENTS.md path");
    create_rule(&mut world, Scope::User, "AGENTS.md")?;
    let operations = sequence_operations(&world.database, 0)?;
    let create_op = operations
        .iter()
        .find(|op| {
            operation_paths(&world.database, op)
                .is_ok_and(|paths| paths.contains(&PathBuf::from(&new_path)))
        })
        .cloned();
    if let Some(operation) = create_op {
        operation_undo_inner(&world.state, &operation, false)?;
    }
    let remaining: i64 = world.database.connection().query_row(
        "SELECT COUNT(*) FROM items WHERE kind = 'rule' AND source_path = ?1",
        [new_path.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(remaining, 0, "undo of a rule create removes its row");
    Ok(())
}

#[tokio::test]
async fn migration_normalizes_a_spaced_native_name() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    migrate_step(&mut world).await?;
    let layout = world.sandbox.layout(Scope::User)?;
    let package = layout.skill_dir(&Slug::parse("native-migrate")?);
    assert!(package.is_dir(), "the native name is normalized to a slug");
    let rows: i64 = world.database.connection().query_row(
        "SELECT COUNT(*) FROM items WHERE kind = 'skill' AND scope = 'user' AND slug = 'native-migrate'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(rows, 1);
    let operations = sequence_operations(&world.database, 0)?;
    undo_all_sync(&world, &operations)?;
    Ok(())
}

#[test]
fn skill_delete_undo_restores_exact_index_rows() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    skill_toggle(&world, 0, &AgentId::ClaudeCode, true)?;
    let before_items = item_rows(&world.database)?;
    let before_deployments = deployment_rows(&world.database)?;
    let baseline = max_operation_rowid(&world.database)?;

    delete_skill(&mut world, 0)?;
    let operations = sequence_operations(&world.database, baseline)?;
    undo_all_sync(&world, &operations)?;

    assert_row_set(
        &before_items,
        &item_rows(&world.database)?,
        "items",
        0,
        &world,
    );
    assert_row_set(
        &before_deployments,
        &deployment_rows(&world.database)?,
        "deployments",
        0,
        &world,
    );
    Ok(())
}

#[test]
fn mcp_delete_undo_restores_exact_index_rows() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    let gamma = world
        .mcps
        .iter()
        .position(|slot| slot.name == "Seq Gamma")
        .expect("project server");
    let before_items = item_rows(&world.database)?;
    let before_deployments = deployment_rows(&world.database)?;
    let baseline = max_operation_rowid(&world.database)?;

    delete_mcp(&mut world, gamma)?;
    let operations = sequence_operations(&world.database, baseline)?;
    undo_all_sync(&world, &operations)?;

    assert_row_set(
        &before_items,
        &item_rows(&world.database)?,
        "items",
        0,
        &world,
    );
    assert_row_set(
        &before_deployments,
        &deployment_rows(&world.database)?,
        "deployments",
        0,
        &world,
    );
    Ok(())
}

#[test]
fn a_shared_mcp_store_hash_is_consistent_and_undo_restores_it() -> TestResult<()> {
    let mut world = fixture(DriftMode::None)?;
    let before_items = item_rows(&world.database)?;
    let before_deployments = deployment_rows(&world.database)?;
    let baseline = max_operation_rowid(&world.database)?;

    add_step(&mut world, Scope::User, AgentId::ClaudeCode)?;

    let layout = world.sandbox.layout(Scope::User)?;
    let store_hash = at_fs::sha256_hex(&fs::read(layout.mcp_file())?);
    let rows = world.database.connection().prepare(
        "SELECT d.store_hash FROM deployments d JOIN items i ON i.id = d.item_id \
         WHERE i.kind = 'mcp_server' AND i.scope = 'user'",
    )?;
    let mut statement = rows;
    let hashes = statement
        .query_map([], |row| row.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    assert!(!hashes.is_empty());
    assert!(
        hashes
            .iter()
            .all(|hash| hash.as_deref() == Some(store_hash.as_str())),
        "every user MCP row describes the same shared store document: {hashes:?}"
    );

    let operations = sequence_operations(&world.database, baseline)?;
    undo_all_sync(&world, &operations)?;
    assert_row_set(
        &before_items,
        &item_rows(&world.database)?,
        "items",
        0,
        &world,
    );
    assert_row_set(
        &before_deployments,
        &deployment_rows(&world.database)?,
        "deployments",
        0,
        &world,
    );
    Ok(())
}
