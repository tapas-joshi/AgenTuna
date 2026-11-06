//! Reviewed preset apply and unapply over the engine's member planners.
//!
//! Each preset member is planned by the kind-specific engine planner (skill deployment or MCP
//! projection) and merged into one preset `ChangeSet` by
//! [`at_engine::presets::PresetService::plan_apply`]. The merged set is applied exactly once
//! through `at-fs` with authorized roots, so one preset application is one operation with one
//! backup set and one history row.
//!
//! Every row the member planners need is read before the service takes its write borrow, so the
//! planners work from one consistent snapshot instead of issuing per-member queries mid-plan.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{
    AgentId, ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, ItemState, LinkMode, McpServerDef,
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

/// Request to apply a preset against one agent or the active project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetApplyRequest {
    /// Preset to apply.
    pub preset_id: String,
    /// `agent` or `project`.
    pub target_kind: String,
    /// Agent id, or the registered project id for a project target.
    pub target_id: String,
    /// Registered project providing native Project paths.
    pub project_id: Option<String>,
}

/// Outcome of one applied or unapplied preset operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetApplyResultView {
    /// Operation id recorded by the safe write pipeline; absent for a no-op.
    pub operation_id: Option<String>,
    /// Whether any file or database row changed.
    pub changed: bool,
    /// Members this operation changed.
    pub changed_members: i64,
    /// Enabled members after the operation.
    pub enabled: i64,
    /// Existing members after the operation.
    pub total: i64,
    /// Whether every existing member is enabled afterwards.
    pub complete: bool,
    /// Non-fatal notices, such as a member whose item was deleted.
    pub warnings: Vec<String>,
}

struct TargetContext {
    paths: at_store::paths::AppPaths,
    env: AgentEnvironment,
    registry: AgentAdapterRegistry,
    agents: Vec<at_db::agents::AgentSummary>,
    /// Agents a member is planned for: one for an agent target, every managed agent for a project.
    selected_agents: Vec<AgentId>,
    /// Native scope receiving the deployment.
    native_scope: Scope,
    /// Registered project id, when the target is a project.
    project_id: Option<String>,
    /// Deployment mode per agent.
    modes: BTreeMap<AgentId, LinkMode>,
}

impl TargetContext {
    fn link_supported(&self, agent: &AgentId) -> bool {
        self.registry
            .get(agent)
            .ok()
            .flatten()
            .is_none_or(|adapter| adapter.capabilities(&self.env).supports_symlink)
    }

    /// Primary native candidates one agent has for `kind` at the target's native scope.
    fn candidate_paths(&self, agent: &AgentId, kind: ItemKind) -> Vec<PathBuf> {
        let Ok(Some(adapter)) = self.registry.get(agent) else {
            return Vec::new();
        };
        adapter
            .paths(kind, self.native_scope, &self.env)
            .into_iter()
            .filter(|candidate| candidate.primary)
            .map(|candidate| candidate.path)
            .collect()
    }

    /// Agents that read one native root for `kind`, used for the shared-root warning.
    fn shared_agents(&self, kind: ItemKind, root: &Path) -> Vec<AgentId> {
        self.agents
            .iter()
            .filter(|agent| {
                self.candidate_paths(&agent.id, kind)
                    .iter()
                    .any(|candidate| candidate == root)
            })
            .map(|agent| agent.id.clone())
            .collect()
    }
}

/// One skill destination prepared before planning.
struct SkillTarget {
    agent: AgentId,
    destination: PathBuf,
    destination_managed: bool,
    mode: LinkMode,
    link_supported: bool,
    shared_agents: Vec<AgentId>,
}

/// One native MCP document prepared before planning.
struct McpTargetFact {
    agent: AgentId,
    path: PathBuf,
}

/// Everything a member planner needs, read once from one consistent database snapshot.
struct MemberFacts {
    item: at_db::shared::ItemRow,
    layout: StoreLayout,
    skill_source: PathBuf,
    skill_targets: Vec<SkillTarget>,
    definition: Option<McpServerDef>,
    mcp_targets: Vec<McpTargetFact>,
}

