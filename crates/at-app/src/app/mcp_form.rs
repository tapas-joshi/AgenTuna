//! Reviewed MCP server creation commands.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{
    AgentId, ChangeSet, EnvValue, ExpectedState, FileOp, ItemId, ItemKind, McpServerDef,
    McpTransport, Scope, SecretRef,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::{
    commands::{
        CommandError, CommandErrorCode, McpChangeSetPreviewView, McpFileChangeView,
        McpTestResultView, McpUnsupportedFieldView,
    },
    mcp_launch::{McpLaunchApproval, McpLaunchApprovalView},
    mcp_servers::scoped_project_id,
    paths::resolve_command_paths,
    state::AppState,
};

/// One literal or Keychain-referenced MCP form value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpFormValue {
    /// Environment variable or HTTP header name.
    pub name: String,
    /// Literal value, or `${NAME}` reference to a secret already in Keychain.
    pub value: String,
}

/// Input to a read-only MCP plan preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpPlanPreviewRequest {
    /// Display name used in the native config.
    pub name: String,
    /// Transport (`stdio` or `http`).
    pub transport: String,
    /// Command for stdio transport.
    pub command: String,
    /// Command arguments for stdio transport.
    pub args: Vec<String>,
    /// Endpoint URL for HTTP transport.
    pub url: String,
    /// HTTP headers.
    pub headers: Vec<McpFormValue>,
    /// Stdio environment variables.
    pub env: Vec<McpFormValue>,
    /// Target scope (`global`, `user`, `project`, or `local`).
    pub scope: String,
    /// Selected native agents.
    pub agents: Vec<String>,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
    /// Existing canonical item id; omitted for add mode.
    #[serde(default)]
    pub item_id: Option<String>,
}

/// Identity used to apply a reviewed MCP plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpSaveRequest {
    /// Content-bound identity returned by `mcp_plan_preview`.
    pub preview_id: String,
    /// Selected target agents in the reviewed plan.
    pub selected_agents: Vec<String>,
    /// Explicit confirmation of the fields the reviewed preview drops.
    #[serde(default)]
    pub acknowledge_unsupported: bool,
}

/// Result of applying the reviewed MCP plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpSaveResultView {
    /// Operation journal identity.
    pub operation_id: String,
    /// Number of native config files changed.
    pub files_changed: usize,
}

#[derive(Clone)]
struct CachedPlan {
    store: Option<ChangeSet>,
    native: Option<ChangeSet>,
    preview_set: ChangeSet,
    preview_root: PathBuf,
    apply_roots: Vec<PathBuf>,
    store_root: PathBuf,
    store_file: PathBuf,
    scope: Scope,
    backup_folder: PathBuf,
    agents_by_op: Vec<String>,
    blocked_agents: Vec<String>,
    has_unsupported_drops: bool,
    preview_id: String,
    /// Root of the canonical store the save writes, used to re-read its manifest.
    store_layout_root: PathBuf,
    /// Slug of the saved definition, used to find its manifest record after apply.
    slug: String,
    /// Registered project for Project and Local scopes.
    project_id: Option<String>,
}

fn plan_cache() -> &'static Mutex<HashMap<String, CachedPlan>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedPlan>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Creates a read-only, content-bound preview for an MCP server definition.
#[tauri::command]
pub fn mcp_plan_preview(
    state: State<'_, AppState>,
    request: McpPlanPreviewRequest,
) -> Result<McpChangeSetPreviewView, CommandError> {
    mcp_plan_preview_impl(&state, request)
}

