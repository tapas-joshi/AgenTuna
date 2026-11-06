//! MCP server list plus the reviewed disable and delete row actions (TM-025).
//!
//! The list is the minimal projection the row menu needs; the full MCP list screen is TM-020.
//! Disable and delete both plan through [`at_engine::mcp_plan`], return a content-bound preview
//! first, and only apply the reviewed plan — so the dialog the user confirms is the plan that
//! runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{AgentId, ChangeSet, ItemId, ItemKind, ItemState, Scope};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::{
    commands::{CommandError, CommandErrorCode},
    paths::resolve_process_paths,
    state::AppState,
};

/// One agent cell in the minimal MCP list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerAgentView {
    /// Agent identity.
    pub agent: String,
    /// `linked`, `off`, `locked`, `shadowed`, `native`, `none` or `not_installed`.
    pub state: String,
    /// Native config path, when one exists.
    pub config_path: Option<String>,
    /// Whether the server loads for this agent (native-only entries always do).
    pub enabled: bool,
}

/// One MCP server row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerRowView {
    /// Canonical item id; empty for a native-only discovery.
    pub id: String,
    /// Canonical store slug, when the server is managed.
    pub slug: Option<String>,
    /// Display name.
    pub name: String,
    /// Scope of the definition or discovery.
    pub scope: String,
    /// `stdio`, `http` or `unknown`.
    pub transport: String,
    /// Whether the row exists only in an agent's own folder.
    pub native: bool,
    /// Whether a lock blocks changes.
    pub locked: bool,
    /// Scope that holds the lock.
    pub lock_scope: Option<String>,
    /// Lock reason shown as the tooltip and in the locked toast.
    pub lock_reason: Option<String>,
    /// Latest health status (`ok`, `failed`, `unknown`).
    pub health: String,
    /// Source path of the canonical definition or native discovery.
    pub source_path: Option<String>,
    /// Per-agent cells.
    pub agents: Vec<McpServerAgentView>,
}

/// The minimal MCP list projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerListView {
    /// Rows for the selected scope.
    pub servers: Vec<McpServerRowView>,
    /// Active project identity, when one is selected.
    pub project_id: Option<String>,
    /// Active project display name, used by the disable-in-repo dialog.
    pub project_name: Option<String>,
    /// Agents that have a project-level MCP config path, so the dialog can say who keeps loading.
    pub project_agents: Vec<String>,
    /// Whether the Global store accepts writes (lock actions are Global-only).
    pub global_writable: bool,
    /// Why Global writes are unavailable, when they are.
    pub global_reason: Option<String>,
}

/// Input to the minimal MCP list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerListRequest {
    /// Selected scope (`global`, `user`, `project`, `local`).
    pub scope: String,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
}

/// Input to a reviewed MCP row action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerActionRequest {
    /// Canonical item id from the list row.
    pub item_id: String,
    /// Scope of the definition.
    pub scope: String,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
    /// Agent for a per-agent disable; ignored for disable-in-repo and delete.
    pub agent: Option<String>,
    /// Record a project-scope tombstone instead of changing one agent.
    pub in_repo: bool,
    /// Apply the reviewed plan instead of returning a preview.
    pub apply: bool,
    /// Content-bound preview identity returned by the preview call.
    pub preview_id: Option<String>,
}

/// One file in a row-action preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpActionFileView {
    /// Target agent, or `AgenTuna` for the canonical store.
    pub agent: String,
    /// Scope whose file changes.
    pub scope: String,
    /// Absolute path that changes.
    pub path: String,
    /// Added lines in the exact diff.
    pub additions: u64,
    /// Removed lines in the exact diff.
    pub deletions: u64,
    /// Unified diff for review.
    pub diff: String,
}

