//! Stable EffectiveConfig and why-trace snapshots over migration-backed SQLite fixtures.
//!
//! Each scenario seeds a temporary on-disk database through the shipped migrations, builds the
//! effective projection from its deployment rows, and compares both serialized outputs with
//! committed snapshots. No process HOME, Keychain, network or real agent configuration is used.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use at_adapters::adapter::NativeItem;
use at_db::Database;
use at_engine::effective::{self, EffectiveConfig, EffectiveQuery};
use at_engine::loader::{self, AgentObservation};
use at_engine::locks::{LockId, LockRecord, LockStore, MemoryLockStore};
use at_engine::tombstones::MemoryTombstones;
use at_engine::why::{self, DocumentLocation, StepStatus, TraceEvidence, WhyTrace};
use at_model::{AgentId, Definition, ItemId, ItemKind, ItemState, Lock, Scope, ScopeSnapshot};
use at_store::manifest::{DisabledAgents, DisabledEntry};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use tempfile::TempDir;

const SCENARIOS: [&str; 7] = [
    "inherited",
    "shadowed",
    "disabled",
    "locked",
    "native",
    "missing-agent",
    "unreadable",
];
const PROJECT_ID: &str = "fixture-project";

#[derive(Clone)]
struct DeploymentFixture {
    item_id: ItemId,
    scope: Scope,
    state: ItemState,
}

struct Scenario {
    layers: Vec<ScopeSnapshot>,
    deployments: Vec<DeploymentFixture>,
    installed: bool,
    native: Vec<NativeItem>,
    tombstone: Option<(Scope, DisabledEntry)>,
    lock: Option<LockRecord>,
    trace_item_id: ItemId,
    trace_evidence: [(Scope, TraceEvidence); 4],
}

#[derive(Serialize)]
struct Snapshot {
    effective: EffectiveConfig,
    trace: WhyTrace,
}

#[test]
fn seeded_database_scenarios_match_committed_effective_and_trace_snapshots()
-> Result<(), Box<dyn Error>> {
    let update = std::env::var("UPDATE_EFFECTIVE_SNAPSHOTS").is_ok_and(|value| value == "1");
    for name in SCENARIOS {
        let line = if name == "inherited" {
            Some(server_line("trace-server.json")?)
        } else {
            None
        };
        let (actual, trace, unreadable_error) = run_scenario(name, line)?;
        if name == "locked" {
            assert!(
                trace
                    .steps
                    .iter()
                    .any(|step| matches!(step.status, StepStatus::Locked { .. }))
            );
            assert!(
                trace
                    .steps
                    .iter()
                    .any(|step| matches!(step.status, StepStatus::Shadowed))
            );
            assert!(
                !trace
                    .steps
                    .iter()
                    .any(|step| matches!(step.status, StepStatus::Winning))
            );
        }
        if name == "unreadable" {
            let expected_error = unreadable_error.as_deref().ok_or_else(|| {
                io::Error::other("unreadable scenario did not produce an OS error")
            })?;
            let os_error = trace.steps.iter().find_map(|step| match &step.status {
                StepStatus::Unreadable { os_error } => Some(os_error.as_str()),
                _ => None,
            });
            assert_eq!(os_error, Some(expected_error));
            assert!(!expected_error.trim().is_empty());
        }
        let actual = normalize_snapshot(actual, unreadable_error.as_deref());
        let snapshot_path = snapshot_path(name);
        if update {
            fs::write(
                &snapshot_path,
                format!("{}\n", serde_json::to_string_pretty(&actual)?),
            )?;
            continue;
        }
        let expected: Value = serde_json::from_slice(&fs::read(&snapshot_path)?)?;
        assert_eq!(actual, expected, "snapshot changed for {name}");
    }
    Ok(())
}

#[test]
fn moving_the_server_three_lines_changes_only_trace_line_fields() -> Result<(), Box<dyn Error>> {
    let base_line = server_line("trace-server.json")?;
    let shifted_line = server_line("trace-server-shifted.json")?;
    assert_eq!(shifted_line, base_line + 3);

    let (base, _, _) = run_scenario("inherited", Some(base_line))?;
    let (shifted, _, _) = run_scenario("inherited", Some(shifted_line))?;
    let base_line_field = base["trace"]["steps"][0]["location"]["line"]
        .as_u64()
        .ok_or_else(|| io::Error::other("base trace line is missing"))?;
    let shifted_line_field = shifted["trace"]["steps"][0]["location"]["line"]
        .as_u64()
        .ok_or_else(|| io::Error::other("shifted trace line is missing"))?;
    assert_eq!(shifted_line_field, base_line_field + 3);

    let mut base_without_lines = base;
    let mut shifted_without_lines = shifted;
    remove_line_fields(&mut base_without_lines);
    remove_line_fields(&mut shifted_without_lines);
    assert_eq!(base_without_lines, shifted_without_lines);
    Ok(())
}

