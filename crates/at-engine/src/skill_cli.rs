//! CLI-facing composition for skill installation and per-agent toggles.
//!
//! The desktop app already assembles these facts inside its Tauri command layer. A command
//! line transport cannot reach that layer, so the same planning inputs are assembled here
//! and handed to the unchanged engine planners (`deploy::plan`,
//! `skill_agent::plan_skill_agent_change`, `at_fs::apply_with_roots`). Both transports
//! therefore review and apply the identical ChangeSets.

use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::agents::AgentPresence;
use at_model::{
    AgentId, ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, ItemState, LinkMode,
    OperationKind, Scope,
};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::ManifestItem;
use at_store::paths::AppPaths;
use thiserror::Error;

/// Failure while assembling or applying a CLI skill operation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillCliError {
    /// Agent detection says the selected agent is unavailable.
    #[error("agent `{agent}` is not installed")]
    AgentNotInstalled {
        /// Requested agent.
        agent: AgentId,
    },
    /// The agent is installed but AgenTuna does not manage it.
    #[error("agent `{agent}` is not managed by AgenTuna")]
    AgentUnmanaged {
        /// Requested agent.
        agent: AgentId,
    },
    /// A native folder occupies the deployment target.
    #[error("native conflict at {path}")]
    NativeConflict {
        /// Occupied path.
        path: PathBuf,
    },
    /// A Global lock refuses the change.
    #[error("skill is locked: {reason}")]
    Locked {
        /// Recorded lock reason.
        reason: String,
    },
    /// The canonical package is missing or changed.
    #[error("the canonical skill package is unavailable or changed: {path}")]
    Package {
        /// Canonical package path.
        path: PathBuf,
    },
    /// The recorded deployment has disappeared from disk.
    #[error("the recorded deployment is missing: {path}")]
    MissingDeployment {
        /// Missing deployment path.
        path: PathBuf,
    },
    /// Database access failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// The deployment planner refused the request.
    #[error(transparent)]
    Deployment(#[from] crate::deploy::DeployPlanError),
    /// The skill-agent planner refused the request.
    #[error(transparent)]
    Agent(#[from] crate::skill_agent::SkillAgentPlanError),
    /// Invalid ChangeSet metadata.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// The authorized-root write pipeline failed and rolled back.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// The store paths could not be used.
    #[error("the skill store is unavailable: {0}")]
    Store(String),
}

/// One manifest item that must be visible in the index before a reviewed operation.
pub struct ManifestItemInput<'a> {
    /// Scope owning the store.
    pub scope: Scope,
    /// Registered project for Project and Local scopes.
    pub project_id: Option<&'a str>,
    /// Manifest record that carries the stable identity and content hash.
    pub item: &'a ManifestItem,
}

/// Creates or refreshes the indexed identity for one manifest item.
///
/// The manifest owns the stable id; this function makes the derived row agree with it
/// without scanning the disk, which a command-line invocation cannot wait for. The item kind
/// comes from the manifest record, so skills, rules and MCP servers share one implementation:
/// a command that changes a store file must call this before it plans a deployment, or the
/// deployment insert fails the `items` foreign key and the command reports a failure after it
/// has already changed the native file.
///
/// # Errors
/// Returns the SQLite failure when the row cannot be written.
pub fn sync_manifest_item(
    connection: &rusqlite::Connection,
    input: &ManifestItemInput<'_>,
) -> Result<ItemId, at_db::DbError> {
    let spec = at_db::items::ManifestItemSpec {
        id: input.item.id,
        kind: input.item.kind,
        scope: input.scope,
        project_id: input.project_id,
        slug: input.item.slug.as_str(),
        name: input
            .item
            .name
            .as_deref()
            .unwrap_or(input.item.slug.as_str()),
        source_path: input.item.path.as_str(),
        content_hash: input.item.hash.as_str(),
    };
    at_db::items::sync_manifest_item(connection, &spec)
}