/// Plans an add, edit or rename and returns the exact files the save will write.
///
/// The body is separated from the Tauri command so app-level tests exercise the same planner,
/// permission probe and preview identity the webview sees.
///
/// # Errors
/// Returns a typed command error for invalid arguments, unavailable paths or a refused plan.
pub fn mcp_plan_preview_impl(
    state: &AppState,
    request: McpPlanPreviewRequest,
) -> Result<McpChangeSetPreviewView, CommandError> {
    use std::str::FromStr;

    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))?;
    if request.agents.is_empty() {
        return Err(CommandError::invalid_argument(
            "agents",
            "select at least one installed agent",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    // Only Project and Local items belong to a repository; a User save issued while a project
    // is active must resolve the user store and index a project-less row.
    let project_root = match scoped_project_id(scope, request.project_id.as_deref()) {
        Some(id) => Some(
            at_db::projects::find_project_by_id(database.connection(), id)
                .map_err(|_| CommandError::internal("could not read the selected project"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?
                .path
                .into(),
        ),
        None => None,
    };
    if matches!(scope, Scope::Project | Scope::Local) && project_root.is_none() {
        return Err(CommandError::invalid_argument(
            "project_id",
            "this scope requires an active project",
        ));
    }
    if scope == Scope::Global {
        return Err(CommandError::invalid_argument(
            "scope",
            "global writes are not available in this form",
        ));
    }
    let paths = resolve_command_paths(state, project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: project_root.clone(),
        variables: BTreeMap::new(),
    };
    let transport = make_transport(&request, database.connection())?;
    let definition = McpServerDef {
        display_name: request.name.trim().to_owned(),
        description: None,
        transport,
        extra: BTreeMap::new(),
    };
    if definition.display_name.is_empty() {
        return Err(CommandError::invalid_argument(
            "name",
            "enter a server name",
        ));
    }
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not read detected agents"))?;
    let installed = agents
        .iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id.clone())
        .collect::<Vec<_>>();
    let registry = AgentAdapterRegistry::register();
    let slug = slugify(&definition.display_name);
    if slug.is_empty() {
        return Err(CommandError::invalid_argument(
            "name",
            "name must include an ASCII letter or number",
        ));
    }
    let plan_slug = slug.clone();
    let mut targets = Vec::with_capacity(request.agents.len());
    let mut unsupported_fields = Vec::new();
    let mut blocked_agents = Vec::new();
    for raw_agent in &request.agents {
        let agent = AgentId::from_str(raw_agent)
            .map_err(|_| CommandError::invalid_argument("agents", "contains an unknown agent"))?;
        let adapter = registry
            .get(&agent)
            .map_err(|_| CommandError::internal("agent registry is unavailable"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("agents", "agent adapter is unavailable")
            })?;
        let candidate = adapter
            .paths(ItemKind::McpServer, scope, &env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .ok_or_else(|| {
                CommandError::invalid_argument("scope", "agent has no path for this scope")
            })?;
        let rendered =
            at_adapters::mcp_capabilities::project_transport(&agent, &definition.transport);
        if !rendered.unsupported.is_empty() {
            unsupported_fields.push(McpUnsupportedFieldView {
                agent: agent.to_string(),
                fields: rendered
                    .unsupported
                    .iter()
                    .map(|field| field.name().to_owned())
                    .collect(),
            });
        }
        let exists = match std::fs::symlink_metadata(&candidate.path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                return Err(CommandError::invalid_argument(
                    "target",
                    "native MCP config cannot be inspected safely",
                ));
            }
        };
        let bytes = if exists {
            let read = if at_adapters::adapter::follows_native_config_symlink(&agent) {
                at_fs::symlink::read_following(&candidate.path)
            } else {
                at_fs::symlink::read_nofollow(&candidate.path)
            };
            read.map_err(|_| {
                CommandError::invalid_argument("target", "native MCP config cannot be read safely")
            })?
        } else {
            Vec::new()
        };
        let source = String::from_utf8(bytes.clone()).map_err(|_| {
            CommandError::invalid_argument("target", "native MCP config is not UTF-8")
        })?;
        let expected = if exists {
            ExpectedState::hash(at_fs::sha256_hex(&bytes))
                .map_err(|_| CommandError::internal("native config hash is invalid"))?
        } else {
            ExpectedState::absent()
        };
        if write_refusal(&candidate.path).is_some() {
            blocked_agents.push(agent.to_string());
        }
        targets.push(at_engine::mcp_plan::McpTarget {
            agent: agent.clone(),
            scope,
            item_id: ItemId::new(),
            slug: slug.clone(),
            path: candidate.path,
            source,
            expected,
            unsupported_acknowledged: true,
            installed: installed.contains(&agent),
        });
    }
    let intent = if let Some(raw_id) = request.item_id.as_deref() {
        let item_id = raw_id.parse::<ItemId>().map_err(|_| {
            CommandError::invalid_argument("item_id", "expected a canonical item id")
        })?;
        let store_layout =
            at_store::layout::StoreLayout::for_scope(&paths, scope).map_err(|_| {
                CommandError::invalid_argument("scope", "canonical store is unavailable")
            })?;
        let manifest = at_store::manifest::StoreManifest::load(&store_layout).map_err(|_| {
            CommandError::invalid_argument("item_id", "canonical item could not be loaded")
        })?;
        let item = manifest.manifest.item(item_id).ok_or_else(|| {
            CommandError::invalid_argument("item_id", "server does not exist in this scope")
        })?;
        if item.kind != ItemKind::McpServer {
            return Err(CommandError::invalid_argument(
                "item_id",
                "item is not an MCP server",
            ));
        }
        let loaded = at_store::mcp_store::McpStore::load(&store_layout).map_err(|_| {
            CommandError::invalid_argument("item_id", "canonical server could not be loaded")
        })?;
        let old_definition = loaded.store.get(item.slug.as_str()).ok_or_else(|| {
            CommandError::invalid_argument("item_id", "canonical server definition is missing")
        })?;
        let old_slug = item.slug.as_str().to_owned();
        let mut edited = definition;
        edited.description.clone_from(&old_definition.description);
        edited.extra.clone_from(&old_definition.extra);
        let intent = if old_slug == slug {
            at_engine::mcp_plan::McpIntent::Edit {
                slug: old_slug,
                definition: edited.clone(),
            }
        } else {
            at_engine::mcp_plan::McpIntent::Rename {
                from: old_slug,
                to: slug,
            }
        };
        (intent, edited)
    } else {
        (
            at_engine::mcp_plan::McpIntent::Add {
                slug,
                definition: definition.clone(),
            },
            definition,
        )
    };
    let (intent, plan_definition) = intent;
    let project_id = scoped_project_id(scope, request.project_id.as_deref()).map(str::to_owned);
    let cache_project_id = project_id.clone();
    let store_layout = at_store::layout::StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::invalid_argument("scope", "canonical store is unavailable"))?;
    let mut planned = at_engine::mcp_plan::plan(at_engine::mcp_plan::McpPlanRequest {
        registry: &registry,
        env: &env,
        intent: &intent,
        targets: &targets,
        definition: &plan_definition,
        scope,
        project_id,
        locked_reason: None,
        store_layout: Some(&store_layout),
        target_store_layout: None,
    })
    .map_err(|error| CommandError::invalid_argument("definition", &error.to_string()))?;
    if let at_engine::mcp_plan::McpPlanStatus::NotSupported { reason, .. } = &planned.status {
        return Err(CommandError::invalid_argument("agents", reason));
    }
    // A scope with no store yet gets its skeleton in this same reviewed ChangeSet, so one
    // preview shows `Create store at <path>` and one apply (and undo) covers both the store and
    // the server. An already-ready store is left byte-identical.
    let store_init = match planned.store.as_mut() {
        Some(store) => at_store::store_init::plan_store_init(&store_layout, store)
            .map_err(|error| super::store_setup::store_init_error(&store_layout, error))?,
        None => None,
    };
    let mut change_set = planned
        .store
        .clone()
        .or_else(|| planned.native.clone())
        .ok_or_else(|| {
            CommandError::invalid_argument("agents", "plan contains no supported changes")
        })?;
    if let Some(native) = &planned.native {
        change_set.ops.extend(native.ops.clone());
    }
    let preview_root = common_ancestor(&change_set)?;
    let preview_set = relative_preview_set(&change_set, &preview_root)?;
    let preview = at_engine::preview::preview(&preview_set, &preview_root).map_err(|error| {
        CommandError::invalid_argument(
            "target",
            &format!("native config changed or could not be previewed: {error}"),
        )
    })?;
    let preview_id = preview.id;
    if let Some(store) = &mut planned.store {
        store.preview_id.clone_from(&preview_id);
    }
    if let Some(native) = &mut planned.native {
        native.preview_id.clone_from(&preview_id);
    }
    let operations = planned
        .store
        .iter()
        .flat_map(|set| &set.ops)
        .chain(planned.native.iter().flat_map(|set| &set.ops));
    let files = preview
        .files
        .iter()
        .zip(operations)
        .map(|(file, operation)| {
            let path = operation.destination().ok_or_else(|| {
                CommandError::internal("MCP planner returned an unsupported file operation")
            })?;
            let target = targets.iter().find(|target| target.path == path);
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
            Ok(McpFileChangeView {
                agent: target.map_or_else(
                    || String::from("AgenTuna"),
                    |target| target.agent.to_string(),
                ),
                scope: scope.to_string(),
                path: path.display().to_string(),
                additions,
                deletions,
                diff,
                selected: true,
                cannot_write_reason: write_refusal(path),
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    let change_set_id = planned
        .store
        .as_ref()
        .or(planned.native.as_ref())
        .map(|set| set.id.clone())
        .ok_or_else(|| {
            CommandError::invalid_argument("agents", "plan contains no supported changes")
        })?;
    let apply_roots = authorized_roots(&paths, project_root.as_deref());
    let warnings = planned.warnings.clone();
    let cache = CachedPlan {
        store: planned.store,
        native: planned.native,
        preview_set,
        preview_root,
        apply_roots,
        store_root: paths.support_dir().to_path_buf(),
        store_file: store_layout.mcp_file(),
        scope,
        backup_folder: paths.support_dir().join(at_store::layout::BACKUPS_DIR),
        agents_by_op: targets
            .iter()
            .map(|target| target.agent.to_string())
            .collect(),
        blocked_agents,
        has_unsupported_drops: !unsupported_fields.is_empty(),
        preview_id: preview_id.clone(),
        store_layout_root: store_layout.root().to_path_buf(),
        slug: plan_slug,
        project_id: cache_project_id,
    };
    plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("MCP plan cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(McpChangeSetPreviewView {
        change_set_id,
        preview_id,
        files,
        unsupported_fields,
        warnings,
        store_init: store_init
            .as_ref()
            .map(super::store_setup::StoreSetupView::from_plan),
    })
}

/// Applies a previously reviewed MCP preview after checking it is still current.
#[tauri::command]
pub fn mcp_save(
    state: State<'_, AppState>,
    request: McpSaveRequest,
) -> Result<McpSaveResultView, CommandError> {
    mcp_save_impl(&state, request)
}

/// Applies the reviewed plan, refusing stale previews and unacknowledged field drops.
///
/// # Errors
/// Returns an error whose code is `external_write_conflict` when a reviewed file changed
/// outside AgenTuna, and a typed validation error when a blocked agent stays selected or a
/// dropped field was not acknowledged.
pub fn mcp_save_impl(
    state: &AppState,
    request: McpSaveRequest,
) -> Result<McpSaveResultView, CommandError> {
    let cached = plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("MCP plan cache is unavailable"))?
        .get(&request.preview_id)
        .cloned()
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "reviewed plan expired; review again")
        })?;
    if cached.preview_id != request.preview_id {
        return Err(CommandError::external_write_conflict(
            "reviewed plan identity does not match; review again",
        ));
    }
    if cached.has_unsupported_drops && !request.acknowledge_unsupported {
        return Err(CommandError::invalid_argument(
            "acknowledge_unsupported",
            "confirm the fields this plan will drop",
        ));
    }
    let current =
        at_engine::preview::preview(&cached.preview_set, &cached.preview_root).map_err(|_| {
            CommandError::external_write_conflict(
                "reviewed files could not be re-read; review again",
            )
        })?;
    if current.id != cached.preview_id {
        return Err(CommandError::external_write_conflict(
            "a reviewed file changed outside AgenTuna",
        ));
    }
    let selected = request
        .selected_agents
        .iter()
        .collect::<std::collections::HashSet<_>>();
    if let Some(blocked) = cached
        .blocked_agents
        .iter()
        .find(|agent| selected.contains(agent))
    {
        return Err(CommandError::invalid_argument(
            "selected_agents",
            &format!("{blocked} cannot be written; deselect it and review again"),
        ));
    }
    let mut native = cached.native;
    let mut selected_targets: Vec<(AgentId, PathBuf)> = Vec::new();
    if let Some(change_set) = &mut native {
        let mut kept = Vec::with_capacity(change_set.ops.len());
        for (op, agent) in change_set.ops.drain(..).zip(cached.agents_by_op) {
            if selected.contains(&agent) {
                if let Ok(agent) = agent.parse::<AgentId>()
                    && let Some(path) = op.destination()
                {
                    selected_targets.push((agent, path.to_path_buf()));
                }
                kept.push(op);
            }
        }
        change_set.ops = kept;
    }
    if native
        .as_ref()
        .is_none_or(|change_set| change_set.ops.is_empty())
    {
        return Err(CommandError::invalid_argument(
            "selected_agents",
            "select at least one native target",
        ));
    }
    plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("MCP plan cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "reviewed plan expired; review again")
        })?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let index_scope = cached.scope;
    let index_root = cached.store_layout_root.clone();
    let index_slug = cached.slug.clone();
    let index_project =
        scoped_project_id(cached.scope, cached.project_id.as_deref()).map(str::to_owned);
    let roots = at_fs::changeset::ApplyRoots::new(cached.apply_roots)
        .map_err(|_| CommandError::unavailable("authorized configuration roots are unavailable"))?;
    let mut store_reversible: Option<at_fs::changeset::RollbackHandle> = None;
    let store_outcome = if let Some(store) = cached.store.as_ref() {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        // Reversible, so a failed native phase restores the store bytes *and* removes the
        // directories this operation created (including a first-run store skeleton).
        let mut outcome =
            at_fs::changeset::apply_with_roots_reversible(store, &mut context, &roots)
                .map_err(CommandError::from)?;
        store_reversible = outcome.rollback_handle.take();
        state.record_writes(store, &cached.store_root);
        Some(outcome)
    } else {
        None
    };
    let native = native
        .ok_or_else(|| CommandError::invalid_argument("agents", "plan has no native changes"))?;
    let native_result = {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        at_fs::changeset::apply_with_roots(&native, &mut context, &roots)
    };
    let native_outcome = match native_result {
        Ok(outcome) => {
            if let Some(handle) = store_reversible.take() {
                at_fs::changeset::commit_reversible(handle).map_err(CommandError::from)?;
            }
            outcome
        }
        Err(error) => {
            if let Some(handle) = store_reversible.take() {
                let mut recorder =
                    at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
                at_fs::changeset::rollback_committed(handle, &mut recorder).map_err(
                    |rollback| {
                        CommandError::internal(format!(
                            "native MCP write failed and canonical store rollback failed: {rollback}"
                        ))
                    },
                )?;
            }
            return Err(CommandError::from(error));
        }
    };
    state.record_writes(&native, &cached.store_root);
    let operation_id = store_outcome
        .map(|outcome| outcome.operation_id)
        .unwrap_or(native_outcome.operation_id);
    // The manifest and the native documents now hold the saved definition; the derived index is
    // what the MCP list and the row actions read, so it has to agree before the next screen load.
    index_saved_server(
        database.connection(),
        index_scope,
        &index_root,
        &index_slug,
        index_project.as_deref(),
        &selected_targets,
        &operation_id,
    )?;
    // One shared store document means one canonical hash for every server it holds: writing
    // this entry changed the document under the other rows too.
    if let Ok(bytes) = std::fs::read(&cached.store_file) {
        at_db::deployments::update_store_hash_for_scope(
            database.connection(),
            at_model::ItemKind::McpServer,
            cached.scope,
            index_project.as_deref(),
            &at_fs::sha256_hex(&bytes),
        )
        .map_err(|error| {
            CommandError::internal(format!("MCP store hashes could not be refreshed: {error}"))
        })?;
    }
    // The native documents now hold what the plan rendered. Refresh every deployment row that
    // targets them, or the next drift scan reports AgenTuna's own write as an external edit.
    for path in native.ops.iter().filter_map(FileOp::destination) {
        at_engine::drift::sync_written_target(
            database.connection(),
            // Claude's project MCP document is the user file; a project save changes the same
            // bytes a user-scope row targets, so refresh every row at the path.
            None,
            path,
            Some(&cached.store_file),
        )
        .map_err(|error| {
            CommandError::internal(format!(
                "MCP deployment baselines could not be refreshed: {error}"
            ))
        })?;
    }
    // The saved definition is AgenTuna-owned now; re-derive the findings so a server that was
    // adopted through this form leaves the "not in AgenTuna" rows immediately.
    if let Some(paths) = state.paths() {
        super::native_scan::refresh_native_findings(paths, database.connection());
    }
    Ok(McpSaveResultView {
        operation_id,
        files_changed: native.ops.len() + cached.store.map_or(0, |store| store.ops.len()),
    })
}