fn run_scenario(
    name: &str,
    source_line: Option<usize>,
) -> Result<(Value, WhyTrace, Option<String>), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let unreadable_error = if name == "unreadable" {
        Some(read_directory_as_file(&root)?)
    } else {
        None
    };
    let scenario = scenario(name, source_line, unreadable_error.as_deref())?;
    let database_path = root.join("scenario.sqlite3");
    let database = Database::open_file(&database_path)?;
    let tables: i64 = database.connection().query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'items'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(tables, 1, "shipped migrations must create the item index");
    seed_database(database.connection(), &scenario)?;

    let agent = AgentId::ClaudeCode;
    let observations = [AgentObservation {
        agent: agent.clone(),
        installed: scenario.installed,
        native: scenario.native,
        diagnostics: Vec::new(),
    }];
    let agent_loads = loader::agent_loads(
        database.connection(),
        &observations,
        &scenario.layers,
        Scope::Project,
        Some(PROJECT_ID),
    )?;
    let lock_store = MemoryLockStore::new();
    if let Some(record) = &scenario.lock {
        lock_store.put(record.clone())?;
    }
    let tombstones = MemoryTombstones::new();
    if let Some((scope, entry)) = &scenario.tombstone {
        tombstones.add(*scope, entry.clone())?;
    }
    let agent_filter = scenario.installed.then_some(&agent);
    let effective = effective::query(EffectiveQuery {
        scope: Scope::Project,
        layers: &scenario.layers,
        agent_filter,
        kind_filter: None,
        agents: &agent_loads,
        locks: Some(&lock_store),
        tombstones: Some(&tombstones),
    });
    let trace = why::trace(
        scenario.trace_item_id,
        Scope::Project,
        agent,
        scenario.trace_evidence,
    );
    let output = serde_json::to_value(Snapshot {
        effective,
        trace: trace.clone(),
    })?;
    Ok((output, trace, unreadable_error))
}