/// A reviewed MCP row action, before it is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpActionPreviewView {
    /// Identity of the planned ChangeSet.
    pub change_set_id: String,
    /// Content-bound identity that must be passed back to apply.
    pub preview_id: String,
    /// Every file the action changes; the delete dialog counts them.
    pub files: Vec<McpActionFileView>,
    /// Requested agents skipped because they have no path at this scope.
    pub skipped_agents: Vec<String>,
    /// Non-fatal planner warnings.
    pub warnings: Vec<String>,
}

/// Result of applying a reviewed MCP row action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpActionApplyView {
    /// Operation journal identity.
    pub operation_id: String,
    /// Number of file operations applied.
    pub files_changed: usize,
}

#[derive(Clone)]
struct CachedAction {
    plan: at_engine::mcp_plan::McpItemPlan,
    preview_set: ChangeSet,
    preview_root: PathBuf,
    preview_id: String,
    apply_roots: Vec<PathBuf>,
    store_root: PathBuf,
    backup_folder: PathBuf,
    scope: Scope,
    in_repo: bool,
    item_id: ItemId,
    agents: Vec<AgentId>,
}

static ACTION_CACHE: LazyLock<Mutex<HashMap<String, CachedAction>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Lists MCP servers for the selected scope through the same index query the UI renders.
#[tauri::command]
pub fn mcp_server_list(
    state: State<'_, AppState>,
    request: McpServerListRequest,
) -> Result<McpServerListView, CommandError> {
    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_of(&database, request.project_id.as_deref())?;
    let paths = resolve_process_paths(project.as_ref().map(|project| Path::new(&project.path)))
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let rows = at_db::mcp_index::mcp_list_rows(
        database.connection(),
        Some(scope),
        request.project_id.as_deref(),
    )
    .map_err(|error| CommandError::internal(format!("cannot read MCP servers: {error}")))?;
    let transports = transport_lookup(&paths, scope, project.as_ref().map(|p| Path::new(&p.path)));
    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        let lock = row
            .item_id
            .map(|item_id| {
                at_db::locks::locks_for_item(database.connection(), &item_id).unwrap_or_default()
            })
            .and_then(|locks| locks.into_iter().next());
        let item = row
            .item_id
            .and_then(|id| transports.get(&id.to_string()).cloned());
        servers.push(McpServerRowView {
            id: row.item_id.map_or_else(String::new, |id| id.to_string()),
            slug: item.as_ref().map(|entry| entry.0.clone()),
            name: row.display_name.clone(),
            scope: row.scope.wire_name().to_owned(),
            transport: item.map_or_else(|| String::from("unknown"), |entry| entry.1),
            native: row.native_only,
            locked: row.locked_by.is_some(),
            lock_scope: lock.as_ref().map(|lock| lock.scope.wire_name().to_owned()),
            lock_reason: lock.as_ref().and_then(|lock| lock.reason.clone()),
            health: row
                .latest_health
                .clone()
                .unwrap_or_else(|| String::from("unknown")),
            source_path: row.source_path.clone(),
            agents: row
                .agents
                .iter()
                .map(|cell| McpServerAgentView {
                    agent: cell.agent_id.to_string(),
                    state: cell_state(&cell.state).to_owned(),
                    config_path: cell.config_path.clone(),
                    enabled: matches!(
                        cell.state,
                        at_db::mcp_index::McpCellState::Linked
                            | at_db::mcp_index::McpCellState::Native { .. }
                    ),
                })
                .collect(),
        });
    }
    let settings = at_db::settings::load(database.connection()).ok();
    let mut probe_paths = paths.clone();
    if let Some(settings) = settings.as_ref() {
        probe_paths.apply_settings_global_store(settings.global_store_path());
    }
    let global = at_fs::privilege::GlobalStoreStatus::probe(probe_paths.global_store());
    Ok(McpServerListView {
        servers,
        project_id: request.project_id,
        project_name: project.as_ref().map(|project| project.name.clone()),
        project_agents: project_agents(project.as_ref().map(|p| Path::new(&p.path))),
        global_writable: global.is_available(),
        global_reason: global.reason().map(ToOwned::to_owned),
    })
}

