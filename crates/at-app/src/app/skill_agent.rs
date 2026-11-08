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
    OperationKind, Scope,
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

/// Discards a reviewed skill-agent preview after another item blocks an atomic bulk request.
#[tauri::command]
pub fn skill_agent_discard(preview_id: String) -> Result<bool, CommandError> {
    Ok(plans()
        .lock()
        .map_err(|_| CommandError::internal("skill preview cache is unavailable"))?
        .remove(&preview_id)
        .is_some())
}

/// Builds and caches the exact ChangeSet for a per-agent skill toggle.
#[tauri::command]
pub fn skill_agent_preview(
    state: State<'_, AppState>,
    request: SkillAgentPreviewRequest,
) -> Result<SkillAgentPreviewView, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let project_root = preview_project_root(database.connection(), &request)?;
    let paths = resolve_process_paths(project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    skill_agent_preview_impl(database.connection(), &paths, request)
}

/// Resolves the registered project for a preview request, if the scope needs one.
fn preview_project_root(
    connection: &rusqlite::Connection,
    request: &SkillAgentPreviewRequest,
) -> Result<Option<PathBuf>, CommandError> {
    match request.project_id.as_deref() {
        Some(project_id) => Ok(Some(PathBuf::from(
            at_db::projects::find_project_by_id(connection, project_id)
                .map_err(|_| CommandError::internal("could not read the selected project"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?
                .path,
        ))),
        None if matches!(
            Scope::from_str(&request.scope).unwrap_or(Scope::User),
            Scope::Project | Scope::Local
        ) =>
        {
            Err(CommandError::invalid_argument(
                "project_id",
                "this scope requires a registered project",
            ))
        }
        None => Ok(None),
    }
}

/// Preview implementation over injected database and paths so tests never touch the real HOME.
pub(crate) fn skill_agent_preview_impl(
    database: &rusqlite::Connection,
    paths: &at_store::paths::AppPaths,
    request: SkillAgentPreviewRequest,
) -> Result<SkillAgentPreviewView, CommandError> {
    let item_id = request
        .item_id
        .parse::<ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill id"))?;
    let scope = Scope::from_str(&request.scope)
        .map_err(|_| CommandError::invalid_argument("scope", "unsupported scope"))?;
    let agent = AgentId::from_str(&request.agent_id)
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let item = at_db::shared::items_by_ids(database, std::slice::from_ref(&item_id))
        .map_err(|_| CommandError::internal("could not read the skill index"))?
        .into_iter()
        .find(|item| item.kind == ItemKind::Skill && item.scope == scope)
        .ok_or_else(|| {
            CommandError::invalid_argument("item_id", "skill is unavailable in this scope")
        })?;
    let project_root = preview_project_root(database, &request)?;
    let layout = StoreLayout::for_scope(paths, scope)
        .map_err(|_| CommandError::unavailable("the skill store is unavailable"))?;
    let slug = Slug::parse(&item.slug).map_err(|_| {
        CommandError::invalid_argument("item_id", "the skill has an invalid stored slug")
    })?;
    let source = layout.skill_dir(&slug);
    if item.source_path.as_deref().is_some_and(|stored| {
        let p = Path::new(stored);
        if p.is_absolute() {
            p != source
        } else {
            layout.root().join(p) != source
        }
    }) {
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
    let agents = at_db::agents::list_agents_on_mac(database)
        .map_err(|_| CommandError::internal("could not read agent installations"))?;
    let agent_row = agents
        .iter()
        .find(|row| row.id == agent)
        .ok_or_else(|| CommandError::invalid_argument("agent_id", "agent is not installed"))?;
    let installed = agent_row.presence == AgentPresence::Installed;
    let managed = at_db::agents::is_agent_managed(database, &agent)
        .map_err(|_| CommandError::internal("could not read agent management settings"))?;
    let deployment = at_db::deployments::find_deployment(
        database,
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
    let lock_reason = at_db::locks::find_lock(database, &item_id, Scope::Global)
        .map_err(|_| CommandError::internal("could not read skill locks"))?
        .and_then(|lock| lock.reason);
    let settings = at_db::settings::load(database)
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
    .map_err(|error| match error {
        at_engine::skill_agent::SkillAgentPlanError::NativeConflict { path } => CommandError {
            code: CommandErrorCode::RefusedOverwrite,
            message: format!(
                "{} holds an unmanaged skill folder; adopt it or move it aside first.",
                path.display()
            ),
            refusal: Some(Box::new(super::commands::RefusedOverwriteView {
                path: path.display().to_string(),
                kind: "directory".to_owned(),
                existing_hash: None,
                remedies: vec!["adopt".to_owned(), "move_aside".to_owned()],
            })),
        },
        locked @ at_engine::skill_agent::SkillAgentPlanError::Locked { .. } => {
            CommandError::new(CommandErrorCode::Locked, locked.to_string())
        }
        other => CommandError::new(CommandErrorCode::Conflict, other.to_string()),
    })?;
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
        .map(|warning| warning.to_string())
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
    skill_agent_apply_impl(&state, &preview_id)
}

/// Apply implementation over the injected app state so tests use sandbox paths and database.
pub(crate) fn skill_agent_apply_impl(
    state: &AppState,
    preview_id: &str,
) -> Result<SkillAgentApplyView, CommandError> {
    let cached = plans()
        .lock()
        .map_err(|_| CommandError::internal("skill preview cache is unavailable"))?
        .remove(preview_id)
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
    let outcome = at_fs::changeset::apply_with_roots_and_registry(
        set,
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map_err(|error| match error {
        at_fs::changeset::ChangeSetApplyError::Stale(path) => CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "The reviewed skill target changed at {}. Review the current state again.",
                path.display()
            ),
        ),
        other => CommandError::from(other),
    })?;
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
    let hashes = at_db::deployments::DeploymentHashes::new(
        Some(&store_hash),
        target_hash.as_deref(),
        cached.enabled.then_some(store_hash.as_str()),
    );
    let spec = DeploymentSpec::applied(
        &cached.item_id,
        &cached.agent,
        cached.scope,
        state_value,
        Some(stored_mode.unwrap_or("none")),
        Some(&target_text),
        hashes,
    )
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
    if cached.enabled {
        // The native directory now holds the deployed package. Refresh every row that shares
        // it (a shared skills root couples agents) so drift does not report our own write.
        at_engine::drift::sync_written_target(
            database.connection(),
            Some(cached.scope),
            &cached.target,
            Some(&cached.source),
        )
        .map_err(|error| {
            CommandError::internal(format!(
                "skill deployment baselines could not be refreshed: {error}"
            ))
        })?;
    }
    Ok(SkillAgentApplyView {
        operation_id: Some(outcome.operation_id),
        enabled: cached.enabled,
        changed: true,
    })
}

#[cfg(test)]
mod command_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
    use at_db::{Database, agents, backups, deployments, locks, ops::OperationActor, settings};
    use at_model::{Agent, AgentId, ItemState, LinkMode, Scope, SyncLabel};
    use at_store::layout::{Slug, StoreLayout};
    use at_store::paths::{AppPaths, PathEnvironment};

    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use crate::app::bulk_skill::{BulkSkillApplyRequest, bulk_skill_apply_inner};
    use crate::app::drift_testkit::{assert_external_edit_detected_at, assert_no_drift};
    use crate::app::skill_detail::skill_edit_apply_inner;
    use crate::app::state::AppState;

    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    /// One sandboxed home, injected paths, file-backed database and store package.
    struct Sandbox {
        _home: at_fs::testkit::TestHome,
        paths: AppPaths,
        database_path: PathBuf,
        database: Database,
        item_id: ItemId,
        slug: Slug,
    }

    impl Sandbox {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let home = at_fs::testkit::TestHome::new()?;
            let paths = AppPaths::resolve(
                &PathEnvironment::default().with_home(home.home().to_path_buf()),
            )?;
            let database_path = home.home().join("agentuna").join("index.db");
            if let Some(parent) = database_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let database = Database::open_file(&database_path)?;
            {
                let connection = database.connection();
                agents::upsert_agent(
                    connection,
                    &Agent {
                        id: AgentId::ClaudeCode,
                        display_name: "Claude Code".to_owned(),
                        detected: true,
                        version: Some("1.0".to_owned()),
                    },
                )?;
                agents::set_agent_managed(connection, &AgentId::ClaudeCode, true)?;
                let layout = StoreLayout::for_scope(&paths, Scope::User)?;
                let slug = Slug::parse("demo-skill")?;
                let source = layout.skill_dir(&slug);
                std::fs::create_dir_all(&source)?;
                std::fs::write(
                    source.join("SKILL.md"),
                    "---\nname: demo-skill\ndescription: demo\n---\n\nBody.\n",
                )?;
                let item_id = ItemId::new();
                let hash = at_store::manifest::hash_skill_folder(&source)?.to_string();
                connection.execute(
                    "INSERT INTO items (id, kind, scope, slug, name, source_path, content_hash, \
                     state, managed) VALUES (?1, 'skill', 'user', ?2, 'demo-skill', ?3, ?4, \
                     'present', 1)",
                    rusqlite::params![
                        item_id.to_string(),
                        slug.as_str(),
                        source.to_string_lossy(),
                        hash
                    ],
                )?;
                Ok(Self {
                    _home: home,
                    paths,
                    database_path,
                    database,
                    item_id,
                    slug,
                })
            }
        }

        fn request(&self, enabled: bool) -> SkillAgentPreviewRequest {
            SkillAgentPreviewRequest {
                item_id: self.item_id.to_string(),
                scope: "user".to_owned(),
                agent_id: "claude_code".to_owned(),
                enabled,
                project_id: None,
            }
        }

        fn agent_skills_dir(&self) -> PathBuf {
            let registry = AgentAdapterRegistry::register();
            let adapter = registry
                .get(&AgentId::ClaudeCode)
                .expect("registry")
                .expect("claude adapter");
            let env = AgentEnvironment {
                home: self.paths.home_dir().expect("home").to_path_buf(),
                project: None,
                variables: Default::default(),
            };
            adapter
                .paths(ItemKind::Skill, Scope::User, &env)
                .into_iter()
                .find(|candidate| candidate.primary)
                .expect("claude user skills dir")
                .path
        }

        /// A second database handle over the same file, as the app shell holds.
        fn state(&self) -> AppState {
            let database = Database::open_file(&self.database_path).expect("second handle");
            AppState::new(
                Arc::new(JobBridge::new(
                    Arc::new(NullSink),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(self.paths.clone())
        }

        fn source_dir(&self) -> PathBuf {
            StoreLayout::for_scope(&self.paths, Scope::User)
                .expect("layout")
                .skill_dir(&self.slug)
        }
    }

    #[test]
    fn enable_then_disable_round_trips_a_user_scope_symlink() {
        let sandbox = Sandbox::new().expect("sandbox");
        let target = sandbox.agent_skills_dir().join(sandbox.slug.as_str());
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("enable preview");
        assert!(!preview.no_op);
        assert_eq!(preview.mode.as_deref(), Some("link"));
        assert_eq!(preview.paths, vec![target.display().to_string()]);

        let applied =
            skill_agent_apply_impl(&sandbox.state(), &preview.preview_id).expect("enable apply");
        assert!(applied.changed);
        assert!(
            target.symlink_metadata().is_ok(),
            "enable creates a symlink"
        );
        let deployment = deployments::find_deployment(
            sandbox.database.connection(),
            &sandbox.item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            None,
        )
        .expect("deployment read")
        .expect("recorded");
        assert_eq!(deployment.state, ItemState::Linked);
        assert_eq!(deployment.mode.as_deref(), Some("symlink"));
        assert_eq!(deployment.sync_label, Some(SyncLabel::InSync));
        let resolved_target = target
            .parent()
            .unwrap()
            .join(std::fs::read_link(&target).expect("link target"))
            .canonicalize()
            .expect("canonical link target");
        assert_eq!(
            resolved_target,
            sandbox
                .source_dir()
                .canonicalize()
                .expect("canonical source"),
        );

        // Re-applying the same state must not create a second operation.
        let repeat = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("repeat preview");
        assert!(repeat.no_op);
        let repeat_apply =
            skill_agent_apply_impl(&sandbox.state(), &repeat.preview_id).expect("repeat apply");
        assert!(!repeat_apply.changed);
        assert!(repeat_apply.operation_id.is_none());

        // Disabling removes only the deployed link, never the store package.
        let disable = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(false),
        )
        .expect("disable preview");
        assert_eq!(disable.paths, vec![target.display().to_string()]);
        let disabled =
            skill_agent_apply_impl(&sandbox.state(), &disable.preview_id).expect("disable apply");
        assert!(disabled.changed);
        assert!(
            target.symlink_metadata().is_err(),
            "the deployed link is gone"
        );
        assert!(
            sandbox.source_dir().join("SKILL.md").exists(),
            "the store package survives"
        );
        let off = deployments::find_deployment(
            sandbox.database.connection(),
            &sandbox.item_id,
            &AgentId::ClaudeCode,
            Scope::User,
            None,
        )
        .expect("deployment read")
        .expect("recorded");
        assert_eq!(off.state, ItemState::Off);
        let operations: i64 = sandbox
            .database
            .connection()
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .expect("operations count");
        assert_eq!(operations, 2, "enable and disable, no idempotent repeats");
    }

    #[test]
    fn a_missing_agent_writes_nothing() {
        let sandbox = Sandbox::new().expect("sandbox");
        let request = SkillAgentPreviewRequest {
            agent_id: "codex".to_owned(),
            ..sandbox.request(true)
        };
        let error =
            skill_agent_preview_impl(sandbox.database.connection(), &sandbox.paths, request)
                .expect_err("codex is not seeded");
        assert!(error.message.contains("not installed"), "{error:?}");
        assert!(
            !sandbox
                .agent_skills_dir()
                .join(sandbox.slug.as_str())
                .symlink_metadata()
                .is_ok(),
            "nothing is written"
        );
    }

    #[test]
    fn a_locked_skill_is_refused_without_a_changeset() {
        let mut sandbox = Sandbox::new().expect("sandbox");
        locks::upsert_lock(
            sandbox.database.connection_mut(),
            &locks::LockSpec {
                item_id: &sandbox.item_id,
                scope: Scope::Global,
                reason: Some("pinned"),
                locked_by: Some("ui"),
            },
        )
        .expect("lock");
        let error = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect_err("locked skills cannot deploy");
        assert!(error.message.contains("locked"), "{error:?}");
        assert!(
            !sandbox
                .agent_skills_dir()
                .join(sandbox.slug.as_str())
                .symlink_metadata()
                .is_ok()
        );
    }

    #[test]
    fn copy_mode_creates_a_copy_and_disable_records_a_backup() {
        let mut sandbox = Sandbox::new().expect("sandbox");
        settings::set_link_mode(
            sandbox.database.connection_mut(),
            &AgentId::ClaudeCode,
            LinkMode::Copy,
            OperationActor::Ui,
        )
        .expect("copy mode");
        let target = sandbox.agent_skills_dir().join(sandbox.slug.as_str());
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("enable preview");
        assert_eq!(preview.mode.as_deref(), Some("copy"));
        skill_agent_apply_impl(&sandbox.state(), &preview.preview_id).expect("apply");
        let metadata = target.symlink_metadata().expect("copy exists");
        assert!(!metadata.file_type().is_symlink(), "a real copied folder");
        let copy_target = std::fs::read_to_string(target.join("SKILL.md")).expect("copy content");
        assert!(copy_target.contains("demo-skill"));

        let disable = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(false),
        )
        .expect("disable preview");
        skill_agent_apply_impl(&sandbox.state(), &disable.preview_id).expect("disable apply");
        assert!(target.symlink_metadata().is_err());
        let backups = backups::list_backups(
            sandbox.database.connection(),
            backups::BackupQuery {
                path_prefix: None,
                from: None,
                to: None,
                operation_id: None,
                limit: None,
            },
        )
        .expect("backup catalogue");
        assert!(
            backups.iter().any(|entry| entry
                .original_path
                .as_deref()
                .is_some_and(|path| path.contains("demo-skill"))),
            "the copied deployment was backed up before removal: {backups:?}"
        );
    }

    #[test]
    fn enabling_a_skill_refreshes_the_drift_baseline() {
        let sandbox = Sandbox::new().expect("sandbox");
        let state = sandbox.state();
        let target = sandbox.agent_skills_dir().join(sandbox.slug.as_str());
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("enable preview");
        skill_agent_apply_impl(&state, &preview.preview_id).expect("enable apply");
        assert_no_drift(&state, "after skill_agent_apply");
        assert_external_edit_detected_at(
            &state,
            &target.join("SKILL.md"),
            &target,
            b"---\nname: demo-skill\n---\n\nExternal body.\n",
        );
    }

    #[test]
    fn bulk_enabling_skills_refreshes_the_drift_baseline() {
        let sandbox = Sandbox::new().expect("sandbox");
        let state = sandbox.state();
        let target = sandbox.agent_skills_dir().join(sandbox.slug.as_str());
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("enable preview");
        let batch = bulk_skill_apply_inner(
            &state,
            BulkSkillApplyRequest {
                preview_ids: vec![preview.preview_id],
                label: "Enable demo-skill".to_owned(),
            },
        )
        .expect("bulk apply");
        assert!(batch.applied, "{batch:?}");
        assert_no_drift(&state, "after bulk_skill_apply");
        assert_external_edit_detected_at(
            &state,
            &target.join("SKILL.md"),
            &target,
            b"---\nname: demo-skill\n---\n\nExternal body.\n",
        );
    }

    #[test]
    fn editing_a_linked_skill_refreshes_the_drift_baseline() {
        let sandbox = Sandbox::new().expect("sandbox");
        let state = sandbox.state();
        let target = sandbox.agent_skills_dir().join(sandbox.slug.as_str());
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("enable preview");
        skill_agent_apply_impl(&state, &preview.preview_id).expect("enable apply");
        assert_no_drift(&state, "after enabling the linked skill");

        let expected_hash = at_store::manifest::hash_skill_folder(&sandbox.source_dir())
            .expect("package hash")
            .to_string();
        let content = "---\nname: demo-skill\ndescription: demo\n---\n\nEdited body.\n";
        let preview_id = at_fs::sha256_hex(format!("{expected_hash}\0{content}").as_bytes());
        skill_edit_apply_inner(
            &state,
            &sandbox.item_id.to_string(),
            content,
            &expected_hash,
            &preview_id,
        )
        .expect("edit apply");
        assert_no_drift(&state, "after skill_edit_apply");
        assert_external_edit_detected_at(
            &state,
            &target.join("SKILL.md"),
            &target,
            b"---\nname: demo-skill\n---\n\nExternal body.\n",
        );
    }

    #[test]
    fn the_cli_and_app_planners_emit_the_same_change_set_for_one_intent() {
        // TU-034: the CLI and the app assemble the same intent and hand it to the same engine
        // planner. Identity fields are random per call, so they are normalized; every other byte
        // of the serialized ChangeSet must match exactly.
        let sandbox = Sandbox::new().expect("sandbox");
        let preview = skill_agent_preview_impl(
            sandbox.database.connection(),
            &sandbox.paths,
            sandbox.request(true),
        )
        .expect("app preview");
        let app_set = {
            let plans = plans().lock().expect("plan cache");
            plans
                .get(&preview.preview_id)
                .and_then(|cached| cached.set.clone())
                .expect("the app preview caches its ChangeSet")
        };

        let store = StoreLayout::for_scope(&sandbox.paths, Scope::User).expect("user layout");
        let cli_plan = at_engine::skill_cli::plan_toggle(
            sandbox.database.connection(),
            &at_engine::skill_cli::ToggleRequest {
                paths: &sandbox.paths,
                store: &store,
                scope: Scope::User,
                project_id: None,
                project_root: None,
                item_id: sandbox.item_id,
                item_name: "demo-skill".to_owned(),
                slug: sandbox.slug.clone(),
                agent: AgentId::ClaudeCode,
                enabled: true,
            },
        )
        .expect("CLI toggle plan");
        let cli_set = cli_plan
            .change_set
            .expect("the CLI plan carries a ChangeSet");

        let normalized = |set: ChangeSet| {
            let mut value = serde_json::to_value(set).expect("serialize ChangeSet");
            value["id"] = serde_json::Value::String("normalized".to_owned());
            value["preview_id"] = serde_json::Value::String("normalized".to_owned());
            value["created_at_unix_seconds"] = serde_json::Value::from(0);
            serde_json::to_vec(&value).expect("canonical bytes")
        };
        assert_eq!(
            normalized(app_set),
            normalized(cli_set),
            "CLI --dry-run and the app planner must serialize byte-identical ChangeSets"
        );
    }
}