/// Records the canonical identity and per-agent deployment rows for a saved MCP server.
///
/// The reviewed plan writes `store.json` and the native documents; the derived index
/// (`items`, `deployments`) is what the MCP list, the row actions and the drift baselines read.
/// A save that skipped it would leave a server the user cannot see, edit, disable or delete.
fn index_saved_server(
    connection: &rusqlite::Connection,
    scope: Scope,
    store_layout_root: &Path,
    slug_text: &str,
    project_id: Option<&str>,
    selected_targets: &[(AgentId, PathBuf)],
    operation_id: &str,
) -> Result<(), CommandError> {
    // The index requires a project id exactly for Project and Local scopes; a User or Global
    // save must stay project-less even when the screen had an active project selected.
    let project_id = scoped_project_id(scope, project_id);
    let layout = at_store::layout::StoreLayout::new(scope, store_layout_root.to_path_buf());
    let manifest = at_store::manifest::StoreManifest::load(&layout)
        .map_err(|_| CommandError::internal("the saved MCP manifest could not be read"))?;
    let slug = at_store::layout::Slug::parse(slug_text)
        .map_err(|_| CommandError::internal("the saved MCP slug is invalid"))?;
    let Some(item) = manifest
        .manifest
        .item_by_slug(ItemKind::McpServer, &slug)
        .cloned()
    else {
        return Ok(());
    };
    let item_id = at_engine::skill_cli::sync_manifest_item(
        connection,
        &at_engine::skill_cli::ManifestItemInput {
            scope,
            project_id,
            item: &item,
        },
    )
    .map_err(|error| {
        CommandError::internal(format!("the saved server could not be indexed: {error}"))
    })?;
    let store_hash = item.hash.to_string();
    for (agent, path) in selected_targets {
        let target_hash = std::fs::read(path)
            .ok()
            .map(|bytes| at_fs::sha256_hex(&bytes));
        let hashes = at_db::deployments::DeploymentHashes::new(
            Some(&store_hash),
            target_hash.as_deref(),
            target_hash.as_deref(),
        );
        let path_text = path.to_string_lossy().into_owned();
        let mut spec = at_db::deployments::DeploymentSpec::applied(
            &item_id,
            agent,
            scope,
            at_model::ItemState::Linked,
            Some("native"),
            Some(&path_text),
            hashes,
        )
        .with_operation(operation_id);
        if let Some(project) = project_id {
            spec = spec.with_project(project);
        }
        at_db::deployments::upsert_deployment(connection, &spec).map_err(|error| {
            CommandError::internal(format!(
                "the MCP deployment row could not be recorded: {error}"
            ))
        })?;
    }
    Ok(())
}