fn build_context(
    state: &AppState,
    target_kind: &str,
    target_id: &str,
    project_id: Option<&str>,
) -> Result<(at_engine::presets::Target, TargetContext), CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let project_root = match project_id {
        Some(id) => Some(PathBuf::from(
            at_db::projects::find_project_by_id(database.connection(), id)
                .map_err(|_| CommandError::internal("could not read the selected project"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?
                .path,
        )),
        None => None,
    };
    let (target, native_scope) = match target_kind {
        "agent" => (
            at_engine::presets::Target::Agent(
                target_id
                    .parse::<AgentId>()
                    .map_err(|_| CommandError::invalid_argument("target_id", "unknown agent"))?,
            ),
            Scope::User,
        ),
        "project" => {
            if project_root.is_none() {
                return Err(CommandError::invalid_argument(
                    "project_id",
                    "a project target needs a registered project",
                ));
            }
            (
                at_engine::presets::Target::Repo(target_id.to_owned()),
                Scope::Project,
            )
        }
        _ => {
            return Err(CommandError::invalid_argument(
                "target_kind",
                "expected agent or project",
            ));
        }
    };
    let paths = resolve_process_paths(project_root.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let env = AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: project_root,
        variables: BTreeMap::new(),
    };
    let registry = AgentAdapterRegistry::register();
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not read detected agents"))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not read deployment settings"))?;
    let installed = agents
        .iter()
        .filter(|agent| agent.presence != at_db::agents::AgentPresence::NotInstalled)
        .map(|agent| agent.id.clone())
        .collect::<BTreeSet<_>>();
    let managed = agents
        .iter()
        .filter(|agent| agent.managed)
        .map(|agent| agent.id.clone())
        .collect::<BTreeSet<_>>();
    let selected_agents: Vec<AgentId> = match &target {
        at_engine::presets::Target::Agent(agent) => {
            if !installed.contains(agent) {
                return Err(CommandError::invalid_argument(
                    "target_id",
                    "the agent is not installed",
                ));
            }
            if !managed.contains(agent) {
                return Err(CommandError::invalid_argument(
                    "target_id",
                    "AgenTuna does not manage this agent",
                ));
            }
            vec![agent.clone()]
        }
        at_engine::presets::Target::Repo(_) => installed.intersection(&managed).cloned().collect(),
        _ => {
            return Err(CommandError::invalid_argument(
                "target_kind",
                "unsupported preset target",
            ));
        }
    };
    let modes = agents
        .iter()
        .map(|agent| (agent.id.clone(), settings.link_mode(&agent.id)))
        .collect();
    Ok((
        target,
        TargetContext {
            paths,
            env,
            registry,
            agents,
            selected_agents,
            native_scope,
            project_id: project_id.map(ToOwned::to_owned),
            modes,
        },
    ))
}

