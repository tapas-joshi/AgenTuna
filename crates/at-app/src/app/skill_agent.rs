//! Reviewed per-agent skill enablement commands over the engine deployment planner.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{LazyLock, Mutex},
};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::{agents::AgentPresence, deployments::DeploymentSpec};
use at_engine::skill_agent::{
    SetSkillAgentEnabled, SkillAgentPlanRequest, plan_skill_agent_change,
};
use at_model::{
    AgentId, ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, ItemState, LinkMode,
    OperationKind, Scope, SyncLabel,
};
use at_store::layout::{Slug, StoreLayout};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::{
    commands::{CommandError, CommandErrorCode},
    paths::resolve_process_paths,
    state::AppState,
};

/// Inputs for a reviewed per-agent skill state change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentPreviewRequest {
    /// Stable skill identity.
    pub item_id: String,
    /// Scope containing the canonical package.
    pub scope: String,
    /// Agent id whose deployment changes.
    pub agent_id: String,
    /// Desired state.
    pub enabled: bool,
    /// Registered project context for Project and Local scopes.
    pub project_id: Option<String>,
}

/// Read-only preview of one reviewed deployment transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentPreviewView {
    /// Identity of the exact cached plan the apply command will execute.
    pub preview_id: String,
    /// Requested final state.
    pub enabled: bool,
    /// Deployment materialization (`link`, `copy`) when applicable.
    pub mode: Option<String>,
    /// Absolute paths the reviewed operation will change.
    pub paths: Vec<String>,
    /// Other agents that read the same native skills root.
    pub coupled_agents: Vec<String>,
    /// Human-readable warnings, including shared-root coupling.
    pub warnings: Vec<String>,
    /// Whether the desired state is already present.
    pub no_op: bool,
}

/// Outcome of applying a previously reviewed skill deployment plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentApplyView {
    /// Operation id, absent for an idempotent no-op.
    pub operation_id: Option<String>,
    /// Final state.
    pub enabled: bool,
    /// Whether a filesystem/database change was made.
    pub changed: bool,
}

struct CachedPlan {
    set: Option<ChangeSet>,
    store_root: PathBuf,
    backup_root: PathBuf,
    authorized_roots: Vec<PathBuf>,
    item_id: ItemId,
    scope: Scope,
    agent: AgentId,
    project_id: Option<String>,
    source: PathBuf,
    source_hash: String,
    target: PathBuf,
    mode: Option<LinkMode>,
    enabled: bool,
    no_op: bool,
}

static PLANS: LazyLock<Mutex<HashMap<String, CachedPlan>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn plans() -> &'static Mutex<HashMap<String, CachedPlan>> {
    &PLANS
}

/// One reviewed single-item plan handed to a bulk batch (TS-015).
pub(crate) struct ReviewedSkillPlan {
    pub(crate) item_id: ItemId,
    pub(crate) set: Option<ChangeSet>,
    pub(crate) no_op: bool,
    pub(crate) source: PathBuf,
    pub(crate) source_hash: String,
    pub(crate) store_root: PathBuf,
    pub(crate) backup_root: PathBuf,
    pub(crate) authorized_roots: Vec<PathBuf>,
    pub(crate) scope: Scope,
    pub(crate) agent: AgentId,
    pub(crate) project_id: Option<String>,
    pub(crate) target: PathBuf,
    pub(crate) enabled: bool,
}

/// Takes the reviewed plans for `preview_ids`, consuming each one.
///
/// A missing id means the user reviewed it in another session or it already applied, so the
/// whole batch is refused instead of applying a partial selection.
pub(crate) fn take_reviewed_plans(
    preview_ids: &[String],
) -> Result<Vec<ReviewedSkillPlan>, CommandError> {
    let mut cache = plans()
        .lock()
        .map_err(|_| CommandError::internal("skill preview cache is unavailable"))?;
    let mut taken = Vec::with_capacity(preview_ids.len());
    for preview_id in preview_ids {
        let cached = cache.remove(preview_id).ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_ids",
                "a reviewed skill preview expired; review the selection again",
            )
        })?;
        taken.push(ReviewedSkillPlan {
            item_id: cached.item_id,
            set: cached.set,
            no_op: cached.no_op,
            source: cached.source,
            source_hash: cached.source_hash,
            store_root: cached.store_root,
            backup_root: cached.backup_root,
            authorized_roots: cached.authorized_roots,
            scope: cached.scope,
            agent: cached.agent,
            project_id: cached.project_id,
            target: cached.target,
            enabled: cached.enabled,
        });
    }
    Ok(taken)
}