/// Describes the exact launch a draft would start and issues a one-shot approval for it.
///
/// The UI shows this description in a confirmation dialog; `mcp_test_draft` only starts a
/// process when the echoed approval matches the same transport.
///
/// # Errors
/// Returns a typed command error for invalid arguments or unavailable database access.
#[tauri::command]
pub fn mcp_launch_prepare(
    state: State<'_, AppState>,
    request: McpPlanPreviewRequest,
) -> Result<McpLaunchApprovalView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let transport = make_transport(&request, database.connection())?;
    Ok(state
        .launch_approvals()
        .prepare(&transport, std::time::Instant::now()))
}

/// Unsaved draft plus the one-shot approval the user confirmed for its exact launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpDraftTestRequest {
    /// Draft converted exactly as a save would convert it.
    pub draft: McpPlanPreviewRequest,
    /// Approval echoed from `mcp_launch_prepare` after the confirmation dialog.
    pub approval: McpLaunchApproval,
}

/// Tests the unsaved form draft through the same MCP session used by saved-server health checks.
///
/// The draft is converted exactly as a save would convert it, so the panel reflects the current
/// form values rather than the saved server. No process starts unless `request.approval` is the
/// unconsumed, unexpired approval issued for exactly this launch.
///
/// # Errors
/// Returns a typed command error for invalid arguments, an unverified launch approval, or
/// unavailable database access.
#[tauri::command]
pub async fn mcp_test_draft(
    state: State<'_, AppState>,
    network: State<'_, super::network::NetworkState>,
    request: McpDraftTestRequest,
) -> Result<McpTestResultView, CommandError> {
    use std::str::FromStr;

    let scope = Scope::from_str(&request.draft.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope"))?;
    let transport = {
        let database = state.database().map_err(CommandError::unavailable)?;
        make_transport(&request.draft, database.connection())?
    };
    let consent = super::mcp_launch::consent_for(
        state.launch_approvals(),
        &request.approval,
        &transport,
        std::time::Instant::now(),
    )
    .map_err(|error| CommandError::new(CommandErrorCode::Conflict, error.to_string()))?;
    let display_name = request.draft.name.trim();
    let server = at_model::McpServer::from_def(
        at_model::McpServerId::new(),
        scope,
        McpServerDef {
            display_name: if display_name.is_empty() {
                "Unsaved server".to_owned()
            } else {
                display_name.to_owned()
            },
            description: None,
            transport,
            extra: BTreeMap::new(),
        },
    );
    let secrets: std::sync::Arc<dyn at_secrets::SecretStore> =
        std::sync::Arc::new(at_secrets::KeychainStore::new());
    let stdio = at_mcp::StdioClient::new(
        std::sync::Arc::new(at_mcp::PathResolver::new()),
        std::sync::Arc::clone(&secrets),
    );
    let http = at_mcp::HttpClient::new(std::sync::Arc::new(network.policy()), secrets);
    let result = at_mcp::test_connection(
        &server,
        &stdio,
        &http,
        consent.as_ref(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    Ok(super::commands::mcp_test_result_view(result))
}

fn make_transport(
    request: &McpPlanPreviewRequest,
    connection: &rusqlite::Connection,
) -> Result<McpTransport, CommandError> {
    let convert = |rows: &[McpFormValue]| -> Result<BTreeMap<String, EnvValue>, CommandError> {
        let mut result = BTreeMap::new();
        for row in rows {
            if row.name.trim().is_empty() {
                continue;
            }
            let value = if let Some(name) = row
                .value
                .strip_prefix("${")
                .and_then(|v| v.strip_suffix('}'))
            {
                if name.is_empty() {
                    return Err(CommandError::invalid_argument(
                        "secrets",
                        "secret reference is empty",
                    ));
                }
                let secret_name = at_secrets::SecretName::new(name.to_owned()).map_err(|_| {
                    CommandError::invalid_argument("secrets", "secret reference is invalid")
                })?;
                let account =
                    at_db::secret_refs::keychain_id(connection, &secret_name).map_err(|_| {
                        CommandError::invalid_argument("secrets", "secret reference is unavailable")
                    })?;
                EnvValue::Secret(SecretRef {
                    env_name: row.name.clone(),
                    keychain_account: account.account().to_owned(),
                })
            } else {
                EnvValue::Literal(row.value.clone())
            };
            if result.insert(row.name.clone(), value).is_some() {
                return Err(CommandError::invalid_argument(
                    "values",
                    "duplicate environment or header name",
                ));
            }
        }
        Ok(result)
    };
    match request.transport.as_str() {
        "stdio" => Ok(McpTransport::Stdio {
            command: request.command.trim().to_owned(),
            args: request.args.clone(),
            cwd: None,
            env: convert(&request.env)?,
        }),
        "http" => {
            let url = request.url.parse::<url::Url>().map_err(|_| {
                CommandError::invalid_argument("url", "expected an absolute HTTP URL")
            })?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(CommandError::invalid_argument(
                    "url",
                    "expected an absolute HTTP URL",
                ));
            }
            Ok(McpTransport::Http {
                url,
                headers: convert(&request.headers)?,
                oauth: None,
            })
        }
        _ => Err(CommandError::invalid_argument(
            "transport",
            "expected stdio or http",
        )),
    }
}

fn slugify(name: &str) -> String {
    let slug = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    slug.trim_matches('-').to_owned()
}

fn common_ancestor(set: &ChangeSet) -> Result<PathBuf, CommandError> {
    let mut paths = set.ops.iter().filter_map(FileOp::destination);
    let first = paths
        .next()
        .ok_or_else(|| CommandError::invalid_argument("agents", "plan contains no target files"))?;
    if !first.is_absolute() {
        return Err(CommandError::internal(
            "MCP planner returned a relative native path",
        ));
    }
    let mut root = first
        .parent()
        .ok_or_else(|| CommandError::internal("native path has no parent"))?
        .to_path_buf();
    for path in paths {
        if !path.is_absolute() {
            return Err(CommandError::internal(
                "MCP planner returned a relative native path",
            ));
        }
        while !path.starts_with(&root) {
            if !root.pop() || root.as_os_str().is_empty() {
                return Err(CommandError::invalid_argument(
                    "target",
                    "native targets do not share a safe preview root",
                ));
            }
        }
    }
    Ok(root)
}

fn relative_preview_set(set: &ChangeSet, root: &Path) -> Result<ChangeSet, CommandError> {
    fn strip(path: &mut PathBuf, root: &Path) -> Result<(), CommandError> {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| CommandError::internal("native path escaped preview root"))?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(CommandError::internal("native path is invalid for preview"));
        }
        *path = relative.to_path_buf();
        Ok(())
    }
    let mut result = set.clone();
    for op in &mut result.ops {
        match op {
            FileOp::Write { path, .. }
            | FileOp::Delete { path, .. }
            | FileOp::Mkdir { path, .. } => strip(path, root)?,
            FileOp::Symlink { link_path, .. } => strip(link_path, root)?,
            FileOp::Copy { dest, .. } | FileOp::CopyTree { dest, .. } => strip(dest, root)?,
            FileOp::Move { to, .. } => strip(to, root)?,
            _ => {
                return Err(CommandError::internal(
                    "preview operation has no destination",
                ));
            }
        }
    }
    Ok(result)
}