/// Plans or applies disabling an MCP server for one agent or one repository.
#[tauri::command]
pub fn mcp_server_disable(
    state: State<'_, AppState>,
    request: McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    run_action(&state, request, ActionKind::Disable)
}

/// Plans or applies deleting a managed MCP server and its native projections.
#[tauri::command]
pub fn mcp_server_delete(
    state: State<'_, AppState>,
    request: McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    run_action(&state, request, ActionKind::Delete)
}

/// Input to an explicit MCP connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerTestRequest {
    /// Canonical item id from the list row.
    pub item_id: String,
    /// Scope of the definition.
    pub scope: String,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
}

/// Redacted result of one explicit connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpServerTestView {
    /// Server name reported by the peer.
    pub server_name: String,
    /// Number of tools the server listed.
    pub tools_count: usize,
    /// Round-trip time of the tool listing.
    pub latency_ms: u128,
    /// Tool names, never their arguments or environment.
    pub tools: Vec<String>,
}

/// Tests one canonical MCP server after the user explicitly asked for it.
///
/// Testing is the consent step: nothing runs from a scan, and a failure is a typed, redacted
/// error rather than a silent empty list.
///
/// # Errors
/// Returns a typed failure when the definition is missing or the connection fails.
#[tauri::command]
pub fn mcp_server_test(
    state: State<'_, AppState>,
    request: McpServerTestRequest,
) -> Result<McpServerTestView, CommandError> {
    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))?;
    let item_id = ItemId::from_str(&request.item_id)
        .map_err(|_| CommandError::invalid_argument("item_id", "expected a canonical item id"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = project_of(&database, request.project_id.as_deref())?;
    let paths = resolve_process_paths(project.as_ref().map(|project| Path::new(&project.path)))
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let scoped = match project.as_ref() {
        Some(project) if matches!(scope, Scope::Project | Scope::Local) => {
            paths.for_project(Path::new(&project.path))
        }
        _ => Ok(paths.clone()),
    }
    .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let layout = at_store::layout::StoreLayout::for_scope(&scoped, scope)
        .map_err(|_| CommandError::invalid_argument("scope", "canonical store is unavailable"))?;
    let manifest = at_store::manifest::StoreManifest::load(&layout)
        .map_err(|_| CommandError::invalid_argument("item_id", "canonical item is unavailable"))?;
    let item = manifest
        .manifest
        .item(item_id)
        .ok_or_else(|| CommandError::invalid_argument("item_id", "server is not in this scope"))?;
    let loaded = at_store::mcp_store::McpStore::load(&layout).map_err(|_| {
        CommandError::invalid_argument("item_id", "server definition is unavailable")
    })?;
    let definition = loaded
        .store
        .get(item.slug.as_str())
        .ok_or_else(|| CommandError::invalid_argument("item_id", "server definition is missing"))?;
    let server = at_engine::mcp_cli::make_server(scope, definition.clone());
    let report = at_engine::mcp_cli::test_server(&server)
        .map_err(|error| CommandError::new(CommandErrorCode::Conflict, error.to_string()))?;
    Ok(McpServerTestView {
        server_name: report.server_name,
        tools_count: report.tools_count,
        latency_ms: report.latency_ms,
        tools: report.tools,
    })
}

/// Either the reviewed preview or the applied result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum McpServerActionOutcome {
    /// Nothing was written; the caller reviews this plan.
    Preview {
        /// Reviewed plan.
        preview: McpActionPreviewView,
    },
    /// The reviewed plan was applied.
    Applied {
        /// Applied result.
        applied: McpActionApplyView,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Disable,
    Delete,
}

fn run_action(
    state: &State<'_, AppState>,
    request: McpServerActionRequest,
    kind: ActionKind,
) -> Result<McpServerActionOutcome, CommandError> {
    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))?;
    let item_id = ItemId::from_str(&request.item_id).map_err(|_| {
        CommandError::invalid_argument(
            "item_id",
            "native-only servers must be adopted before they can be changed",
        )
    })?;
    let database = state.database().map_err(CommandError::unavailable)?;
    if request.apply {
        return apply_cached(state, &request);
    }
    let project = project_of(&database, request.project_id.as_deref())?;
    if matches!(scope, Scope::Project | Scope::Local) && project.is_none() {
        return Err(CommandError::invalid_argument(
            "project_id",
            "this scope requires an active project",
        ));
    }
    let paths = resolve_process_paths(project.as_ref().map(|project| Path::new(&project.path)))
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    if let Some(lock) = at_db::locks::locks_for_item(database.connection(), &item_id)
        .map_err(|error| CommandError::internal(format!("cannot read locks: {error}")))?
        .into_iter()
        .next()
    {
        return Err(CommandError::new(
            CommandErrorCode::Locked,
            lock.reason.unwrap_or_else(|| {
                format!("this server is locked at {} scope", lock.scope.wire_name())
            }),
        ));
    }
    let installed = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|error| CommandError::internal(format!("cannot read agents: {error}")))?
        .into_iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id)
        .collect::<Vec<_>>();
    let deployed = at_db::deployments::list_deployments_for_item(database.connection(), &item_id)
        .map_err(|error| CommandError::internal(format!("cannot read deployments: {error}")))?
        .into_iter()
        .filter(|cell| cell.scope == scope)
        .map(|cell| cell.agent_id)
        .collect::<Vec<_>>();
    let action = match kind {
        ActionKind::Delete => at_engine::mcp_plan::McpItemAction::Delete,
        ActionKind::Disable if request.in_repo => {
            let project = project.as_ref().ok_or_else(|| {
                CommandError::invalid_argument("project_id", "disable in this repo needs a project")
            })?;
            let agents = project_agents(Some(Path::new(&project.path)))
                .into_iter()
                .filter_map(|agent| AgentId::from_str(&agent).ok())
                .collect::<Vec<_>>();
            if agents.is_empty() {
                return Err(CommandError::invalid_argument(
                    "agent",
                    "no agent has a project-level MCP scope; disable it for one agent instead",
                ));
            }
            at_engine::mcp_plan::McpItemAction::DisableInRepo {
                project: PathBuf::from(&project.path),
                agents,
            }
        }
        ActionKind::Disable => {
            let agent = request.agent.as_deref().ok_or_else(|| {
                CommandError::invalid_argument("agent", "choose the agent to disable")
            })?;
            let agent = AgentId::from_str(agent)
                .map_err(|_| CommandError::invalid_argument("agent", "unknown agent identity"))?;
            at_engine::mcp_plan::McpItemAction::SetEnabled {
                agent,
                enabled: false,
            }
        }
    };
    let planned = at_engine::mcp_plan::plan_item(at_engine::mcp_plan::McpItemPlanRequest {
        paths: &paths,
        scope,
        project_id: request.project_id.clone(),
        item_id,
        action,
        installed: &installed,
        deployed: &deployed,
        locked_reason: None,
    })
    .map_err(|error| match error {
        at_engine::mcp_plan::McpPlanError::Locked(reason) => {
            CommandError::new(CommandErrorCode::Locked, reason)
        }
        other => CommandError::invalid_argument("server", &other.to_string()),
    })?;
    let mut combined = planned
        .plan
        .store
        .clone()
        .or_else(|| planned.plan.native.clone())
        .ok_or_else(|| CommandError::invalid_argument("server", "the plan changes no files"))?;
    if let Some(native) = &planned.plan.native {
        combined.ops.extend(native.ops.clone());
    }
    let (preview_set, preview_root) = at_engine::preview::relative_to_common_root(&combined)
        .map_err(|_| CommandError::invalid_argument("server", "the plan cannot be previewed"))?;
    let preview = at_engine::preview::preview(&preview_set, &preview_root)
        .map_err(|_| CommandError::invalid_argument("server", "the plan cannot be previewed"))?;
    let preview_id = preview.id.clone();
    let agents = planned.agents.clone();
    let store_ops = planned.plan.store.as_ref().map_or(0, |set| set.ops.len());
    let files = preview
        .files
        .iter()
        .enumerate()
        .zip(combined.ops.iter())
        .map(|((index, file), operation)| {
            let path = operation.destination().ok_or_else(|| {
                CommandError::internal(
                    "the MCP planner returned an operation without a destination",
                )
            })?;
            let agent = index
                .checked_sub(store_ops)
                .and_then(|native_index| agents.get(native_index))
                .map_or_else(|| String::from("AgenTuna"), ToString::to_string);
            let diff = match &file.diff {
                at_engine::preview::DiffKind::Unified { text, .. } => text.clone(),
                at_engine::preview::DiffKind::Binary { size } => {
                    format!("Binary file ({size} bytes)")
                }
                at_engine::preview::DiffKind::TooLarge { size } => {
                    format!("File too large to display ({size} bytes)")
                }
                at_engine::preview::DiffKind::Unchanged => String::new(),
                _ => String::from("Change preview unavailable"),
            };
            let (additions, deletions) = count_diff(&diff);
            Ok(McpActionFileView {
                agent,
                scope: scope.wire_name().to_owned(),
                path: path.display().to_string(),
                additions,
                deletions,
                diff,
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    let cache = CachedAction {
        plan: planned.clone(),
        preview_set,
        preview_root,
        preview_id: preview_id.clone(),
        apply_roots: authorized_roots(&paths, project.as_ref().map(|p| Path::new(&p.path))),
        store_root: paths.support_dir().to_path_buf(),
        backup_folder: paths.support_dir().join(at_store::layout::BACKUPS_DIR),
        scope,
        in_repo: request.in_repo && kind == ActionKind::Disable,
        item_id,
        agents,
    };
    ACTION_CACHE
        .lock()
        .map_err(|_| CommandError::internal("the MCP plan cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(McpServerActionOutcome::Preview {
        preview: McpActionPreviewView {
            change_set_id: combined.id.clone(),
            preview_id,
            files,
            skipped_agents: planned
                .skipped_agents
                .iter()
                .map(ToString::to_string)
                .collect(),
            warnings: planned.plan.warnings.clone(),
        },
    })
}

fn apply_cached(
    state: &State<'_, AppState>,
    request: &McpServerActionRequest,
) -> Result<McpServerActionOutcome, CommandError> {
    let preview_id = request.preview_id.as_deref().ok_or_else(|| {
        CommandError::invalid_argument("preview_id", "review the change before applying it")
    })?;
    let cached = ACTION_CACHE
        .lock()
        .map_err(|_| CommandError::internal("the MCP plan cache is unavailable"))?
        .remove(preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "reviewed plan expired; review again")
        })?;
    let current = at_engine::preview::preview(&cached.preview_set, &cached.preview_root)
        .map_err(|_| CommandError::invalid_argument("preview_id", "reviewed files changed"))?;
    if current.id != cached.preview_id {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the reviewed files changed; review the plan again",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::ApplyRoots::new(cached.apply_roots.clone())
        .map_err(|_| CommandError::unavailable("authorized configuration roots are unavailable"))?;
    let outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        at_engine::mcp_plan::apply_item_plan(&cached.plan, &mut context, &roots)
            .map_err(CommandError::from)?
    };
    for set in cached.plan.plan.apply_order() {
        state.record_writes(set, &cached.store_root);
    }
    if !cached.in_repo {
        let state_value = ItemState::Off;
        for agent in &cached.agents {
            let spec = at_db::deployments::DeploymentSpec::new(
                &cached.item_id,
                agent,
                cached.scope,
                state_value,
            )
            .with_mode("none")
            .with_operation(&outcome.operation_id);
            at_db::deployments::upsert_deployment(database.connection(), &spec).map_err(
                |error| CommandError::internal(format!("cannot record the deployment: {error}")),
            )?;
        }
    }
    Ok(McpServerActionOutcome::Applied {
        applied: McpActionApplyView {
            operation_id: outcome.operation_id,
            files_changed: outcome.completed,
        },
    })
}

fn project_of(
    database: &at_db::Database,
    project_id: Option<&str>,
) -> Result<Option<at_db::projects::ProjectSummary>, CommandError> {
    match project_id {
        None => Ok(None),
        Some(id) => at_db::projects::find_project_by_id(database.connection(), id)
            .map_err(|_| CommandError::internal("could not read the selected project"))?
            .map(Some)
            .ok_or_else(|| {
                CommandError::invalid_argument("project_id", "project is not registered")
            }),
    }
}

/// Canonical slug and transport per indexed item id.
fn transport_lookup(
    paths: &at_store::paths::AppPaths,
    scope: Scope,
    project_root: Option<&Path>,
) -> HashMap<String, (String, String)> {
    let mut result = HashMap::new();
    let scoped = match project_root {
        Some(root) if matches!(scope, Scope::Project | Scope::Local) => {
            match paths.for_project(root) {
                Ok(paths) => paths,
                Err(_) => return result,
            }
        }
        _ => paths.clone(),
    };
    let Ok(layout) = at_store::layout::StoreLayout::for_scope(&scoped, scope) else {
        return result;
    };
    let Ok(manifest) = at_store::manifest::StoreManifest::load(&layout) else {
        return result;
    };
    let Ok(loaded) = at_store::mcp_store::McpStore::load(&layout) else {
        return result;
    };
    for item in manifest
        .manifest
        .items_of_kind(ItemKind::McpServer)
        .collect::<Vec<_>>()
    {
        let transport = loaded.store.get(item.slug.as_str()).map_or_else(
            || String::from("unknown"),
            |definition| definition.transport.kind().to_owned(),
        );
        result.insert(
            item.id.to_string(),
            (item.slug.as_str().to_owned(), transport),
        );
    }
    result
}

/// Agents that declare a project-level MCP config path.
fn project_agents(project_root: Option<&Path>) -> Vec<String> {
    let registry = AgentAdapterRegistry::register();
    let environment = AgentEnvironment {
        home: PathBuf::from("/"),
        project: project_root.map(Path::to_path_buf),
        variables: std::collections::BTreeMap::new(),
    };
    let Ok(adapters) = registry.adapters() else {
        return Vec::new();
    };
    let mut agents = adapters
        .into_iter()
        .filter(|adapter| {
            adapter
                .paths(ItemKind::McpServer, Scope::Project, &environment)
                .into_iter()
                .any(|candidate| candidate.primary)
        })
        .map(|adapter| adapter.id().to_string())
        .collect::<Vec<_>>();
    agents.sort();
    agents
}

fn cell_state(state: &at_db::mcp_index::McpCellState) -> &'static str {
    match state {
        at_db::mcp_index::McpCellState::Linked => "linked",
        at_db::mcp_index::McpCellState::Off => "off",
        at_db::mcp_index::McpCellState::Locked => "locked",
        at_db::mcp_index::McpCellState::Shadowed => "shadowed",
        at_db::mcp_index::McpCellState::Native { .. } => "native",
        at_db::mcp_index::McpCellState::None => "none",
        at_db::mcp_index::McpCellState::NotInstalled => "not_installed",
        _ => "unknown",
    }
}

fn authorized_roots(paths: &at_store::paths::AppPaths, project: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![paths.support_dir().to_path_buf()];
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    roots.push(paths.global_store().to_path_buf());
    roots.push(paths.user_store().to_path_buf());
    if let Some(project) = project {
        roots.push(project.to_path_buf());
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

fn count_diff(diff: &str) -> (u64, u64) {
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            additions = additions.saturating_add(1);
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions = deletions.saturating_add(1);
        }
    }
    (additions, deletions)
}