/// Builds and caches the exact ChangeSet for a per-agent skill toggle.
#[tauri::command]
pub fn skill_agent_preview(
    state: State<'_, AppState>,
    request: SkillAgentPreviewRequest,
) -> Result<SkillAgentPreviewView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let item_id = request
        .item_id
        .parse::<ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill id"))?;
    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "unsupported scope"))?;
    let agent = AgentId::from_str(&request.agent_id)
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let item = at_db::shared::items_by_ids(database.connection(), std::slice::from_ref(&item_id))
        .map_err(|_| CommandError::internal("could not read the skill index"))?
        .into_iter()
        .find(|item| item.kind == ItemKind::Skill && item.scope == scope)
        .ok_or_else(|| {
            CommandError::invalid_argument("item_id", "skill is unavailable in this scope")
        })?;
    let project = match request.project_id.as_deref() {
        Some(project_id) => Some(
            at_db::projects::find_project_by_id(database.connection(), project_id)
                .map_err(|_| CommandError::internal("could not read the selected project"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?,
        ),
        None if matches!(scope, Scope::Project | Scope::Local) => {
            return Err(CommandError::invalid_argument(
                "project_id",
                "this scope requires a registered project",
            ));
        }
        None => None,
    };
    let project_root = project.as_ref().map(|project| PathBuf::from(&project.path));
    let paths = resolve_process_paths(project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let layout = StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("the skill store is unavailable"))?;
    let slug = Slug::parse(&item.slug).map_err(|_| {
        CommandError::invalid_argument("item_id", "the skill has an invalid stored slug")
    })?;
    let source = layout.skill_dir(&slug);
    if item
        .source_path
        .as_deref()
        .is_some_and(|stored| Path::new(stored) != source)
    {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the skill store path changed; refresh the skill list",
        ));
    }
    if !source.is_dir() {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the canonical skill package is unavailable",
        ));
    }
    let source_hash = at_store::manifest::hash_skill_folder(&source)
        .map_err(|_| CommandError::unavailable("the canonical skill package could not be hashed"))?
        .to_string();
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: project_root.clone(),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let adapter = registry
        .get(&agent)
        .map_err(|_| CommandError::internal("agent registry is unavailable"))?
        .ok_or_else(|| {
            CommandError::invalid_argument("agent_id", "agent adapter is unavailable")
        })?;
    let candidate = adapter
        .paths(ItemKind::Skill, scope, &env)
        .into_iter()
        .find(|candidate| candidate.primary)
        .ok_or_else(|| {
            CommandError::invalid_argument("scope", "agent has no skill directory for this scope")
        })?;
    let target = candidate.path.join(slug.as_str());
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not read agent installations"))?;
    let agent_row = agents
        .iter()
        .find(|row| row.id == agent)
        .ok_or_else(|| CommandError::invalid_argument("agent_id", "agent is not installed"))?;
    let installed = agent_row.presence == AgentPresence::Installed;
    let managed = at_db::agents::is_agent_managed(database.connection(), &agent)
        .map_err(|_| CommandError::internal("could not read agent management settings"))?;
    let deployment = at_db::deployments::find_deployment(
        database.connection(),
        &item_id,
        &agent,
        scope,
        request.project_id.as_deref(),
    )
    .map_err(|_| CommandError::internal("could not read the skill deployment"))?;
    let already_enabled = deployment
        .as_ref()
        .is_some_and(|cell| cell.state == ItemState::Linked);
    let destination_managed = deployment.as_ref().is_some_and(|cell| {
        cell.state == ItemState::Linked
            && cell.target_path.as_deref() == Some(target.to_string_lossy().as_ref())
    });
    let lock_reason = at_db::locks::find_lock(database.connection(), &item_id, Scope::Global)
        .map_err(|_| CommandError::internal("could not read skill locks"))?
        .and_then(|lock| lock.reason);
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not read deployment settings"))?;
    let mode = settings.link_mode(&agent);
    let supports_symlink = adapter.capabilities(&env).supports_symlink;
    let mut shared_agents = Vec::new();
    for other in &agents {
        let Some(other_adapter) = registry
            .get(&other.id)
            .map_err(|_| CommandError::internal("agent registry is unavailable"))?
        else {
            continue;
        };
        if other_adapter
            .paths(ItemKind::Skill, scope, &env)
            .iter()
            .any(|path| path.primary && path.path == candidate.path)
        {
            shared_agents.push(other.id.clone());
        }
    }
    let existing_hash = if request.enabled {
        None
    } else {
        at_fs::hash_node(&target).map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "the deployed skill could not be safely inspected",
            )
        })?
    };
    if !request.enabled && already_enabled && existing_hash.is_none() {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the recorded skill deployment is missing; refresh the skill state before disabling",
        ));
    }
    let disable_change_set = match existing_hash {
        Some(hash) => Some(ChangeSet {
            id: uuid::Uuid::now_v7().to_string(),
            preview_id: uuid::Uuid::now_v7().to_string(),
            operation_kind: OperationKind::parse("deploy")
                .map_err(|_| CommandError::internal("deployment history type is unavailable"))?,
            scope: Some(scope),
            project_id: request.project_id.clone(),
            summary: format!("Disable skill {} for {}", item.name, agent),
            ops: vec![FileOp::Delete {
                path: target.clone(),
                expected: ExpectedState::hash(hash)
                    .map_err(|_| CommandError::internal("the deployment hash is invalid"))?,
            }],
            created_at_unix_seconds: 0,
        }),
        None => None,
    };
    let deployment_request = at_engine::deploy::PlanRequest {
        item_id,
        scope,
        agent: agent.clone(),
        store_root: layout.root().to_path_buf(),
        source: source.clone(),
        destination: target.clone(),
        mode,
        link_supported: supports_symlink,
        managed,
        destination_managed,
        shared_agents,
        project_id: request.project_id.clone(),
        summary: format!("Set skill {} for {}", item.name, agent),
    };
    let planned = plan_skill_agent_change(SkillAgentPlanRequest {
        intent: SetSkillAgentEnabled {
            item_id,
            scope,
            agent: agent.clone(),
            enabled: request.enabled,
        },
        deployment: deployment_request,
        agent_installed: installed,
        lock_reason,
        already_enabled,
        disable_change_set,
    })
    .map_err(|error| CommandError::new(CommandErrorCode::Conflict, error.to_string()))?;
    let preview_id = planned
        .change_set
        .as_ref()
        .map(|set| set.preview_id.clone())
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let change_set = planned.change_set.clone();
    let paths_changed = change_set
        .as_ref()
        .map(|set| {
            set.ops
                .iter()
                .filter_map(|op| op.destination())
                .map(|path| path.display().to_string())
                .collect()
        })
        .unwrap_or_default();
    let warnings = planned
        .warnings
        .iter()
        .map(|warning| format!("{warning:?}"))
        .collect();
    let mut authorized_roots = vec![env.home.clone()];
    if let Some(project_root) = &project_root
        && !project_root.starts_with(&env.home)
    {
        authorized_roots.push(project_root.clone());
    }
    if !candidate.path.starts_with(&env.home)
        && project_root
            .as_ref()
            .is_none_or(|root| !candidate.path.starts_with(root))
    {
        let root = super::skill_wizard::existing_authorized_ancestor(&candidate.path)?;
        if !authorized_roots.contains(&root) {
            authorized_roots.push(root);
        }
    }
    let cached = CachedPlan {
        set: change_set,
        store_root: layout.root().to_path_buf(),
        backup_root: paths.backup_dir().to_path_buf(),
        authorized_roots,
        item_id,
        scope,
        agent: agent.clone(),
        project_id: request.project_id,
        source,
        source_hash,
        target,
        mode: planned.mode_used,
        enabled: request.enabled,
        no_op: planned.no_op,
    };
    plans()
        .lock()
        .map_err(|_| CommandError::internal("skill preview cache is unavailable"))?
        .insert(preview_id.clone(), cached);
    Ok(SkillAgentPreviewView {
        preview_id,
        enabled: request.enabled,
        mode: planned.mode_used.map(|mode| mode.to_string()),
        paths: paths_changed,
        coupled_agents: planned
            .coupled_agents
            .iter()
            .map(ToString::to_string)
            .collect(),
        warnings,
        no_op: planned.no_op,
    })
}