/// Inputs for reviewing one per-agent skill toggle.
pub struct ToggleRequest<'a> {
    /// Resolved application roots.
    pub paths: &'a AppPaths,
    /// Canonical store that owns the skill.
    pub store: &'a StoreLayout,
    /// Scope of the canonical skill.
    pub scope: Scope,
    /// Registered project for Project and Local scopes.
    pub project_id: Option<String>,
    /// Project directory used to resolve agent Project paths.
    pub project_root: Option<PathBuf>,
    /// Stable skill identity.
    pub item_id: ItemId,
    /// Display name used in the operation summary.
    pub item_name: String,
    /// Validated canonical slug.
    pub slug: Slug,
    /// Agent whose deployment changes.
    pub agent: AgentId,
    /// Desired state.
    pub enabled: bool,
}

/// Reviewed per-agent toggle, including the exact ChangeSet to apply.
#[derive(Debug, Clone)]
pub struct TogglePlan {
    /// Stable skill identity.
    pub item_id: ItemId,
    /// Agent whose deployment changes.
    pub agent: AgentId,
    /// Scope of the canonical skill.
    pub scope: Scope,
    /// Registered project for Project and Local scopes.
    pub project_id: Option<String>,
    /// Filesystem operations; `None` when the requested state already holds.
    pub change_set: Option<ChangeSet>,
    /// Store root used to resolve relative operations.
    pub store_root: PathBuf,
    /// Backup root handed to the write pipeline.
    pub backup_root: PathBuf,
    /// Authorized native roots for this operation.
    pub authorized_roots: Vec<PathBuf>,
    /// Canonical package that must still hash to [`Self::source_hash`].
    pub source: PathBuf,
    /// Canonical package hash captured during planning.
    pub source_hash: String,
    /// Native target path.
    pub target: PathBuf,
    /// Materialization mode actually planned.
    pub mode: Option<LinkMode>,
    /// Requested final state.
    pub enabled: bool,
    /// Whether the requested state was already present.
    pub no_op: bool,
    /// Other agents that read the same native root.
    pub coupled_agents: Vec<AgentId>,
    /// Human-readable warnings, including shared-root coupling.
    pub warnings: Vec<String>,
    /// Paths the reviewed operation will change.
    pub paths: Vec<PathBuf>,
}

/// Result of applying a reviewed toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleOutcome {
    /// Operation recorded by the write pipeline; absent for a no-op.
    pub operation_id: Option<String>,
    /// Whether any file or database row changed.
    pub changed: bool,
    /// Final state.
    pub enabled: bool,
}