/// Reads every row the member planners need for one preset.
fn prefetch_members(
    connection: &rusqlite::Connection,
    ctx: &TargetContext,
    members: &[at_db::presets::PresetMemberRecord],
) -> Result<BTreeMap<String, MemberFacts>, CommandError> {
    let mut ids = Vec::new();
    for member in members {
        let Ok(id) = member.item_id.parse::<ItemId>() else {
            continue;
        };
        ids.push(id);
    }
    let items = at_db::shared::items_by_ids(connection, &ids)
        .map_err(|_| CommandError::internal("could not read preset members"))?;
    let mut facts = BTreeMap::new();
    for item in items {
        let layout = StoreLayout::for_scope(&ctx.paths, item.scope)
            .map_err(|_| CommandError::unavailable("a preset member store is unavailable"))?;
        let mut skill_targets = Vec::new();
        let mut mcp_targets = Vec::new();
        if item.kind == ItemKind::Skill
            && let Ok(slug) = Slug::parse(&item.slug)
        {
            {
                for agent in &ctx.selected_agents {
                    let Some(root) = ctx
                        .candidate_paths(agent, ItemKind::Skill)
                        .into_iter()
                        .next()
                    else {
                        continue;
                    };
                    let destination = root.join(slug.as_str());
                    let deployment = at_db::deployments::find_deployment(
                        connection,
                        &item.id,
                        agent,
                        item.scope,
                        ctx.project_id.as_deref(),
                    )
                    .map_err(|_| CommandError::internal("could not read a skill deployment"))?;
                    let destination_managed = deployment.as_ref().is_some_and(|cell| {
                        cell.state == ItemState::Linked
                            && cell.target_path.as_deref()
                                == Some(destination.to_string_lossy().as_ref())
                    });
                    skill_targets.push(SkillTarget {
                        agent: agent.clone(),
                        shared_agents: ctx.shared_agents(ItemKind::Skill, &root),
                        destination,
                        destination_managed,
                        mode: ctx.modes.get(agent).copied().unwrap_or(LinkMode::Link),
                        link_supported: ctx.link_supported(agent),
                    });
                }
            }
        }
        if item.kind == ItemKind::McpServer {
            for agent in &ctx.selected_agents {
                if let Some(path) = ctx
                    .candidate_paths(agent, ItemKind::McpServer)
                    .into_iter()
                    .next()
                {
                    mcp_targets.push(McpTargetFact {
                        agent: agent.clone(),
                        path,
                    });
                }
            }
        }
        let definition = if item.kind == ItemKind::McpServer {
            at_store::mcp_store::McpStore::load(&layout)
                .ok()
                .and_then(|store| store.store.get(&item.slug).cloned())
        } else {
            None
        };
        let skill_source = Slug::parse(&item.slug).map_or_else(
            |_| layout.root().to_path_buf(),
            |slug| layout.skill_dir(&slug),
        );
        facts.insert(
            item.id.to_string(),
            MemberFacts {
                skill_source,
                layout,
                item,
                skill_targets,
                definition,
                mcp_targets,
            },
        );
    }
    Ok(facts)
}

/// Current native bytes for `path`: planned overlay bytes first, then the file on disk.
fn native_bytes(overlay: &at_engine::presets::NativeFileOverlay, path: &Path) -> Option<Vec<u8>> {
    if let Some(bytes) = overlay.get(path) {
        return Some(bytes.to_vec());
    }
    std::fs::read(path).ok()
}

fn push_ops(target: &mut Option<ChangeSet>, set: ChangeSet) {
    *target = Some(match target.take() {
        Some(mut existing) => {
            existing.ops.extend(set.ops);
            existing
        }
        None => set,
    });
}

fn plan_skill(ctx: &TargetContext, facts: &MemberFacts) -> Result<ChangeSet, String> {
    if !facts.skill_source.is_dir() {
        return Err(format!(
            "the canonical skill package is missing at {}",
            facts.skill_source.display()
        ));
    }
    let mut combined: Option<ChangeSet> = None;
    for target in &facts.skill_targets {
        let planned = at_engine::deploy::plan(at_engine::deploy::PlanRequest {
            item_id: facts.item.id,
            scope: facts.item.scope,
            agent: target.agent.clone(),
            store_root: facts.layout.root().to_path_buf(),
            source: facts.skill_source.clone(),
            destination: target.destination.clone(),
            mode: target.mode,
            link_supported: target.link_supported,
            managed: true,
            destination_managed: target.destination_managed,
            shared_agents: target.shared_agents.clone(),
            project_id: ctx.project_id.clone(),
            summary: format!("Enable {} for {}", facts.item.name, target.agent),
        })
        .map_err(|error| error.to_string())?;
        if let Some(set) = planned.change_set {
            push_ops(&mut combined, set);
        }
    }
    combined
        .ok_or_else(|| String::from("the member has no writable native skill root for this target"))
}