fn authorized_roots(paths: &at_store::paths::AppPaths, project: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![paths.support_dir().to_path_buf()];
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    roots.extend(
        [paths.global_store(), paths.user_store()]
            .into_iter()
            .map(Path::to_path_buf),
    );
    if let Some(path) = project {
        roots.push(path.to_path_buf());
    }
    roots.retain(|path| path.is_dir());
    roots.sort();
    roots.dedup();
    roots
}

/// Explains why a native target cannot be written, or `None` when it is writable.
///
/// Read-only permission is decided from the target's own mode bits, or from its parent
/// directory when the target does not exist yet, so the review panel can block Save before
/// `at-fs` refuses the write.
fn write_refusal(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.is_dir() {
        return Some("Refused: the target path is a directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o200 == 0 {
            return Some("Permission denied: the file is read-only".to_owned());
        }
    }
    #[cfg(not(unix))]
    {
        if metadata.permissions().readonly() {
            return Some("Permission denied: the file is read-only".to_owned());
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use crate::app::commands::CommandErrorCode;
    use crate::app::drift_testkit::{assert_external_edit_detected, assert_no_drift};
    use crate::app::mcp_servers::{
        ActionKind, McpServerActionOutcome, McpServerActionRequest, run_action,
    };
    use at_db::deployments::DeploymentSpec;
    use at_model::{Agent, ItemState, McpServerDef};
    use at_store::paths::{AppPaths, PathEnvironment};
    use std::sync::Arc;

    struct NullSink;
    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    struct Harness {
        _home: at_fs::testkit::TestHome,
        home: PathBuf,
        state: AppState,
        item_id: ItemId,
    }

    fn seeded_definition(env_value: &str) -> McpServerDef {
        McpServerDef {
            display_name: "github".to_owned(),
            description: None,
            transport: McpTransport::Stdio {
                command: "fixture-server".to_owned(),
                args: vec!["--safe".to_owned()],
                cwd: None,
                env: BTreeMap::from([(
                    "TOKEN".to_owned(),
                    EnvValue::Literal(env_value.to_owned()),
                )]),
            },
            extra: BTreeMap::new(),
        }
    }

    fn native_source(agent: &AgentId) -> &'static str {
        match agent {
            AgentId::ClaudeCode => "{\"mcpServers\":{}}",
            AgentId::Omp => "{}",
            _ => "",
        }
    }

    fn projected(agent: &AgentId, path: &Path, definition: &McpServerDef) -> String {
        let bytes = at_adapters::mcp_projection::project(
            agent,
            &at_adapters::mcp_projection::McpProjectionRequest {
                source: native_source(agent).to_owned(),
                path: path.to_path_buf(),
                scope: Scope::User,
                project: None,
                operation: at_adapters::mcp_projection::McpProjectionOperation::Upsert {
                    name: "github".to_owned(),
                    transport: Box::new(definition.transport.clone()),
                    enabled: true,
                    extra: BTreeMap::new(),
                    env_vars: Vec::new(),
                },
            },
        )
        .expect("native projection");
        String::from_utf8(bytes).expect("UTF-8 native document")
    }

    impl Harness {
        fn new() -> Self {
            let home = at_fs::testkit::TestHome::new().expect("test home");
            let home_path = home.home().to_path_buf();
            let paths = AppPaths::resolve(&PathEnvironment::default().with_home(home_path.clone()))
                .expect("app paths");
            let database = at_db::Database::open_in_memory().expect("database");
            for (id, name) in [
                (AgentId::ClaudeCode, "Claude Code"),
                (AgentId::Codex, "Codex"),
                (AgentId::Omp, "omp"),
            ] {
                at_db::agents::upsert_agent(
                    database.connection(),
                    &Agent {
                        id,
                        display_name: name.to_owned(),
                        detected: true,
                        version: Some("1.0".to_owned()),
                    },
                )
                .expect("agent row");
            }
            let state = AppState::new(
                Arc::new(JobBridge::new(
                    Arc::new(NullSink),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(paths.clone());

            let definition = seeded_definition("old-value");
            let files = [
                (
                    AgentId::ClaudeCode,
                    home_path.join(".claude.json"),
                    "{\"mcpServers\":{}}",
                ),
                (AgentId::Codex, home_path.join(".codex/config.toml"), ""),
                (AgentId::Omp, home_path.join(".omp/agent/mcp.json"), "{}"),
            ];
            for (agent, path, source) in files {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("native directory");
                }
                let document = projected(&agent, &path, &definition);
                assert!(document.len() >= source.len());
                std::fs::write(&path, document).expect("native fixture");
            }

            let layout = at_store::layout::StoreLayout::for_scope(&paths, Scope::User)
                .expect("store layout");
            std::fs::create_dir_all(layout.root()).expect("store root");
            let mut store = at_store::mcp_store::McpStore::empty();
            store.set("github", definition);
            let bytes = store.to_bytes().expect("store bytes");
            std::fs::write(layout.mcp_file(), &bytes).expect("mcp.json");
            let item_id = ItemId::new();
            let mut manifest = at_store::manifest::StoreManifest::new(
                Scope::User,
                at_store::layout::StoreId::new(),
            );
            manifest
                .insert_item(at_store::manifest::ManifestItem::new(
                    item_id,
                    ItemKind::McpServer,
                    at_store::layout::Slug::parse("github").expect("slug"),
                    at_store::manifest::StorePath::parse("mcp.json").expect("store path"),
                    at_store::manifest::ContentHash::from_bytes(&bytes),
                ))
                .expect("manifest item");
            std::fs::write(
                layout.manifest_path(),
                manifest.to_bytes().expect("manifest bytes"),
            )
            .expect("store.json");

            Self {
                _home: home,
                home: home_path,
                state,
                item_id,
            }
        }

        fn native_path(&self, agent: &str) -> PathBuf {
            match agent {
                "claude_code" => self.home.join(".claude.json"),
                "codex" => self.home.join(".codex/config.toml"),
                _ => self.home.join(".omp/agent/mcp.json"),
            }
        }

        fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
            ["claude_code", "codex", "omp"]
                .into_iter()
                .map(|agent| {
                    let path = self.native_path(agent);
                    let bytes = std::fs::read(&path).expect("native bytes");
                    (path, bytes)
                })
                .collect()
        }

        fn store_file(&self) -> PathBuf {
            let paths = self.state.paths().expect("paths");
            at_store::layout::StoreLayout::for_scope(paths, Scope::User)
                .expect("store layout")
                .mcp_file()
        }

        /// Adds a second canonical server that no agent document contains yet.
        fn seed_extra_server(&self) -> ItemId {
            let paths = self.state.paths().expect("paths");
            let layout =
                at_store::layout::StoreLayout::for_scope(paths, Scope::User).expect("store layout");
            let mut store = at_store::mcp_store::McpStore::load(&layout).expect("mcp store");
            let mut definition = seeded_definition("extra-value");
            definition.display_name = "extra".to_owned();
            store.store.set("extra", definition);
            let bytes = store.store.to_bytes().expect("store bytes");
            std::fs::write(layout.mcp_file(), &bytes).expect("mcp.json");
            let mut manifest = at_store::manifest::StoreManifest::load(&layout).expect("manifest");
            let item_id = ItemId::new();
            manifest
                .manifest
                .insert_item(at_store::manifest::ManifestItem::new(
                    item_id,
                    ItemKind::McpServer,
                    at_store::layout::Slug::parse("extra").expect("slug"),
                    at_store::manifest::StorePath::parse("mcp.json").expect("store path"),
                    at_store::manifest::ContentHash::from_bytes(&bytes),
                ))
                .expect("manifest item");
            std::fs::write(
                layout.manifest_path(),
                manifest.manifest.to_bytes().expect("manifest bytes"),
            )
            .expect("store.json");
            item_id
        }
    }

    /// Inserts the index row a deployment row needs for its foreign key.
    fn insert_server_item(state: &AppState, item_id: &ItemId, slug: &str) {
        let database = state.database().expect("database");
        database
            .connection()
            .execute(
                "INSERT OR IGNORE INTO items (id, kind, scope, slug, name, managed) \
                 VALUES (?1, 'mcp_server', 'user', ?2, ?2, 1)",
                rusqlite::params![item_id.to_string(), slug],
            )
            .expect("item row");
    }

    /// Records the pre-command baseline every agent document had for `item_id`.
    fn seed_file_deployments(harness: &Harness, item_id: &ItemId, agents: &[&str]) {
        let store = std::fs::read(harness.store_file()).expect("store");
        let store_hash = at_fs::sha256_hex(&store);
        let database = harness.state.database().expect("database");
        for agent in agents {
            let path = harness.native_path(agent);
            let bytes = std::fs::read(&path).expect("native document");
            let target_hash = at_fs::sha256_hex(&bytes);
            let agent_id = agent.parse::<AgentId>().expect("agent id");
            let target_path = path.to_string_lossy().into_owned();
            let hashes = at_db::deployments::DeploymentHashes::new(
                Some(&store_hash),
                Some(&target_hash),
                Some(&target_hash),
            );
            let spec = DeploymentSpec::applied(
                item_id,
                &agent_id,
                Scope::User,
                ItemState::Linked,
                Some("native"),
                Some(&target_path),
                hashes,
            )
            .with_operation("op_fixture");
            at_db::deployments::upsert_deployment(database.connection(), &spec)
                .expect("deployment row");
        }
    }

    fn preview_id_of(outcome: McpServerActionOutcome) -> String {
        match outcome {
            McpServerActionOutcome::Preview { preview } => preview.preview_id,
            McpServerActionOutcome::Applied { .. } => panic!("expected a reviewed preview"),
        }
    }

    fn action_request(
        item_id: &ItemId,
        agent: Option<&str>,
        apply: bool,
        preview_id: Option<String>,
    ) -> McpServerActionRequest {
        McpServerActionRequest {
            item_id: item_id.to_string(),
            scope: "user".to_owned(),
            project_id: None,
            agent: agent.map(str::to_owned),
            in_repo: false,
            apply,
            preview_id,
        }
    }

    fn apply_action(harness: &Harness, item_id: &ItemId, agent: Option<&str>, kind: ActionKind) {
        let preview = run_action(
            &harness.state,
            action_request(item_id, agent, false, None),
            kind,
        )
        .expect("action preview");
        let preview_id = preview_id_of(preview);
        let applied = run_action(
            &harness.state,
            action_request(item_id, agent, true, Some(preview_id)),
            kind,
        )
        .expect("action apply");
        assert!(matches!(applied, McpServerActionOutcome::Applied { .. }));
    }

    fn request(item_id: Option<&ItemId>, name: &str, env_value: &str) -> McpPlanPreviewRequest {
        McpPlanPreviewRequest {
            name: name.to_owned(),
            transport: "stdio".to_owned(),
            command: "fixture-server".to_owned(),
            args: vec!["--safe".to_owned()],
            url: String::new(),
            headers: Vec::new(),
            env: vec![McpFormValue {
                name: "TOKEN".to_owned(),
                value: env_value.to_owned(),
            }],
            scope: "user".to_owned(),
            agents: vec![
                "claude_code".to_owned(),
                "codex".to_owned(),
                "omp".to_owned(),
            ],
            project_id: None,
            item_id: item_id.map(ToString::to_string),
        }
    }

    fn added_lines(diff: &str) -> Vec<&str> {
        diff.lines()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
            .collect()
    }

    fn removed_lines(diff: &str) -> Vec<&str> {
        diff.lines()
            .filter(|line| line.starts_with('-') && !line.starts_with("---"))
            .collect()
    }

    #[test]
    fn claude_add_preview_diff_matches_the_applied_file_line_for_line() {
        let harness = Harness::new();
        let path = harness.native_path("claude_code");
        let before = std::fs::read_to_string(&path).expect("before");
        let mut add = request(None, "search", "old-value");
        add.agents = vec!["claude_code".to_owned()];
        let view = mcp_plan_preview_impl(&harness.state, add).expect("preview");
        let preview_id = view.preview_id.clone();
        let row = view
            .files
            .iter()
            .find(|file| file.agent == "claude_code")
            .expect("claude row");
        mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id,
                selected_agents: vec!["claude_code".to_owned()],
                acknowledge_unsupported: false,
            },
        )
        .expect("save");
        let after = std::fs::read_to_string(&path).expect("after");
        let mut remaining: Vec<&str> = before.lines().collect();
        let mut inserted: Vec<&str> = Vec::new();
        for line in after.lines() {
            if let Some(index) = remaining.iter().position(|candidate| *candidate == line) {
                remaining.remove(index);
            } else {
                inserted.push(line);
            }
        }
        assert!(
            remaining.is_empty(),
            "the add must leave every existing line untouched: {remaining:?}"
        );
        assert_eq!(
            added_lines(&row.diff)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            inserted
                .iter()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
        );
        assert!(removed_lines(&row.diff).is_empty());
        assert_eq!(u64::try_from(inserted.len()).expect("count"), row.additions);
        assert!(after.contains("\"search\": {"), "{after}");
    }

    #[test]
    fn edit_plan_limits_every_agent_diff_to_the_changed_env_entry() {
        let harness = Harness::new();
        let view = mcp_plan_preview_impl(
            &harness.state,
            request(Some(&harness.item_id), "github", "new-value"),
        )
        .expect("preview");
        let agents: Vec<_> = view
            .files
            .iter()
            .filter(|file| file.agent != "AgenTuna")
            .collect();
        assert_eq!(agents.len(), 3);
        for file in agents {
            assert_eq!(file.additions, 1, "{} should add one line", file.agent);
            assert_eq!(file.deletions, 1, "{} should remove one line", file.agent);
            let added = added_lines(&file.diff);
            let removed = removed_lines(&file.diff);
            assert!(added[0].contains("TOKEN") && added[0].contains("new-value"));
            assert!(removed[0].contains("TOKEN") && removed[0].contains("old-value"));
        }
    }

    #[test]
    fn rename_plan_names_the_lost_and_new_key_and_writes_nothing() {
        let harness = Harness::new();
        let before = harness.snapshot();
        let view = mcp_plan_preview_impl(
            &harness.state,
            request(Some(&harness.item_id), "github-enterprise", "old-value"),
        )
        .expect("rename preview");
        let agents: Vec<_> = view
            .files
            .iter()
            .filter(|file| file.agent != "AgenTuna")
            .collect();
        assert_eq!(agents.len(), 3);
        for file in agents {
            let removed = removed_lines(&file.diff).join("\n");
            let added = added_lines(&file.diff).join("\n");
            assert!(
                removed.contains("github") && !removed.contains("github-enterprise"),
                "{} should lose the old key: {}",
                file.agent,
                file.diff
            );
            assert!(
                added.contains("github-enterprise"),
                "{} should gain the new key: {}",
                file.agent,
                file.diff
            );
        }
        assert_eq!(harness.snapshot(), before, "planning must not write files");
    }

    #[test]
    fn read_only_target_blocks_save_until_it_is_deselected() {
        let harness = Harness::new();
        let codex = harness.native_path("codex");
        let mut permissions = std::fs::metadata(&codex).expect("mode").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&codex, permissions).expect("read-only");
        let view = mcp_plan_preview_impl(
            &harness.state,
            request(Some(&harness.item_id), "github", "new-value"),
        )
        .expect("preview");
        let blocked = view
            .files
            .iter()
            .find(|file| file.agent == "codex")
            .expect("codex row");
        let reason = blocked.cannot_write_reason.clone().expect("refusal reason");
        assert!(reason.contains("Permission denied"));
        assert!(blocked.selected, "a blocked agent stays selected");

        let blocked_save = mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id: view.preview_id.clone(),
                selected_agents: vec!["codex".to_owned()],
                acknowledge_unsupported: false,
            },
        );
        assert!(matches!(
            blocked_save,
            Err(CommandError {
                code: CommandErrorCode::InvalidArgument,
                ..
            })
        ));

        let saved = mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id: view.preview_id,
                selected_agents: vec!["claude_code".to_owned()],
                acknowledge_unsupported: false,
            },
        )
        .expect("save without the read-only agent");
        // Canonical store file, its manifest item and the one selected native document.
        assert_eq!(saved.files_changed, 3);
    }

    #[test]
    fn external_edit_returns_external_write_conflict() {
        let harness = Harness::new();
        let view = mcp_plan_preview_impl(
            &harness.state,
            request(Some(&harness.item_id), "github", "new-value"),
        )
        .expect("preview");
        let claude = harness.native_path("claude_code");
        std::fs::write(&claude, "{\"mcpServers\":{},\"external\":true}").expect("external edit");
        let result = mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id: view.preview_id,
                selected_agents: vec!["claude_code".to_owned()],
                acknowledge_unsupported: false,
            },
        );
        assert!(matches!(
            result,
            Err(CommandError {
                code: CommandErrorCode::ExternalWriteConflict,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&claude).expect("unchanged"),
            "{\"mcpServers\":{},\"external\":true}"
        );
    }

    #[test]
    fn unsupported_fields_require_acknowledgement_before_apply() {
        let harness = Harness::new();
        let mut http = request(None, "remote-fixture", "old-value");
        http.transport = "http".to_owned();
        http.url = "https://example.invalid/mcp".to_owned();
        http.headers = vec![McpFormValue {
            name: "Authorization".to_owned(),
            value: "literal-value".to_owned(),
        }];
        http.env = Vec::new();
        http.agents = vec!["codex".to_owned()];
        let view = mcp_plan_preview_impl(&harness.state, http).expect("preview");
        assert_eq!(
            view.unsupported_fields,
            vec![McpUnsupportedFieldView {
                agent: "codex".to_owned(),
                fields: vec!["headers".to_owned()],
            }]
        );
        let refused = mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id: view.preview_id.clone(),
                selected_agents: vec!["codex".to_owned()],
                acknowledge_unsupported: false,
            },
        );
        assert!(matches!(
            refused,
            Err(CommandError {
                code: CommandErrorCode::InvalidArgument,
                ..
            })
        ));
        assert!(
            mcp_save_impl(
                &harness.state,
                McpSaveRequest {
                    preview_id: view.preview_id,
                    selected_agents: vec!["codex".to_owned()],
                    acknowledge_unsupported: true,
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn saving_refreshes_every_baseline_for_the_written_documents() {
        let harness = Harness::new();
        insert_server_item(&harness.state, &harness.item_id, "github");
        seed_file_deployments(&harness, &harness.item_id, &["claude_code", "codex", "omp"]);
        assert_no_drift(&harness.state, "before mcp_save");

        let view = mcp_plan_preview_impl(
            &harness.state,
            request(Some(&harness.item_id), "github", "new-value"),
        )
        .expect("preview");
        let saved = mcp_save_impl(
            &harness.state,
            McpSaveRequest {
                preview_id: view.preview_id,
                selected_agents: vec![
                    "claude_code".to_owned(),
                    "codex".to_owned(),
                    "omp".to_owned(),
                ],
                acknowledge_unsupported: false,
            },
        )
        .expect("save");
        assert_eq!(
            saved.files_changed, 5,
            "the store document, the manifest and three native documents"
        );
        assert_no_drift(&harness.state, "after mcp_save");
        assert_external_edit_detected(
            &harness.state,
            &harness.native_path("claude_code"),
            b"{\"mcpServers\":{},\"external\":true}",
        );
    }

    /// Seeds the github deployment rows and a second, not-yet-deployed server that shares the
    /// same native documents once enabled.
    fn shared_document_harness() -> (Harness, ItemId) {
        let harness = Harness::new();
        insert_server_item(&harness.state, &harness.item_id, "github");
        let extra = harness.seed_extra_server();
        insert_server_item(&harness.state, &extra, "extra");
        seed_file_deployments(&harness, &harness.item_id, &["claude_code", "codex", "omp"]);
        (harness, extra)
    }

    #[test]
    fn enabling_a_server_refreshes_every_row_sharing_the_document() {
        let (harness, extra) = shared_document_harness();
        assert_no_drift(&harness.state, "before mcp_server_enable");
        apply_action(&harness, &extra, Some("claude_code"), ActionKind::Enable);
        assert_no_drift(&harness.state, "after mcp_server_enable");
        assert_external_edit_detected(
            &harness.state,
            &harness.native_path("claude_code"),
            b"{\"mcpServers\":{},\"external\":true}",
        );
    }

    #[test]
    fn disabling_a_server_refreshes_every_row_sharing_the_document() {
        let (harness, extra) = shared_document_harness();
        apply_action(&harness, &extra, Some("claude_code"), ActionKind::Enable);
        apply_action(
            &harness,
            &harness.item_id,
            Some("claude_code"),
            ActionKind::Disable,
        );
        assert_no_drift(&harness.state, "after mcp_server_disable");
        assert_external_edit_detected(
            &harness.state,
            &harness.native_path("claude_code"),
            b"{\"mcpServers\":{},\"external\":true}",
        );
    }

    #[test]
    fn deleting_a_server_refreshes_every_row_sharing_the_document() {
        let (harness, extra) = shared_document_harness();
        apply_action(&harness, &extra, Some("claude_code"), ActionKind::Enable);
        apply_action(&harness, &harness.item_id, None, ActionKind::Delete);
        assert_no_drift(&harness.state, "after mcp_server_delete");
        assert_external_edit_detected(
            &harness.state,
            &harness.native_path("claude_code"),
            b"{\"mcpServers\":{},\"external\":true}",
        );
    }
}