/// Builds the reviewed toggle without writing anything.
///
/// # Errors
/// Returns a typed missing-agent, lock, native-conflict, package or planner failure.
pub fn plan_toggle(
    connection: &rusqlite::Connection,
    request: &ToggleRequest<'_>,
) -> Result<TogglePlan, SkillCliError> {
    let store_root = request.store.root().to_path_buf();
    let source = request.store.skill_dir(&request.slug);
    if !source.is_dir() {
        return Err(SkillCliError::Package { path: source });
    }
    let source_hash = at_store::manifest::hash_skill_folder(&source)
        .map_err(|_| SkillCliError::Package {
            path: source.clone(),
        })?
        .to_string();
    let home = request
        .paths
        .home_dir()
        .ok_or_else(|| SkillCliError::Store("the home directory is unavailable".to_owned()))?
        .to_path_buf();
    let env = AgentEnvironment {
        home: home.clone(),
        project: request.project_root.clone(),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let adapter = registry
        .get(&request.agent)
        .map_err(|_| SkillCliError::Store("the agent registry is unavailable".to_owned()))?
        .ok_or(SkillCliError::AgentNotInstalled {
            agent: request.agent.clone(),
        })?;
    let candidate = adapter
        .paths(ItemKind::Skill, request.scope, &env)
        .into_iter()
        .find(|candidate| candidate.primary)
        .ok_or_else(|| {
            SkillCliError::Store("the agent has no skill directory for this scope".to_owned())
        })?;
    let target = candidate.path.join(request.slug.as_str());

    let agents = at_db::agents::list_agents_on_mac(connection)?;
    let agent_row = agents.iter().find(|row| row.id == request.agent).ok_or(
        SkillCliError::AgentNotInstalled {
            agent: request.agent.clone(),
        },
    )?;
    let installed = agent_row.presence == AgentPresence::Installed;
    let managed = at_db::agents::is_agent_managed(connection, &request.agent)?;
    let deployment = at_db::deployments::find_deployment(
        connection,
        &request.item_id,
        &request.agent,
        request.scope,
        request.project_id.as_deref(),
    )?;
    let already_enabled = deployment
        .as_ref()
        .is_some_and(|cell| cell.state == ItemState::Linked);
    let destination_managed = deployment.as_ref().is_some_and(|cell| {
        cell.state == ItemState::Linked
            && cell.target_path.as_deref() == Some(target.to_string_lossy().as_ref())
    });
    let lock_reason = at_db::locks::find_lock(connection, &request.item_id, Scope::Global)?
        .and_then(|lock| lock.reason);
    let settings = at_db::settings::load(connection)
        .map_err(|_| SkillCliError::Store("deployment settings are unavailable".to_owned()))?;
    let mode = settings.link_mode(&request.agent);
    let supports_symlink = adapter.capabilities(&env).supports_symlink;
    let mut shared_agents = Vec::new();
    for other in &agents {
        let Some(other_adapter) = registry
            .get(&other.id)
            .map_err(|_| SkillCliError::Store("the agent registry is unavailable".to_owned()))?
        else {
            continue;
        };
        if other_adapter
            .paths(ItemKind::Skill, request.scope, &env)
            .iter()
            .any(|path| path.primary && path.path == candidate.path)
        {
            shared_agents.push(other.id.clone());
        }
    }
    let existing_hash = if request.enabled {
        None
    } else {
        at_fs::hash_node(&target).map_err(|_| SkillCliError::NativeConflict {
            path: target.clone(),
        })?
    };
    if !request.enabled && already_enabled && existing_hash.is_none() {
        return Err(SkillCliError::MissingDeployment { path: target });
    }
    let disable_change_set = match existing_hash {
        Some(hash) => Some(ChangeSet {
            id: uuid::Uuid::now_v7().to_string(),
            preview_id: uuid::Uuid::now_v7().to_string(),
            operation_kind: OperationKind::parse("deploy")?,
            scope: Some(request.scope),
            project_id: request.project_id.clone(),
            summary: format!("Disable skill {} for {}", request.item_name, request.agent),
            ops: vec![FileOp::Delete {
                path: target.clone(),
                expected: ExpectedState::hash(hash)?,
            }],
            created_at_unix_seconds: 0,
        }),
        None => None,
    };
    let deployment_request = crate::deploy::PlanRequest {
        item_id: request.item_id,
        scope: request.scope,
        agent: request.agent.clone(),
        store_root: store_root.clone(),
        source: source.clone(),
        destination: target.clone(),
        mode,
        link_supported: supports_symlink,
        managed,
        destination_managed,
        shared_agents,
        project_id: request.project_id.clone(),
        summary: format!("Set skill {} for {}", request.item_name, request.agent),
    };
    let planned =
        crate::skill_agent::plan_skill_agent_change(crate::skill_agent::SkillAgentPlanRequest {
            intent: crate::skill_agent::SetSkillAgentEnabled {
                item_id: request.item_id,
                scope: request.scope,
                agent: request.agent.clone(),
                enabled: request.enabled,
            },
            deployment: deployment_request,
            agent_installed: installed,
            lock_reason,
            already_enabled,
            disable_change_set,
        })
        .map_err(|error| match error {
            crate::skill_agent::SkillAgentPlanError::NativeConflict { path } => {
                SkillCliError::NativeConflict { path }
            }
            crate::skill_agent::SkillAgentPlanError::AgentUnmanaged { agent } => {
                SkillCliError::AgentUnmanaged { agent }
            }
            crate::skill_agent::SkillAgentPlanError::Locked { reason, .. } => {
                SkillCliError::Locked { reason }
            }
            other => SkillCliError::Agent(other),
        })?;
    let paths = planned.change_set.as_ref().map_or_else(Vec::new, |set| {
        set.ops
            .iter()
            .filter_map(|op| op.destination().map(Path::to_path_buf))
            .collect()
    });
    let warnings = planned
        .warnings
        .iter()
        .map(|warning| format!("{warning:?}"))
        .collect();
    let mut authorized_roots = vec![home.clone()];
    if let Some(project_root) = &request.project_root
        && !project_root.starts_with(&home)
    {
        authorized_roots.push(project_root.clone());
    }
    if !candidate.path.starts_with(&home)
        && request
            .project_root
            .as_ref()
            .is_none_or(|root| !candidate.path.starts_with(root))
    {
        let root = existing_authorized_ancestor(&candidate.path);
        if !authorized_roots.contains(&root) {
            authorized_roots.push(root);
        }
    }
    Ok(TogglePlan {
        item_id: request.item_id,
        agent: request.agent.clone(),
        scope: request.scope,
        project_id: request.project_id.clone(),
        change_set: planned.change_set,
        store_root,
        backup_root: request.paths.backup_dir().to_path_buf(),
        authorized_roots,
        source,
        source_hash,
        target,
        mode: planned.mode_used,
        enabled: request.enabled,
        no_op: planned.no_op,
        coupled_agents: planned.coupled_agents,
        warnings,
        paths,
    })
}

/// Applies the reviewed toggle and records the deployment row.
///
/// # Errors
/// Returns a conflict when the package changed after review, or a typed apply/database error.
pub fn apply_toggle(
    connection: &rusqlite::Connection,
    plan: &TogglePlan,
) -> Result<ToggleOutcome, SkillCliError> {
    if plan.no_op {
        return Ok(ToggleOutcome {
            operation_id: None,
            changed: false,
            enabled: plan.enabled,
        });
    }
    let current_source_hash = at_store::manifest::hash_skill_folder(&plan.source)
        .map_err(|_| SkillCliError::Package {
            path: plan.source.clone(),
        })?
        .to_string();
    if current_source_hash != plan.source_hash {
        return Err(SkillCliError::Package {
            path: plan.source.clone(),
        });
    }
    let set = plan
        .change_set
        .as_ref()
        .ok_or_else(|| SkillCliError::Store("the reviewed skill plan is missing".to_owned()))?;
    let roots = at_fs::ApplyRoots::new(plan.authorized_roots.clone())?;
    let mut recorder =
        at_fs::DbOperationRecorder::new_with_actor(connection, 30, at_db::ops::OperationActor::Cli);
    let mut context = at_fs::ApplyContext {
        store_root: &plan.store_root,
        backup_folder: &plan.backup_root,
        recorder: &mut recorder,
    };
    let outcome = at_fs::apply_with_roots(set, &mut context, &roots)?;
    let store_hash = at_store::manifest::hash_skill_folder(&plan.source)
        .map_err(|_| SkillCliError::Package {
            path: plan.source.clone(),
        })?
        .to_string();
    let stored_mode = if plan.enabled {
        Some(match plan.mode.unwrap_or(LinkMode::Link) {
            LinkMode::Link => "symlink",
            LinkMode::Copy => "copy",
        })
    } else {
        None
    };
    let target_text = plan.target.to_string_lossy().into_owned();
    let source_text = plan.source.to_string_lossy().into_owned();
    let target_hash = if plan.enabled {
        Some(
            at_fs::hash_node(&plan.target)
                .map_err(|_| SkillCliError::MissingDeployment {
                    path: plan.target.clone(),
                })?
                .ok_or_else(|| SkillCliError::MissingDeployment {
                    path: plan.target.clone(),
                })?,
        )
    } else {
        None
    };
    let state_value = if plan.enabled {
        ItemState::Linked
    } else {
        ItemState::Off
    };
    let hashes = at_db::deployments::DeploymentHashes::new(
        Some(&store_hash),
        target_hash.as_deref(),
        plan.enabled.then_some(store_hash.as_str()),
    );
    let mut spec = at_db::deployments::DeploymentSpec::applied(
        &plan.item_id,
        &plan.agent,
        plan.scope,
        state_value,
        Some(stored_mode.unwrap_or("none")),
        Some(&target_text),
        hashes,
    )
    .with_operation(&outcome.operation_id);
    if plan.enabled && plan.mode == Some(LinkMode::Link) {
        spec = spec.with_link_target(&source_text);
    }
    let spec = match plan.project_id.as_deref() {
        Some(project_id) => spec.with_project(project_id),
        None => spec,
    };
    at_db::deployments::upsert_deployment(connection, &spec)?;
    Ok(ToggleOutcome {
        operation_id: Some(outcome.operation_id),
        changed: true,
        enabled: plan.enabled,
    })
}

/// Finds the nearest existing directory at or above `path`.
fn existing_authorized_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.is_dir() {
            return current.to_path_buf();
        }
        let Some(parent) = current.parent() else {
            return current.to_path_buf();
        };
        current = parent;
    }
}