fn plan_mcp(
    ctx: &TargetContext,
    facts: &MemberFacts,
    overlay: &mut at_engine::presets::NativeFileOverlay,
) -> Result<ChangeSet, String> {
    let definition = facts
        .definition
        .as_ref()
        .ok_or_else(|| String::from("the canonical MCP definition is missing"))?;
    let mut combined: Option<ChangeSet> = None;
    for target in &facts.mcp_targets {
        let bytes = native_bytes(overlay, &target.path);
        let source = bytes
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        let expected = match &bytes {
            Some(bytes) => ExpectedState::hash(at_fs::sha256_hex(bytes))
                .map_err(|_| String::from("the native MCP hash is invalid"))?,
            None => ExpectedState::absent(),
        };
        let plan_target = at_engine::mcp_plan::McpTarget {
            agent: target.agent.clone(),
            scope: ctx.native_scope,
            item_id: facts.item.id,
            slug: facts.item.slug.clone(),
            path: target.path.clone(),
            source,
            expected,
            unsupported_acknowledged: false,
            installed: true,
        };
        let intent = at_engine::mcp_plan::McpIntent::SetEnabledForAgent {
            agent: target.agent.clone(),
            enabled: true,
        };
        let planned = at_engine::mcp_plan::plan(at_engine::mcp_plan::McpPlanRequest {
            registry: &ctx.registry,
            env: &ctx.env,
            intent: &intent,
            targets: std::slice::from_ref(&plan_target),
            definition,
            scope: facts.item.scope,
            project_id: ctx.project_id.clone(),
            locked_reason: None,
            store_layout: Some(&facts.layout),
            target_store_layout: None,
        })
        .map_err(|error| error.to_string())?;
        let mut sets = planned.apply_order();
        if let Some(first) = sets.next() {
            let mut set = first.clone();
            for extra in sets {
                set.ops.extend(extra.ops.clone());
            }
            set.validate().map_err(|error| error.to_string())?;
            push_ops(&mut combined, set);
        }
    }
    combined
        .ok_or_else(|| String::from("no selected agent has a native MCP document for this target"))
}

/// Builds the inverse transition that removes what a previous apply installed.
fn inverse_member(
    ctx: &TargetContext,
    facts: &MemberFacts,
    prior_enabled: Option<bool>,
) -> Result<at_engine::presets::MemberTransition, String> {
    let mut transition = at_engine::presets::MemberTransition {
        item_id: facts.item.id.to_string(),
        prior_enabled,
        change_set: None,
    };
    if prior_enabled != Some(false) {
        return Ok(transition);
    }
    let mut combined: Option<ChangeSet> = None;
    match facts.item.kind {
        ItemKind::Skill => {
            for target in &facts.skill_targets {
                let Some(hash) =
                    at_fs::hash_node(&target.destination).map_err(|error| error.to_string())?
                else {
                    continue;
                };
                push_ops(
                    &mut combined,
                    ChangeSet {
                        id: uuid::Uuid::now_v7().to_string(),
                        preview_id: uuid::Uuid::now_v7().to_string(),
                        operation_kind: OperationKind::parse("deploy")
                            .map_err(|_| String::from("deployment history type is unavailable"))?,
                        scope: Some(facts.item.scope),
                        project_id: ctx.project_id.clone(),
                        summary: format!("Disable skill {} for {}", facts.item.name, target.agent),
                        ops: vec![FileOp::Delete {
                            path: target.destination.clone(),
                            expected: ExpectedState::hash(hash)
                                .map_err(|_| String::from("the deployment hash is invalid"))?,
                        }],
                        created_at_unix_seconds: 0,
                    },
                );
            }
        }
        ItemKind::McpServer => {
            let Some(definition) = facts.definition.as_ref() else {
                return Ok(transition);
            };
            for target in &facts.mcp_targets {
                let bytes = native_bytes(
                    &at_engine::presets::NativeFileOverlay::default(),
                    &target.path,
                );
                let source = bytes
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_default();
                let expected = match &bytes {
                    Some(bytes) => ExpectedState::hash(at_fs::sha256_hex(bytes))
                        .map_err(|_| String::from("the native MCP hash is invalid"))?,
                    None => ExpectedState::absent(),
                };
                let plan_target = at_engine::mcp_plan::McpTarget {
                    agent: target.agent.clone(),
                    scope: ctx.native_scope,
                    item_id: facts.item.id,
                    slug: facts.item.slug.clone(),
                    path: target.path.clone(),
                    source,
                    expected,
                    unsupported_acknowledged: false,
                    installed: true,
                };
                let intent = at_engine::mcp_plan::McpIntent::SetEnabledForAgent {
                    agent: target.agent.clone(),
                    enabled: false,
                };
                let planned = at_engine::mcp_plan::plan(at_engine::mcp_plan::McpPlanRequest {
                    registry: &ctx.registry,
                    env: &ctx.env,
                    intent: &intent,
                    targets: std::slice::from_ref(&plan_target),
                    definition,
                    scope: facts.item.scope,
                    project_id: ctx.project_id.clone(),
                    locked_reason: None,
                    store_layout: Some(&facts.layout),
                    target_store_layout: None,
                })
                .map_err(|error| error.to_string())?;
                let mut sets = planned.apply_order();
                if let Some(first) = sets.next() {
                    let mut set = first.clone();
                    for extra in sets {
                        set.ops.extend(extra.ops.clone());
                    }
                    push_ops(&mut combined, set);
                }
            }
        }
        _ => {}
    }
    transition.change_set = combined;
    Ok(transition)
}