fn scenario(
    name: &str,
    source_line: Option<usize>,
    unreadable_error: Option<&str>,
) -> Result<Scenario, Box<dyn Error>> {
    let mut definitions = Vec::<(Scope, Definition)>::new();
    let mut deployments = Vec::new();
    let mut installed = true;
    let mut native = Vec::new();
    let mut tombstone = None;
    let mut lock = None;
    let trace_item_id;

    match name {
        "inherited" => {
            let id = item_id(1)?;
            definitions.push((
                Scope::Global,
                mcp_definition(id, "Search", "search-server", "<global>/mcp.json"),
            ));
            deployments.push(DeploymentFixture {
                item_id: id,
                scope: Scope::Global,
                state: ItemState::Linked,
            });
            trace_item_id = id;
        }
        "shadowed" => {
            let values = [
                (Scope::Global, 2),
                (Scope::User, 3),
                (Scope::Project, 4),
                (Scope::Local, 5),
            ];
            for (scope, number) in values {
                let id = item_id(number)?;
                definitions.push((
                    scope,
                    mcp_definition(
                        id,
                        "Search",
                        "search-server",
                        &format!("<{}/mcp.json", scope_label(scope)),
                    ),
                ));
                deployments.push(DeploymentFixture {
                    item_id: id,
                    scope,
                    state: ItemState::Linked,
                });
            }
            trace_item_id = item_id(5)?;
        }
        "disabled" => {
            let id = item_id(6)?;
            definitions.push((
                Scope::User,
                skill_definition(id, "formatter", "<user>/skills/formatter/SKILL.md"),
            ));
            deployments.push(DeploymentFixture {
                item_id: id,
                scope: Scope::User,
                state: ItemState::Linked,
            });
            tombstone = Some((
                Scope::Project,
                DisabledEntry {
                    item_id: id,
                    kind: ItemKind::Skill,
                    agents: DisabledAgents::All,
                },
            ));
            trace_item_id = id;
        }
        "locked" => {
            let global_id = item_id(7)?;
            let project_id = item_id(8)?;
            definitions.push((
                Scope::Global,
                mcp_definition(global_id, "Search", "search-server", "<global>/mcp.json"),
            ));
            definitions.push((
                Scope::Project,
                mcp_definition(project_id, "Search", "search-server", "<project>/mcp.json"),
            ));
            deployments.push(DeploymentFixture {
                item_id: global_id,
                scope: Scope::Global,
                state: ItemState::Linked,
            });
            deployments.push(DeploymentFixture {
                item_id: project_id,
                scope: Scope::Project,
                state: ItemState::Linked,
            });
            let lock_id = LockId::from_str("lck_018f47aa-7a3d-7000-8000-000000000001")?;
            lock = Some(LockRecord {
                id: lock_id,
                lock: Lock {
                    scope: Scope::Global,
                    item_id: global_id,
                    reason: "Organization policy pins this server".to_owned(),
                    locked_by: "fixture".to_owned(),
                    locked_at: time::OffsetDateTime::from_unix_timestamp(1_700_000_000)?,
                },
            });
            trace_item_id = global_id;
        }
        "native" => {
            let id = item_id(9)?;
            definitions.push((
                Scope::User,
                skill_definition(id, "formatter", "<user>/skills/formatter/SKILL.md"),
            ));
            native.push(NativeItem {
                kind: ItemKind::Skill,
                scope: Scope::User,
                name: "formatter".to_owned(),
                path: PathBuf::from("<agent>/skills/formatter/SKILL.md"),
            });
            trace_item_id = id;
        }
        "missing-agent" => {
            let id = item_id(10)?;
            definitions.push((
                Scope::Global,
                mcp_definition(id, "Search", "search-server", "<global>/mcp.json"),
            ));
            installed = false;
            trace_item_id = id;
        }
        "unreadable" => {
            if unreadable_error.is_none_or(str::is_empty) {
                return Err(io::Error::other("unreadable fixture is missing its OS error").into());
            }
            trace_item_id = item_id(11)?;
        }
        _ => return Err(io::Error::other(format!("unknown effective scenario `{name}`")).into()),
    }

    let layers = [Scope::Global, Scope::User, Scope::Project, Scope::Local]
        .into_iter()
        .map(|scope| ScopeSnapshot {
            scope,
            definitions: definitions
                .iter()
                .filter(|(definition_scope, _)| *definition_scope == scope)
                .map(|(_, definition)| definition.clone())
                .collect(),
        })
        .collect();
    let trace_evidence = [Scope::Global, Scope::User, Scope::Project, Scope::Local].map(|scope| {
        let (defined, disabled, winning, shadowed, lock_reason, unreadable, line) =
            match (name, scope) {
                ("inherited", Scope::Global) => (true, false, true, false, None, None, source_line),
                ("shadowed", Scope::Global | Scope::User | Scope::Project) => (
                    true,
                    false,
                    false,
                    true,
                    None,
                    None,
                    Some(usize::from(scope.precedence()) + 1),
                ),
                ("shadowed", Scope::Local) => (true, false, true, false, None, None, Some(4)),
                ("disabled", Scope::User) => (true, false, false, false, None, None, Some(2)),
                ("disabled", Scope::Project) => (false, true, false, false, None, None, Some(4)),
                ("locked", Scope::Global) => (
                    true,
                    false,
                    false,
                    false,
                    Some("Organization policy pins this server".to_owned()),
                    None,
                    Some(2),
                ),
                ("locked", Scope::Project) => (true, false, false, true, None, None, Some(4)),
                ("native", Scope::User) => (true, false, true, false, None, None, Some(3)),
                ("missing-agent", Scope::Global) => (true, false, true, false, None, None, Some(2)),
                ("unreadable", Scope::User) => (
                    false,
                    false,
                    false,
                    false,
                    None,
                    unreadable_error.map(str::to_owned),
                    Some(3),
                ),
                _ => (false, false, false, false, None, None, None),
            };
        let (location_path, rendering_path) = match (name, scope) {
            ("disabled", Scope::User) => (
                "<user>/skills/formatter/SKILL.md".to_owned(),
                "<agent>/skills/formatter/SKILL.md".to_owned(),
            ),
            ("disabled", Scope::Project) => (
                "<project>/store.json".to_owned(),
                "<agent>/settings.json".to_owned(),
            ),
            ("native", Scope::User) => (
                "<user>/skills/formatter/SKILL.md".to_owned(),
                "<agent>/skills/formatter/SKILL.md".to_owned(),
            ),
            _ => (
                format!("<{}>/mcp.json", scope_label(scope)),
                "<agent>/settings.json".to_owned(),
            ),
        };
        let location =
            (defined || disabled || lock_reason.is_some() || unreadable.is_some()).then(|| {
                DocumentLocation {
                    path: PathBuf::from(location_path),
                    byte_span: Some(0..18),
                    line,
                    rendering_path: Some(PathBuf::from(rendering_path)),
                }
            });
        (
            scope,
            TraceEvidence {
                defined,
                disabled,
                winning,
                shadowed,
                lock_reason,
                unreadable,
                location,
            },
        )
    });

    Ok(Scenario {
        layers,
        deployments,
        installed,
        native,
        tombstone,
        lock,
        trace_item_id,
        trace_evidence,
    })
}