/// Applies the exact cached preview after the UI has shown it to the user.
#[tauri::command]
pub fn skill_agent_apply(
    state: State<'_, AppState>,
    preview_id: String,
) -> Result<SkillAgentApplyView, CommandError> {
    let cached = plans()
        .lock()
        .map_err(|_| CommandError::internal("skill preview cache is unavailable"))?
        .remove(&preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "the skill preview expired; review it again",
            )
        })?;
    if cached.no_op {
        return Ok(SkillAgentApplyView {
            operation_id: None,
            enabled: cached.enabled,
            changed: false,
        });
    }
    let current_source_hash = at_store::manifest::hash_skill_folder(&cached.source)
        .map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "the canonical skill package changed after review",
            )
        })?
        .to_string();
    if current_source_hash != cached.source_hash {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the canonical skill package changed after review; preview the skill again",
        ));
    }
    let set = cached
        .set
        .as_ref()
        .ok_or_else(|| CommandError::internal("the reviewed skill plan is missing"))?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.authorized_roots)
        .map_err(|_| CommandError::unavailable("authorized skill directories are unavailable"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &cached.store_root,
        backup_folder: &cached.backup_root,
        recorder: &mut recorder,
    };
    let outcome = at_fs::changeset::apply_with_roots(set, &mut context, &roots).map_err(
        |error| match error {
            at_fs::changeset::ChangeSetApplyError::Stale(path) => CommandError::new(
                CommandErrorCode::Conflict,
                format!(
                    "The reviewed skill target changed at {}. Review the current state again.",
                    path.display()
                ),
            ),
            other => CommandError::from(other),
        },
    )?;
    state.record_writes(set, &cached.store_root);
    let store_hash = at_store::manifest::hash_skill_folder(&cached.source)
        .map_err(|_| CommandError::unavailable("the canonical skill package could not be hashed"))?
        .to_string();
    let stored_mode = if cached.enabled {
        Some(match cached.mode.unwrap_or(LinkMode::Link) {
            LinkMode::Link => "symlink",
            LinkMode::Copy => "copy",
        })
    } else {
        None
    };
    let target_text = cached.target.to_string_lossy().into_owned();
    let source_text = cached.source.to_string_lossy().into_owned();
    let target_hash = if cached.enabled {
        Some(
            at_fs::hash_node(&cached.target)
                .map_err(|_| CommandError::unavailable("the deployed skill could not be verified"))?
                .ok_or_else(|| {
                    CommandError::unavailable("the deployed skill is missing after apply")
                })?,
        )
    } else {
        None
    };
    let state_value = if cached.enabled {
        ItemState::Linked
    } else {
        ItemState::Off
    };
    let spec = DeploymentSpec::new(&cached.item_id, &cached.agent, cached.scope, state_value)
        .with_target_path(&target_text)
        .with_mode(stored_mode.unwrap_or("none"))
        .with_hashes(
            Some(&store_hash),
            target_hash.as_deref(),
            cached.enabled.then_some(store_hash.as_str()),
        )
        .with_sync_label(SyncLabel::InSync)
        .with_operation(&outcome.operation_id);
    let spec = if cached.enabled && cached.mode == Some(LinkMode::Link) {
        spec.with_link_target(&source_text)
    } else {
        spec
    };
    let spec = match cached.project_id.as_deref() {
        Some(project_id) => spec.with_project(project_id),
        None => spec,
    };
    at_db::deployments::upsert_deployment(database.connection(), &spec)
        .map_err(|_| CommandError::internal("the deployment record could not be updated"))?;
    Ok(SkillAgentApplyView {
        operation_id: Some(outcome.operation_id),
        enabled: cached.enabled,
        changed: true,
    })
}