fn apply_roots(ctx: &TargetContext) -> Result<at_fs::changeset::ApplyRoots, CommandError> {
    let mut roots = vec![ctx.paths.support_dir().to_path_buf(), ctx.env.home.clone()];
    if let Some(project) = &ctx.env.project
        && !project.starts_with(&ctx.env.home)
    {
        roots.push(project.clone());
    }
    at_fs::changeset::ApplyRoots::new(roots)
        .map_err(|_| CommandError::unavailable("authorized preset paths are unavailable"))
}

fn conflict_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(CommandErrorCode::Conflict, error.to_string())
}

fn result_view(
    plan: &at_engine::presets::PresetPlan,
    outcome: Option<&at_fs::changeset::ApplyOutcome>,
) -> PresetApplyResultView {
    let after = &plan.ratio_after;
    PresetApplyResultView {
        operation_id: outcome.map(|outcome| outcome.operation_id.clone()),
        changed: outcome.is_some_and(|outcome| outcome.completed > 0),
        changed_members: i64::try_from(
            plan.transitions
                .iter()
                .filter(|transition| transition.change_set.is_some())
                .count(),
        )
        .unwrap_or(i64::MAX),
        enabled: after.enabled,
        total: after.total,
        complete: after.complete,
        warnings: plan
            .deleted_members
            .iter()
            .map(|member| {
                format!(
                    "Deleted member {} was excluded from this preset operation.",
                    member.item_id
                )
            })
            .collect(),
    }
}

/// Applies a preset to one agent or the active project after planning every member.
///
/// # Errors
/// Returns a typed refusal when a member cannot be planned, when an agent is locked, or when the
/// safe write pipeline fails; no file is written in those cases.
#[tauri::command]
pub fn preset_apply(
    state: State<'_, AppState>,
    request: PresetApplyRequest,
) -> Result<PresetApplyResultView, CommandError> {
    let (target, ctx) = build_context(
        &state,
        &request.target_kind,
        &request.target_id,
        request.project_id.as_deref(),
    )?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let members = at_db::presets::preset_members(database.connection(), &request.preset_id)
        .map_err(|_| CommandError::invalid_argument("preset_id", "preset is unavailable"))?;
    let (target_kind, target_id) = match &target {
        at_engine::presets::Target::Agent(agent) => {
            (at_db::presets::PresetTargetKind::Agent, agent.to_string())
        }
        at_engine::presets::Target::Repo(id) => {
            (at_db::presets::PresetTargetKind::Project, id.clone())
        }
        _ => {
            return Err(CommandError::invalid_argument(
                "target_kind",
                "unsupported preset target",
            ));
        }
    };
    let ratio = at_db::presets::applied_ratio(
        database.connection(),
        &request.preset_id,
        target_kind,
        &target_id,
    )
    .map_err(|_| CommandError::invalid_argument("preset_id", "preset is unavailable"))?;
    let prior: BTreeMap<String, bool> = ratio
        .members
        .iter()
        .filter(|member| member.exists)
        .map(|member| (member.item_id.clone(), member.enabled))
        .collect();
    let facts = prefetch_members(database.connection(), &ctx, &members)?;
    let overlay = at_engine::presets::NativeFileOverlay::default();
    let plan = {
        let service = at_engine::presets::PresetService::new(&mut database);
        service
            .plan_apply_with(&request.preset_id, target, overlay, |member, overlay| {
                let prior_enabled = prior.get(&member.item_id).copied().unwrap_or(false);
                if prior_enabled {
                    return Ok(at_engine::presets::MemberTransition {
                        item_id: member.item_id.clone(),
                        prior_enabled: Some(true),
                        change_set: None,
                    });
                }
                let Some(member_facts) = facts.get(&member.item_id) else {
                    return Err(String::from("preset member no longer exists"));
                };
                let change_set = match member_facts.item.kind {
                    ItemKind::Skill => plan_skill(&ctx, member_facts)?,
                    ItemKind::McpServer => plan_mcp(&ctx, member_facts, overlay)?,
                    _ => {
                        return Err(String::from(
                            "presets can only apply skills and MCP servers",
                        ));
                    }
                };
                Ok(at_engine::presets::MemberTransition {
                    item_id: member.item_id.clone(),
                    prior_enabled: Some(false),
                    change_set: Some(change_set),
                })
            })
            .map_err(conflict_error)?
    };
    if plan.change_set.is_none() {
        return Ok(result_view(&plan, None));
    }
    let set = plan
        .change_set
        .as_ref()
        .ok_or_else(|| CommandError::internal("the preset plan lost its change set"))?;
    let roots = apply_roots(&ctx)?;
    let outcome = {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut apply_context = at_fs::changeset::ApplyContext {
            store_root: ctx.paths.support_dir(),
            backup_folder: ctx.paths.backup_dir(),
            recorder: &mut recorder,
        };
        at_fs::changeset::apply_with_roots(set, &mut apply_context, &roots)
            .map_err(conflict_error)?
    };
    let mut service = at_engine::presets::PresetService::new(&mut database);
    service
        .record_apply(&plan.preset_id, &plan.target, &outcome, &plan.transitions)
        .map_err(conflict_error)?;
    Ok(result_view(&plan, Some(&outcome)))
}

