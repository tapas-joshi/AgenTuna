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
    commands::{CommandError, McpChangeSetPreviewView, McpFileChangeView},
    paths::resolve_process_paths,
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
}

/// Identity used to apply a reviewed MCP plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpSaveRequest {
    /// Content-bound identity returned by `mcp_plan_preview`.
    pub preview_id: String,
    /// Selected target agents in the reviewed plan.
    pub selected_agents: Vec<String>,
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
    store_rollback: Option<ChangeSet>,
    preview_set: ChangeSet,
    preview_root: PathBuf,
    apply_roots: Vec<PathBuf>,
    store_root: PathBuf,
    backup_folder: PathBuf,
    agents_by_op: Vec<String>,
    preview_id: String,
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
    let project_root = match request.project_id.as_deref() {
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
    let paths = resolve_process_paths(project_root.as_deref())
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
    let mut targets = Vec::with_capacity(request.agents.len());
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
            at_fs::symlink::read_nofollow(&candidate.path).map_err(|_| {
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
        targets.push(at_engine::mcp_plan::McpTarget {
            agent: agent.clone(),
            scope,
            item_id: ItemId::new(),
            slug: slug.clone(),
            path: candidate.path,
            source,
            expected,
            unsupported_acknowledged: false,
            installed: installed.contains(&agent),
        });
    }
    let intent = at_engine::mcp_plan::McpIntent::Add { slug, definition };
    let project_id = request.project_id.clone();
    let store_layout = at_store::layout::StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::invalid_argument("scope", "canonical store is unavailable"))?;
    let mut planned = at_engine::mcp_plan::plan(at_engine::mcp_plan::McpPlanRequest {
        registry: &registry,
        env: &env,
        intent: &intent,
        targets: &targets,
        definition: match &intent {
            at_engine::mcp_plan::McpIntent::Add { definition, .. } => definition,
            _ => return Err(CommandError::internal("MCP plan intent is invalid")),
        },
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
    let preview = at_engine::preview::preview(&preview_set, &preview_root).map_err(|_| {
        CommandError::invalid_argument("target", "native config changed or could not be previewed")
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
                cannot_write_reason: None,
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
    let cache = CachedPlan {
        store: planned.store,
        native: planned.native,
        store_rollback: planned.store_rollback,
        preview_set,
        preview_root,
        apply_roots,
        store_root: paths.support_dir().to_path_buf(),
        backup_folder: paths.support_dir().join(at_store::layout::BACKUPS_DIR),
        agents_by_op: targets
            .iter()
            .map(|target| target.agent.to_string())
            .collect(),
        preview_id: preview_id.clone(),
    };
    plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("MCP plan cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(McpChangeSetPreviewView {
        change_set_id,
        preview_id,
        files,
    })
}

/// Applies a previously reviewed MCP preview after checking it is still current.
#[tauri::command]
pub fn mcp_save(
    state: State<'_, AppState>,
    request: McpSaveRequest,
) -> Result<McpSaveResultView, CommandError> {
    let cached = plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("MCP plan cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "reviewed plan expired; review again")
        })?;
    if cached.preview_id != request.preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "reviewed plan identity does not match",
        ));
    }
    let current =
        at_engine::preview::preview(&cached.preview_set, &cached.preview_root).map_err(|_| {
            CommandError::invalid_argument("preview_id", "reviewed files changed; review again")
        })?;
    if current.id != cached.preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "reviewed files changed; review again",
        ));
    }
    let selected = request
        .selected_agents
        .iter()
        .collect::<std::collections::HashSet<_>>();
    let mut native = cached.native;
    if let Some(change_set) = &mut native {
        change_set.ops = change_set
            .ops
            .drain(..)
            .zip(cached.agents_by_op)
            .filter_map(|(op, agent)| selected.contains(&agent).then_some(op))
            .collect();
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
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.apply_roots)
        .map_err(|_| CommandError::unavailable("authorized configuration roots are unavailable"))?;
    let store_outcome = if let Some(store) = cached.store.as_ref() {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &cached.store_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        Some(
            at_fs::changeset::apply_with_roots(store, &mut context, &roots)
                .map_err(CommandError::from)?,
        )
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
        Ok(outcome) => outcome,
        Err(error) => {
            if store_outcome.is_some() {
                let rollback = cached.store_rollback.as_ref().ok_or_else(|| {
                    CommandError::internal(
                        "native MCP write failed and no store rollback plan exists",
                    )
                })?;
                let mut recorder =
                    at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
                let mut context = at_fs::changeset::ApplyContext {
                    store_root: &cached.store_root,
                    backup_folder: &cached.backup_folder,
                    recorder: &mut recorder,
                };
                at_fs::changeset::apply_with_roots(rollback, &mut context, &roots).map_err(
                    |_| {
                        CommandError::internal(
                            "native MCP write failed and canonical store rollback failed",
                        )
                    },
                )?;
            }
            return Err(CommandError::from(error));
        }
    };
    let operation_id = store_outcome
        .map(|outcome| outcome.operation_id)
        .unwrap_or(native_outcome.operation_id);
    Ok(McpSaveResultView {
        operation_id,
        files_changed: native.ops.len() + cached.store.map_or(0, |store| store.ops.len()),
    })
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
    let mut result = set.clone();
    for op in &mut result.ops {
        if let FileOp::Write { path, .. } = op {
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