fn seed_database(connection: &Connection, scenario: &Scenario) -> Result<(), Box<dyn Error>> {
    for layer in &scenario.layers {
        for definition in &layer.definitions {
            let id = definition
                .item_id
                .ok_or_else(|| io::Error::other("database fixture definition has no item id"))?;
            let project_id =
                matches!(layer.scope, Scope::Project | Scope::Local).then_some(PROJECT_ID);
            connection.execute(
                "INSERT INTO items (id, kind, scope, project_id, slug, name, source_path, state, managed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'present', 1)",
                params![
                    id.to_string(),
                    wire_string(&definition.kind)?,
                    scope_label(layer.scope),
                    project_id,
                    definition.name.to_lowercase().replace(' ', "-"),
                    definition.name,
                    definition.path.to_string_lossy().as_ref(),
                ],
            )?;
        }
    }
    for deployment in &scenario.deployments {
        let project_id =
            matches!(deployment.scope, Scope::Project | Scope::Local).then_some(PROJECT_ID);
        connection.execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state, mode, target_path) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'symlink', '<agent>/configuration.json')",
            params![
                deployment.item_id.to_string(),
                AgentId::ClaudeCode.to_string(),
                scope_label(deployment.scope),
                project_id,
                wire_string(&deployment.state)?,
            ],
        )?;
    }
    Ok(())
}

fn mcp_definition(item_id: ItemId, name: &str, declared_id: &str, path: &str) -> Definition {
    Definition {
        kind: ItemKind::McpServer,
        name: name.to_owned(),
        declared_id: Some(declared_id.to_owned()),
        item_id: Some(item_id),
        path: PathBuf::from(path),
    }
}

fn skill_definition(item_id: ItemId, name: &str, path: &str) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id: Some(item_id),
        path: PathBuf::from(path),
    }
}

fn item_id(number: u32) -> Result<ItemId, Box<dyn Error>> {
    Ok(format!("itm_018f47aa-7a3d-7000-8000-{number:012}").parse()?)
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::User => "user",
        Scope::Project => "project",
        Scope::Local => "local",
    }
}

fn wire_string<T: Serialize>(value: &T) -> Result<String, Box<dyn Error>> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("expected a string wire value").into())
}

fn read_directory_as_file(root: &Path) -> Result<String, Box<dyn Error>> {
    let file = root.join("unreadable-config.json");
    fs::write(&file, b"{\"mcpServers\":{}}")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&file)?.permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&file, permissions)?;
        if let Err(error) = fs::read_to_string(&file) {
            return Ok(error.to_string());
        }
        fs::remove_file(&file)?;
    }
    #[cfg(not(unix))]
    fs::remove_file(&file)?;

    let directory = root.join("unreadable-config.json");
    fs::create_dir(&directory)?;
    let error = fs::read_to_string(directory)
        .err()
        .ok_or_else(|| io::Error::other("reading a directory unexpectedly succeeded"))?;
    Ok(error.to_string())
}

fn normalize_snapshot(mut snapshot: Value, unreadable_error: Option<&str>) -> Value {
    if unreadable_error.is_some()
        && let Some(steps) = snapshot["trace"]["steps"].as_array_mut()
    {
        for step in steps {
            if step["status"].get("Unreadable").is_some() {
                step["status"]["Unreadable"]["os_error"] = Value::String("<OS error>".to_owned());
                step["reason"] = Value::String("Owning file is unreadable: <OS error>".to_owned());
            }
        }
    }
    snapshot
}

fn remove_line_fields(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(remove_line_fields),
        Value::Object(fields) => {
            fields.remove("line");
            fields.values_mut().for_each(remove_line_fields);
        }
        _ => {}
    }
}

fn server_line(file: &str) -> Result<usize, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/effective-config")
        .join(file);
    let source = fs::read_to_string(path)?;
    source
        .lines()
        .position(|line| line.contains("\"search\""))
        .map(|index| index + 1)
        .ok_or_else(|| io::Error::other("trace fixture does not contain the search server").into())
}

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/effective-config/snapshots")
        .join(format!("{name}.json"))
}