/// Reverts exactly the members one recorded application changed.
///
/// # Errors
/// Returns a typed refusal when the application is unknown, its target no longer matches, or the
/// safe write pipeline fails.
#[tauri::command]
pub fn preset_unapply(
    state: State<'_, AppState>,
    application_id: i64,
) -> Result<PresetApplyResultView, CommandError> {
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let application = at_db::presets::find_application(database.connection(), application_id)
        .map_err(|_| CommandError::internal("could not read the preset application"))?
        .ok_or_else(|| {
            CommandError::invalid_argument("application_id", "preset application is unavailable")
        })?;
    let project_id = (application.target_kind == at_db::presets::PresetTargetKind::Project)
        .then(|| application.target_id.clone());
    let (target, ctx) = build_context(
        &state,
        application.target_kind.as_str(),
        &application.target_id,
        project_id.as_deref(),
    )?;
    let history = at_db::presets::application_members(database.connection(), application_id)
        .map_err(|_| CommandError::internal("could not read the application history"))?;
    let members = at_db::presets::preset_members(database.connection(), &application.preset_id)
        .map_err(|_| CommandError::internal("could not read preset members"))?;
    let facts = prefetch_members(database.connection(), &ctx, &members)?;
    let inverses = history
        .iter()
        .map(|member| match facts.get(&member.item_id) {
            Some(member_facts) => inverse_member(&ctx, member_facts, member.prior_enabled),
            None => Ok(at_engine::presets::MemberTransition {
                item_id: member.item_id.clone(),
                prior_enabled: member.prior_enabled,
                change_set: None,
            }),
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(conflict_error)?;
    let plan = {
        let service = at_engine::presets::PresetService::new(&mut database);
        service
            .plan_unapply(application_id, target, inverses)
            .map_err(conflict_error)?
    };
    if plan.change_set.is_none() {
        return Ok(result_view(&plan, None));
    }
    let set = plan
        .change_set
        .as_ref()
        .ok_or_else(|| CommandError::internal("the preset plan lost its change set"))?;
    let roots = apply_roots(&ctx)?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut apply_context = at_fs::changeset::ApplyContext {
        store_root: ctx.paths.support_dir(),
        backup_folder: ctx.paths.backup_dir(),
        recorder: &mut recorder,
    };
    let outcome = at_fs::changeset::apply_with_roots(set, &mut apply_context, &roots)
        .map_err(conflict_error)?;
    Ok(result_view(&plan, Some(&outcome)))
}
