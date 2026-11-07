//! MCP deployment intents projected by registered agent adapters into reviewed ChangeSets.

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_adapters::mcp_capabilities::{
    capabilities, project_transport, require_unsupported_acknowledgement,
};
use at_adapters::secret_launch::{SecretInjection, plan_secret_injection};
use at_model::{
    AgentId, ChangeSet, EnvValue, ExpectedState, FileOp, ItemId, McpServerDef, OperationKind, Scope,
};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

/// Supported MCP planner actions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum McpIntent {
    /// Add a canonical server definition.
    Add {
        /// Canonical store slug.
        slug: String,
        /// MCP server definition.
        definition: McpServerDef,
    },
    /// Replace an existing server definition.
    Edit {
        /// Canonical store slug.
        slug: String,
        /// Updated MCP server definition.
        definition: McpServerDef,
    },
    /// Rename a server key.
    Rename {
        /// Existing slug.
        from: String,
        /// Requested slug.
        to: String,
    },
    /// Enable or disable one native agent projection.
    SetEnabledForAgent {
        /// Agent to change.
        agent: AgentId,
        /// Whether the server should be enabled.
        enabled: bool,
    },
    /// Add a project disable tombstone.
    DisableInRepo {
        /// Project root receiving the tombstone.
        project: PathBuf,
    },
    /// Promote a definition to a broader scope.
    Promote {
        /// Broader destination scope.
        target: Scope,
    },
    /// Move definition to another scope.
    MoveToScope {
        /// Destination scope.
        target: Scope,
    },
    /// Project the definition a copy-to-scope action places in a destination scope.
    ///
    /// The destination store itself is planned by [`plan_item`], because a copy reads two store
    /// documents and writes only the destination one; this intent carries only the native phase.
    CopyToScope {
        /// Destination store slug after the collision decision.
        slug: String,
        /// Definition copied by reference, never with resolved secret values.
        definition: McpServerDef,
    },
    /// Remove a managed server from its canonical scope and selected native projections.
    Remove,
}

/// One selected agent target for MCP projection.
#[derive(Debug, Clone)]
pub struct McpTarget {
    /// Selected agent.
    pub agent: AgentId,
    /// Effective native scope.
    pub scope: Scope,
    /// Definition identity in AgenTuna's manifest.
    pub item_id: ItemId,
    /// Definition slug.
    pub slug: String,
    /// Native MCP document path.
    pub path: PathBuf,
    /// Original native document contents; empty when the target is absent.
    pub source: String,
    /// Existing bytes hash, when the document exists.
    pub expected: ExpectedState,
    /// Whether adapter capability policy has been acknowledged.
    pub unsupported_acknowledged: bool,
    /// Whether the agent is installed.
    pub installed: bool,
}

/// Planner failures; refusals happen before producing any file operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpPlanError {
    /// The selected agent has not been detected.
    #[error("agent is not installed: {0}")]
    AgentNotInstalled(AgentId),
    /// Agent cannot represent one or more canonical fields.
    #[error("agent {agent} cannot represent this MCP definition: {reason}")]
    CapabilityRefusal {
        /// Agent whose capability policy denied the plan.
        agent: AgentId,
        /// Human-readable refusal reason.
        reason: String,
    },
    /// A locked definition cannot be changed.
    #[error("MCP item is locked: {0}")]
    Locked(String),
    /// The adapter could not project a native document.
    #[error(transparent)]
    Adapter(#[from] at_adapters::adapter::AgentError),
    /// Canonical MCP store could not be loaded or serialized.
    #[error(transparent)]
    Store(#[from] at_store::mcp_store::McpStoreError),
    /// Intent requires a canonical scope store.
    #[error("MCP intent requires a canonical store layout")]
    MissingStoreLayout,
    /// Invalid ChangeSet.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// A requested canonical MCP entry does not exist.
    #[error("canonical MCP entry not found: {0}")]
    MissingStoreEntry(String),
    /// Project manifest could not be loaded or validated.
    #[error(transparent)]
    Manifest(#[from] at_store::manifest::ManifestError),
    /// Tombstone violates manifest invariants.
    #[error(transparent)]
    ManifestField(#[from] at_store::manifest::FieldError),
    /// Store bytes could not be read to build a compensating ChangeSet.
    #[error("could not read MCP store path {path}: {source}")]
    RollbackRead {
        /// Store file that could not be read.
        path: PathBuf,
        /// Read failure.
        source: std::io::Error,
    },
    /// No inverse is defined for a non-write store operation.
    #[error("cannot compensate this MCP store operation")]
    RollbackUnsupported,
    /// Rename target is already used in the selected scope.
    #[error("MCP id already exists in this scope: {0}")]
    DuplicateId(String),
    /// The selected canonical item is not an MCP server.
    #[error("item is not an MCP server: {0}")]
    NotMcpServer(String),
    /// The selected native path exists but could not be read safely.
    #[error("native MCP config could not be read safely: {0}")]
    UnreadableTarget(String),
    /// A canonical store item cannot be recorded for this definition.
    #[error("invalid MCP store item: {0}")]
    InvalidItem(String),
    /// A copy must land in a different scope from its source.
    #[error("source and destination scopes must differ")]
    SameScope,
    /// Project and Local destinations need a registered project identity.
    #[error("copying to {scope} scope requires a registered project")]
    MissingProject {
        /// Destination scope that needs a project.
        scope: Scope,
    },
    /// Writing the Global store needs admin permission that is not available.
    #[error("permission denied: writing the Global MCP store requires admin permission")]
    PermissionDenied,
    /// The destination scope has no store to copy into.
    #[error("the destination {scope} scope has no store yet; create it first")]
    DestinationStoreMissing {
        /// Destination scope without an initialized store.
        scope: Scope,
    },
    /// The destination already holds a different definition that needs an explicit decision.
    #[error(
        "destination scope already holds an MCP server named `{slug}`; choose Replace or Rename"
    )]
    Collision {
        /// Existing destination slug.
        slug: String,
        /// Existing destination item identity.
        item_id: ItemId,
        /// Destination scope holding the existing definition.
        scope: Scope,
    },
    /// Replace was chosen but the destination holds no such definition.
    #[error("Replace was selected but the destination scope has no MCP server named `{0}`")]
    NoTargetToReplace(String),
    /// The destination `mcp.json` holds a definition the destination manifest does not own.
    #[error(
        "destination mcp.json defines `{slug}` without a store.json record; adopt or remove it first"
    )]
    UnmanagedStoreEntry {
        /// Destination slug found in the store document.
        slug: String,
    },
    /// A native agent document already holds an entry with this name outside AgenTuna.
    ///
    /// The caller offers the standard remedies: Adopt, Move aside or Cancel.
    #[error(
        "agent {agent} already defines `{slug}` outside AgenTuna: adopt it, move it aside or cancel"
    )]
    UnmanagedNative {
        /// Agent whose document already holds an entry with this name.
        agent: AgentId,
        /// Conflicting native server name.
        slug: String,
        /// Native document holding the unmanaged entry.
        path: PathBuf,
    },
    /// No destination agent loads MCP servers at the destination scope.
    #[error("no destination agent loads `{slug}` at {scope} scope")]
    NoNativeTarget {
        /// Slug being copied.
        slug: String,
        /// Destination scope.
        scope: Scope,
    },
    /// Every destination agent refused the definition, so no config that only looks valid is written.
    #[error("no destination agent can load this MCP server: {reason}")]
    NoSupportedAgent {
        /// Combined per-agent refusal reasons.
        reason: String,
    },
}

/// One row action over a canonical MCP server definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum McpItemAction {
    /// Enable or disable the definition for one agent in the definition's scope.
    SetEnabled {
        /// Agent whose native projection changes.
        agent: AgentId,
        /// Whether the server should be enabled for that agent.
        enabled: bool,
    },
    /// Record a project-scope tombstone so the server stops loading in one repository.
    DisableInRepo {
        /// Project root that receives the tombstone.
        project: PathBuf,
        /// Agents the tombstone covers; agents without a project path stay untouched.
        agents: Vec<AgentId>,
    },
    /// Copy the definition into another scope's store and project it for that scope's agents.
    CopyToScope {
        /// Destination scope.
        target_scope: Scope,
        /// Registered project identity, required when the destination is Project or Local.
        project_id: Option<String>,
        /// Explicit answer to a destination collision; absent until the caller decides.
        on_collision: Option<McpCopyResolution>,
    },
    /// Remove the canonical definition and its native projections.
    Delete,
}

/// Explicit user decision when the destination scope already holds a different definition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpCopyResolution {
    /// Replace the existing managed definition after backing it up.
    Replace,
    /// Copy under a different lowercase slug instead of replacing the existing definition.
    Rename(String),
}

/// One agent a copy plan refuses, with the reason the caller must show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAgentRefusal {
    /// Refused agent.
    pub agent: AgentId,
    /// Human-readable reason; never contains a secret value.
    pub reason: String,
}

/// Destination identity and per-agent outcome of a copy-to-scope plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCopySummary {
    /// Scope the definition was copied from.
    pub source_scope: Scope,
    /// Destination scope.
    pub target_scope: Scope,
    /// Destination store slug after the collision decision.
    pub target_slug: String,
    /// Destination item identity: a new one for a free destination, the existing one on Replace.
    pub target_item_id: ItemId,
    /// Whether an existing destination definition is replaced.
    pub replaced: bool,
    /// Agents the plan projects the definition to.
    pub agents: Vec<AgentId>,
    /// Destination agents refused with an explicit reason; they receive no configuration.
    pub refused_agents: Vec<McpAgentRefusal>,
}

/// Inputs for planning one row action against a canonical MCP server.
pub struct McpItemPlanRequest<'a> {
    /// Resolved AgenTuna roots.
    pub paths: &'a at_store::paths::AppPaths,
    /// Scope of the canonical definition.
    pub scope: Scope,
    /// Registered project identity, for project and local scopes.
    pub project_id: Option<String>,
    /// Canonical item identity.
    pub item_id: ItemId,
    /// Action to plan.
    pub action: McpItemAction,
    /// Agents detected in the index; an empty slice means "treat every registered adapter as installed".
    pub installed: &'a [AgentId],
    /// Agents with a deployment row in this scope, used to find native files on delete.
    pub deployed: &'a [AgentId],
    /// Lock reason blocking the action, when the item is locked.
    pub locked_reason: Option<&'a str>,
}

/// A planned row action plus the agents whose native documents change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpItemPlan {
    /// Canonical-store-then-native plan.
    pub plan: McpPlan,
    /// Canonical store slug of the item the action addresses.
    pub slug: String,
    /// Agents whose native documents the plan changes.
    pub agents: Vec<AgentId>,
    /// Requested agents skipped because they have no path at this scope or no native file.
    pub skipped_agents: Vec<AgentId>,
    /// Destination identity and refusals, present only for [`McpItemAction::CopyToScope`].
    pub copy: Option<McpCopySummary>,
}

/// Plans one enable, disable-in-repo or delete action for a canonical MCP server.
///
/// The caller resolves the item, passes the detected agents and any lock reason; the returned
/// plan carries both the canonical-store ChangeSet and the native ChangeSets, so a single reviewed
/// preview and one apply covers the whole action.
///
/// # Errors
/// Returns a typed failure when the item or definition is missing, an agent cannot represent the
/// definition, the item is locked, or no native target exists.
pub fn plan_item(request: McpItemPlanRequest<'_>) -> Result<McpItemPlan, McpPlanError> {
    let registry = AgentAdapterRegistry::register();
    let env = AgentEnvironment {
        home: request
            .paths
            .home_dir()
            .map_or_else(|| request.paths.user_store().to_path_buf(), PathBuf::from),
        project: request
            .paths
            .project_store()
            .and_then(std::path::Path::parent)
            .map(PathBuf::from),
        variables: std::env::var("PATH")
            .map(|path| std::collections::BTreeMap::from([(String::from("PATH"), path)]))
            .unwrap_or_default(),
    };
    let registry = &registry;
    let env = &env;
    let store_layout = at_store::layout::StoreLayout::for_scope(request.paths, request.scope)
        .map_err(|_| McpPlanError::MissingStoreLayout)?;
    let manifest = at_store::manifest::StoreManifest::load(&store_layout)?;
    let item = manifest
        .manifest
        .item(request.item_id)
        .ok_or_else(|| McpPlanError::MissingStoreEntry(request.item_id.to_string()))?;
    if item.kind != at_model::ItemKind::McpServer {
        return Err(McpPlanError::NotMcpServer(item.slug.to_string()));
    }
    let slug = item.slug.as_str().to_owned();
    let loaded = at_store::mcp_store::McpStore::load(&store_layout)?;
    let definition = loaded
        .store
        .get(slug.as_str())
        .ok_or_else(|| McpPlanError::MissingStoreEntry(slug.clone()))?
        .clone();
    match &request.action {
        McpItemAction::CopyToScope {
            target_scope,
            project_id,
            on_collision,
        } => plan_copy(McpCopyRequest {
            registry,
            env,
            paths: request.paths,
            source_scope: request.scope,
            source_slug: slug,
            definition,
            target_scope: *target_scope,
            target_project_id: project_id.clone(),
            on_collision: on_collision.clone(),
            installed: request.installed,
            locked_reason: request.locked_reason,
        }),
        action => plan_row_action(
            registry,
            env,
            &store_layout,
            slug,
            definition,
            RowActionRequest {
                scope: request.scope,
                project_id: request.project_id,
                item_id: request.item_id,
                action,
                installed: request.installed,
                deployed: request.deployed,
                locked_reason: request.locked_reason,
            },
        ),
    }
}

/// Inputs for enabling, disabling or deleting one resolved canonical MCP server.
struct RowActionRequest<'a> {
    scope: Scope,
    project_id: Option<String>,
    item_id: ItemId,
    action: &'a McpItemAction,
    installed: &'a [AgentId],
    deployed: &'a [AgentId],
    locked_reason: Option<&'a str>,
}

/// Plans an enable, disable-in-repo or delete action for one resolved canonical MCP server.
fn plan_row_action(
    registry: &AgentAdapterRegistry,
    env: &AgentEnvironment,
    store_layout: &at_store::layout::StoreLayout,
    slug: String,
    definition: McpServerDef,
    request: RowActionRequest<'_>,
) -> Result<McpItemPlan, McpPlanError> {
    let mut skipped_agents = Vec::new();
    let mut requested = match &request.action {
        McpItemAction::SetEnabled { agent, .. } => vec![agent.clone()],
        McpItemAction::DisableInRepo { agents, .. } => agents.clone(),
        // A copy resolves its own destination targets in `plan_copy` and never reaches this arm.
        McpItemAction::CopyToScope { .. } => Vec::new(),
        McpItemAction::Delete => {
            let mut agents = request.deployed.to_vec();
            if let Ok(adapters) = registry.adapters() {
                for adapter in adapters {
                    let agent = adapter.id();
                    if !agents.contains(&agent) {
                        agents.push(agent);
                    }
                }
            }
            agents
        }
    };
    requested.sort();
    requested.dedup();
    let mut targets = Vec::with_capacity(requested.len());
    for agent in requested {
        let Some(adapter) = registry.get(&agent)? else {
            skipped_agents.push(agent);
            continue;
        };
        let Some(candidate) = adapter
            .paths(at_model::ItemKind::McpServer, request.scope, env)
            .into_iter()
            .find(|candidate| candidate.primary)
        else {
            skipped_agents.push(agent);
            continue;
        };
        let exists = match std::fs::symlink_metadata(&candidate.path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                return Err(McpPlanError::UnreadableTarget(
                    candidate.path.display().to_string(),
                ));
            }
        };
        let enable = matches!(
            request.action,
            McpItemAction::SetEnabled { enabled: true, .. }
        );
        if !exists && !enable {
            // Nothing to remove from a file that does not exist.
            skipped_agents.push(agent);
            continue;
        }
        let bytes = if exists {
            read_native_document(&agent, &candidate.path)?
        } else {
            Vec::new()
        };
        // An agent file that does not exist yet starts from that agent's empty document, not
        // from zero bytes: Claude Code rejects an empty string as "map is not an object".
        let source = if exists {
            String::from_utf8(bytes.clone())
                .map_err(|_| McpPlanError::UnreadableTarget(candidate.path.display().to_string()))?
        } else {
            empty_native_document(&agent).to_owned()
        };
        let expected = if exists {
            ExpectedState::hash(at_fs::sha256_hex(&bytes))?
        } else {
            ExpectedState::absent()
        };
        targets.push(McpTarget {
            agent: agent.clone(),
            scope: request.scope,
            item_id: request.item_id,
            slug: slug.clone(),
            path: candidate.path,
            source,
            expected,
            unsupported_acknowledged: false,
            installed: request.installed.is_empty() || request.installed.contains(&agent),
        });
    }
    if targets.is_empty() {
        return Err(McpPlanError::MissingStoreEntry(format!(
            "{slug} has no native target in {} scope",
            request.scope.wire_name()
        )));
    }
    let intent = match &request.action {
        McpItemAction::SetEnabled { agent, enabled } => McpIntent::SetEnabledForAgent {
            agent: agent.clone(),
            enabled: *enabled,
        },
        McpItemAction::DisableInRepo { project, .. } => McpIntent::DisableInRepo {
            project: project.clone(),
        },
        // The copy planner builds this intent from the destination slug; this keeps the row
        // paths total for the same action type without sharing the copy's store phase.
        McpItemAction::CopyToScope { .. } => McpIntent::CopyToScope {
            slug: slug.clone(),
            definition: definition.clone(),
        },
        McpItemAction::Delete => McpIntent::Remove,
    };
    let agents = targets.iter().map(|target| target.agent.clone()).collect();
    let plan = plan(McpPlanRequest {
        registry,
        env,
        intent: &intent,
        targets: &targets,
        definition: &definition,
        scope: request.scope,
        project_id: request.project_id,
        locked_reason: request.locked_reason,
        store_layout: Some(store_layout),
        target_store_layout: None,
    })?;
    Ok(McpItemPlan {
        plan,
        slug,
        agents,
        skipped_agents,
        copy: None,
    })
}

/// Reads an agent's native document using that adapter's symlink decision.
fn read_native_document(agent: &AgentId, path: &Path) -> Result<Vec<u8>, McpPlanError> {
    let result = if at_adapters::adapter::follows_native_config_symlink(agent) {
        at_fs::symlink::read_following(path)
    } else {
        at_fs::symlink::read_nofollow(path)
    };
    result.map_err(|_| McpPlanError::UnreadableTarget(path.display().to_string()))
}
/// The smallest document each native adapter parses as "no servers yet".
fn empty_native_document(agent: &AgentId) -> &'static str {
    match agent {
        AgentId::ClaudeCode | AgentId::Omp => "{}\n",
        _ => "",
    }
}

/// Seconds since the Unix epoch, saturating to zero before it.
fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Everything a copy-to-scope plan needs after the source item is resolved.
struct McpCopyRequest<'a> {
    registry: &'a AgentAdapterRegistry,
    env: &'a AgentEnvironment,
    paths: &'a at_store::paths::AppPaths,
    source_scope: Scope,
    source_slug: String,
    definition: McpServerDef,
    target_scope: Scope,
    target_project_id: Option<String>,
    on_collision: Option<McpCopyResolution>,
    installed: &'a [AgentId],
    locked_reason: Option<&'a str>,
}

/// Destination identity a copy writes in the destination store.
struct CopyTarget {
    slug: String,
    item_id: ItemId,
    replaced: bool,
    existing_item: Option<at_store::manifest::ManifestItem>,
}

/// Plans a copy of one canonical definition into another scope.
///
/// The definition is copied verbatim — command, arguments, URL, headers and environment values,
/// including their secret references — so a Keychain value is never resolved into a file. The
/// copy is a new item in the destination store (a move keeps its identity; a copy does not).
fn plan_copy(request: McpCopyRequest<'_>) -> Result<McpItemPlan, McpPlanError> {
    let McpCopyRequest {
        registry,
        env,
        paths,
        source_scope,
        source_slug,
        definition,
        target_scope,
        target_project_id,
        on_collision,
        installed,
        locked_reason,
    } = request;
    if let Some(reason) = locked_reason {
        return Err(McpPlanError::Locked(reason.to_owned()));
    }
    if source_scope == target_scope {
        return Err(McpPlanError::SameScope);
    }
    if matches!(target_scope, Scope::Project | Scope::Local) && target_project_id.is_none() {
        return Err(McpPlanError::MissingProject {
            scope: target_scope,
        });
    }
    if target_scope == Scope::Global
        && !at_fs::privilege::GlobalStoreStatus::probe(paths.global_store()).is_available()
    {
        return Err(McpPlanError::PermissionDenied);
    }
    let scoped_project_id =
        target_project_id.filter(|_| matches!(target_scope, Scope::Project | Scope::Local));
    let target_layout =
        at_store::layout::StoreLayout::for_scope(paths, target_scope).map_err(|_| {
            McpPlanError::MissingProject {
                scope: target_scope,
            }
        })?;
    // A destination scope with no store yet is treated as empty: the caller merges the store
    // skeleton into the same reviewed ChangeSet, so the copy and its destination store land as
    // one operation instead of refusing the user with "create it first".
    let target_manifest = at_store::manifest::StoreManifest::load_for_plan(&target_layout)?;
    let target_store = at_store::mcp_store::McpStore::load(&target_layout)?;
    let target = resolve_copy_target(
        &source_slug,
        &definition,
        &target_store,
        &target_manifest,
        on_collision.as_ref(),
        target_scope,
    )?;
    let mut refused_agents = Vec::new();
    let mut skipped_agents = Vec::new();
    let mut requested = registry
        .adapters()?
        .iter()
        .map(|adapter| adapter.id())
        .collect::<Vec<_>>();
    requested.sort();
    requested.dedup();
    let mut targets = Vec::with_capacity(requested.len());
    for agent in requested {
        if !installed.is_empty() && !installed.contains(&agent) {
            skipped_agents.push(agent);
            continue;
        }
        let Some(adapter) = registry.get(&agent)? else {
            skipped_agents.push(agent);
            continue;
        };
        let Some(candidate) = adapter
            .paths(at_model::ItemKind::McpServer, target_scope, env)
            .into_iter()
            .find(|candidate| candidate.primary)
        else {
            skipped_agents.push(agent);
            continue;
        };
        let exists = match std::fs::symlink_metadata(&candidate.path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                return Err(McpPlanError::UnreadableTarget(
                    candidate.path.display().to_string(),
                ));
            }
        };
        let bytes = if exists {
            read_native_document(&agent, &candidate.path)?
        } else {
            Vec::new()
        };
        // A missing document starts from the adapter's empty document, exactly as the add path
        // does, so the native edit stays minimal instead of replacing the file wholesale.
        let source = if exists {
            String::from_utf8(bytes.clone())
                .map_err(|_| McpPlanError::UnreadableTarget(candidate.path.display().to_string()))?
        } else {
            empty_native_document(&agent).to_owned()
        };
        if !target.replaced && exists {
            // The destination store owns no definition with this name, so an entry already in the
            // agent's document belongs to someone else: never overwrite it silently.
            let present = at_adapters::mcp_projection::contains(
                &agent,
                &source,
                &candidate.path,
                target_scope,
                env.project.as_deref(),
                &target.slug,
            )
            .map_err(|error| McpPlanError::CapabilityRefusal {
                agent: agent.clone(),
                reason: error.to_string(),
            })?;
            if present {
                return Err(McpPlanError::UnmanagedNative {
                    agent,
                    slug: target.slug.clone(),
                    path: candidate.path,
                });
            }
        }
        let rendered = project_transport(&agent, &definition.transport);
        if let Err(error) = require_unsupported_acknowledgement(&rendered, false) {
            refused_agents.push(McpAgentRefusal {
                agent,
                reason: error.to_string(),
            });
            continue;
        }
        if let Some(reason) =
            secret_refusal(&agent, &candidate.path, &env.home, &rendered.transport)
        {
            refused_agents.push(McpAgentRefusal { agent, reason });
            continue;
        }
        // Probe the real projection before committing to this target: an agent with no native
        // MCP format simply has no document to write, and one whose format refuses the entry is
        // reported per agent instead of failing the whole copy.
        let mut probe_transport = rendered.transport.clone();
        let probe_env_vars = apply_secret_policy(
            &agent,
            &candidate.path,
            &env.home,
            &mut probe_transport,
            &mut Vec::new(),
        );
        if let Err(error) = at_adapters::mcp_projection::project(
            &agent,
            &at_adapters::mcp_projection::McpProjectionRequest {
                source: source.clone(),
                path: candidate.path.clone(),
                scope: target_scope,
                project: env.project.clone(),
                operation: projection_upsert(
                    &target.slug,
                    &definition,
                    true,
                    probe_transport,
                    probe_env_vars,
                ),
            },
        ) {
            match error {
                at_adapters::mcp_projection::McpProjectionError::Unsupported(_) => {
                    skipped_agents.push(agent);
                }
                other => refused_agents.push(McpAgentRefusal {
                    agent,
                    reason: other.to_string(),
                }),
            }
            continue;
        }
        let expected = if exists {
            ExpectedState::hash(at_fs::sha256_hex(&bytes))?
        } else {
            ExpectedState::absent()
        };
        targets.push(McpTarget {
            agent,
            scope: target_scope,
            item_id: target.item_id,
            slug: target.slug.clone(),
            path: candidate.path,
            source,
            expected,
            unsupported_acknowledged: false,
            installed: true,
        });
    }
    if targets.is_empty() {
        if !refused_agents.is_empty() {
            return Err(McpPlanError::NoSupportedAgent {
                reason: refused_agents
                    .iter()
                    .map(|refusal| refusal.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
        return Err(McpPlanError::NoNativeTarget {
            slug: target.slug,
            scope: target_scope,
        });
    }
    let store_ops = copy_scope_ops(
        &target_layout,
        &target_store,
        &target_manifest,
        &target.slug,
        &definition,
        target.item_id,
        target.existing_item.as_ref(),
    )?;
    let intent = McpIntent::CopyToScope {
        slug: target.slug.clone(),
        definition: definition.clone(),
    };
    let mut planned = plan(McpPlanRequest {
        registry,
        env,
        intent: &intent,
        targets: &targets,
        definition: &definition,
        scope: target_scope,
        project_id: scoped_project_id.clone(),
        locked_reason: None,
        store_layout: None,
        target_store_layout: None,
    })?;
    let preview_id = planned
        .native
        .as_ref()
        .map(|set| set.preview_id.clone())
        .ok_or(McpPlanError::NoNativeTarget {
            slug: target.slug.clone(),
            scope: target_scope,
        })?;
    let store = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id,
        operation_kind: OperationKind::parse("edit_mcp")?,
        scope: Some(target_scope),
        project_id: scoped_project_id,
        summary: format!(
            "Copied MCP server {} to {} scope",
            target.slug,
            scope_title(target_scope)
        ),
        ops: store_ops,
        created_at_unix_seconds: now_seconds(),
    };
    store.validate()?;
    // A copy applies both phases as one ChangeSet, so it needs no compensating set.
    planned.store_rollback = None;
    planned.store = Some(store);
    let agents = targets
        .iter()
        .map(|target| target.agent.clone())
        .collect::<Vec<_>>();
    let summary = McpCopySummary {
        source_scope,
        target_scope,
        target_slug: target.slug,
        target_item_id: target.item_id,
        replaced: target.replaced,
        agents: agents.clone(),
        refused_agents,
    };
    Ok(McpItemPlan {
        plan: planned,
        slug: source_slug,
        agents,
        skipped_agents,
        copy: Some(summary),
    })
}

/// Resolves the destination slug and identity a copy writes.
fn resolve_copy_target(
    source_slug: &str,
    definition: &McpServerDef,
    target_store: &at_store::mcp_store::LoadedMcpStore,
    target_manifest: &at_store::manifest::PlannedManifest,
    resolution: Option<&McpCopyResolution>,
    target_scope: Scope,
) -> Result<CopyTarget, McpPlanError> {
    let slug = source_slug.to_owned();
    if let Some(existing) = target_store.store.get(&slug) {
        if existing == definition {
            // The destination already holds this exact definition, so the copy would duplicate
            // an item the scope already owns.
            return Err(McpPlanError::DuplicateId(slug));
        }
        let existing_item = owned_item(target_manifest, &slug)?
            .ok_or_else(|| McpPlanError::UnmanagedStoreEntry { slug: slug.clone() })?;
        return match resolution {
            Some(McpCopyResolution::Replace) => Ok(CopyTarget {
                slug,
                item_id: existing_item.id,
                replaced: true,
                existing_item: Some(existing_item),
            }),
            Some(McpCopyResolution::Rename(requested)) => {
                let requested = at_store::layout::Slug::parse(requested)
                    .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?
                    .as_str()
                    .to_owned();
                if let Some(_other) = target_store.store.get(&requested) {
                    let clash = owned_item(target_manifest, &requested)?.ok_or(
                        McpPlanError::UnmanagedStoreEntry {
                            slug: requested.clone(),
                        },
                    )?;
                    return Err(McpPlanError::Collision {
                        slug: requested,
                        item_id: clash.id,
                        scope: target_scope,
                    });
                }
                Ok(CopyTarget {
                    slug: requested,
                    item_id: ItemId::new(),
                    replaced: false,
                    existing_item: None,
                })
            }
            None => Err(McpPlanError::Collision {
                slug,
                item_id: existing_item.id,
                scope: target_scope,
            }),
        };
    }
    if matches!(resolution, Some(McpCopyResolution::Replace)) {
        return Err(McpPlanError::NoTargetToReplace(source_slug.to_owned()));
    }
    let slug = match resolution {
        Some(McpCopyResolution::Rename(requested)) => at_store::layout::Slug::parse(requested)
            .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?
            .as_str()
            .to_owned(),
        _ => slug,
    };
    Ok(CopyTarget {
        slug,
        item_id: ItemId::new(),
        replaced: false,
        existing_item: None,
    })
}

/// The destination manifest record that owns one MCP slug, when it exists.
fn owned_item(
    manifest: &at_store::manifest::PlannedManifest,
    slug: &str,
) -> Result<Option<at_store::manifest::ManifestItem>, McpPlanError> {
    let parsed = at_store::layout::Slug::parse(slug)
        .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
    Ok(manifest
        .manifest
        .item_by_slug(at_model::ItemKind::McpServer, &parsed)
        .cloned())
}

/// Plans the destination `mcp.json` and the `store.json` record that owns the copied item.
fn copy_scope_ops(
    layout: &at_store::layout::StoreLayout,
    loaded: &at_store::mcp_store::LoadedMcpStore,
    manifest: &at_store::manifest::PlannedManifest,
    slug: &str,
    definition: &McpServerDef,
    item_id: ItemId,
    existing_item: Option<&at_store::manifest::ManifestItem>,
) -> Result<Vec<FileOp>, McpPlanError> {
    let mut store = loaded.store.clone();
    store.set(slug.to_owned(), definition.clone());
    let mut ops = vec![FileOp::Write {
        path: layout.mcp_file(),
        bytes: store.to_bytes()?,
        expected: model_expected(at_store::mcp_store::expected_state(loaded))?,
        follow_symlink: false,
        backup: true,
    }];
    let value = serde_json::to_value(definition)
        .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
    let hash = at_store::manifest::hash_mcp_entry(&value)
        .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
    let mut next = manifest.manifest.clone();
    match existing_item {
        Some(item) => {
            let mut refreshed = item.clone();
            refreshed.hash = hash;
            refreshed.name = Some(definition.display_name.clone());
            next.update_item(refreshed)?;
        }
        None => {
            let path = at_store::manifest::StorePath::parse("mcp.json")
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
            let parsed = at_store::layout::Slug::parse(slug)
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
            next.insert_item(
                at_store::manifest::ManifestItem::new(
                    item_id,
                    at_model::ItemKind::McpServer,
                    parsed,
                    path,
                    hash,
                )
                .with_name(definition.display_name.clone()),
            )?;
        }
    }
    ops.push(FileOp::Write {
        path: layout.manifest_path(),
        bytes: next.to_bytes()?,
        expected: match &manifest.hash {
            Some(hash) => ExpectedState::hash(hash.as_str())?,
            None => ExpectedState::Absent,
        },
        follow_symlink: false,
        backup: true,
    });
    Ok(ops)
}

/// Applies a reviewed row-action plan: canonical store first, then native documents.
///
/// The store phase is always concluded here: committed once every native document landed, or
/// rolled back when one fails. A store phase no longer marks its own journal committed, so an
/// unconcluded token would leave a journal behind until the next start.
///
/// # Errors
/// Returns the ChangeSet application error after restoring the store phase. A rollback that itself
/// fails is reported together with the failure that began it.
pub fn apply_item_plan(
    plan: &McpItemPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
) -> Result<at_fs::ApplyOutcome, at_fs::ChangeSetApplyError> {
    let (store_outcome, handle) = match plan.plan.store.as_ref() {
        Some(store) => {
            let mut applied = at_fs::apply_with_roots_reversible(store, context, roots)?;
            let handle = applied.rollback_handle.take();
            (Some(applied), handle)
        }
        None => (None, None),
    };
    let Some(native) = plan.plan.native.as_ref() else {
        let applied = store_outcome.ok_or(at_fs::ChangeSetApplyError::Invalid(
            at_model::ChangeSetError::Empty,
        ))?;
        if let Some(handle) = handle {
            at_fs::commit_reversible(handle)?;
        }
        return Ok(applied);
    };
    match at_fs::apply_with_roots(native, context, roots) {
        Ok(native_outcome) => {
            if let Some(handle) = handle {
                at_fs::commit_reversible(handle)?;
            }
            Ok(native_outcome)
        }
        Err(error) => {
            if let Some(handle) = handle
                && let Err(rollback_failure) = at_fs::rollback_committed(handle, context.recorder)
            {
                return Err(at_fs::ChangeSetApplyError::Apply {
                    cause: error.to_string(),
                    rollback: rollback_failure.to_string(),
                });
            }
            Err(error)
        }
    }
}

/// Planner result for MCP file changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPlan {
    /// Canonical-store write ChangeSet, applied before native files.
    pub store: Option<ChangeSet>,
    /// Native agent-document ChangeSet.
    pub native: Option<ChangeSet>,
    /// Compensating ChangeSet for `store`, applied if native deployment fails.
    pub store_rollback: Option<ChangeSet>,
    /// Non-fatal policy warnings.
    pub warnings: Vec<String>,
    /// Whether some requested agent behavior is unsupported.
    pub status: McpPlanStatus,
}

impl McpPlan {
    /// Returns the forward ChangeSets in safe application order: store, then native.
    pub fn apply_order(&self) -> impl Iterator<Item = &ChangeSet> {
        self.store.iter().chain(self.native.iter())
    }

    /// Returns every forward operation as one ChangeSet, canonical store first.
    ///
    /// A copy applies this single set so one journaled operation — and one undo — covers the
    /// destination store and every native document it projected.
    ///
    /// # Errors
    /// Returns [`at_model::ChangeSetError`] when the plan has no forward operations or the
    /// merged set fails validation.
    pub fn merged(&self) -> Result<ChangeSet, at_model::ChangeSetError> {
        let first = self
            .apply_order()
            .next()
            .ok_or(at_model::ChangeSetError::Empty)?;
        let mut ops = Vec::new();
        for set in self.apply_order() {
            ops.extend(set.ops.iter().cloned());
        }
        let merged = ChangeSet {
            id: uuid::Uuid::now_v7().to_string(),
            preview_id: first.preview_id.clone(),
            operation_kind: first.operation_kind.clone(),
            scope: first.scope,
            project_id: first.project_id.clone(),
            summary: first.summary.clone(),
            ops,
            created_at_unix_seconds: first.created_at_unix_seconds,
        };
        merged.validate()?;
        Ok(merged)
    }
}

/// Typed planner outcome accompanying generated ChangeSets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpPlanStatus {
    /// All requested changes are supported.
    Ready,
    /// The plan includes safe supported changes but one agent behavior is unsupported.
    NotSupported {
        /// Agent whose requested behavior is unsupported.
        agent: AgentId,
        /// Human-readable safe explanation.
        reason: String,
    },
}
/// Full input bundle for an MCP deployment plan.
pub struct McpPlanRequest<'a> {
    /// Registered native adapters.
    pub registry: &'a AgentAdapterRegistry,
    /// Injected paths and environment.
    pub env: &'a AgentEnvironment,
    /// Intent being planned.
    pub intent: &'a McpIntent,
    /// Selected native targets.
    pub targets: &'a [McpTarget],
    /// Canonical definition to project.
    pub definition: &'a McpServerDef,
    /// Definition scope.
    pub scope: Scope,
    /// Project identity, when scoped to one repository.
    pub project_id: Option<String>,
    /// Global lock denial reason, if locked.
    pub locked_reason: Option<&'a str>,
    /// Canonical store to update for add/edit/rename/remove intents.
    pub store_layout: Option<&'a at_store::layout::StoreLayout>,
    /// Destination canonical store for promote and move intents.
    pub target_store_layout: Option<&'a at_store::layout::StoreLayout>,
}

/// Builds native MCP document operations for one or more selected targets.
///
/// The adapter's typed capability projection is checked before writing. No IO is performed.
pub fn plan(request: McpPlanRequest<'_>) -> Result<McpPlan, McpPlanError> {
    if let Some(reason) = request.locked_reason {
        return Err(McpPlanError::Locked(reason.to_owned()));
    }
    let mut store_ops = Vec::new();
    if matches!(
        request.intent,
        McpIntent::Add { .. }
            | McpIntent::Edit { .. }
            | McpIntent::Rename { .. }
            | McpIntent::Remove
    ) {
        store_ops.extend(canonical_store_ops(&request)?);
    } else if let McpIntent::DisableInRepo { project } = request.intent {
        store_ops.push(tombstone_op(project, &request)?);
    } else if matches!(
        request.intent,
        McpIntent::Promote { .. } | McpIntent::MoveToScope { .. }
    ) {
        store_ops.extend(scope_move_ops(&request)?);
    }
    let mut native_ops = Vec::new();
    let mut warnings = Vec::new();
    let mut status = McpPlanStatus::Ready;
    if matches!(request.intent, McpIntent::DisableInRepo { .. })
        && let Some(target) = request
            .targets
            .iter()
            .find(|target| target.agent == AgentId::Codex)
    {
        let reason = "Codex does not support project-scoped MCP configuration";
        warnings.push(format!(
            "{}: {reason}; project tombstone was recorded",
            target.agent
        ));
        status = McpPlanStatus::NotSupported {
            agent: target.agent.clone(),
            reason: reason.to_owned(),
        };
    }
    for target in request.targets {
        if matches!(
            request.intent,
            McpIntent::DisableInRepo { .. }
                | McpIntent::Promote { .. }
                | McpIntent::MoveToScope { .. }
        ) {
            continue;
        }
        if let McpIntent::SetEnabledForAgent { agent, .. } = request.intent
            && agent != &target.agent
        {
            continue;
        }
        if !target.installed {
            return Err(McpPlanError::AgentNotInstalled(target.agent.clone()));
        }
        let mut rendered = project_transport(&target.agent, &request.definition.transport);
        if let Err(error) =
            require_unsupported_acknowledgement(&rendered, target.unsupported_acknowledged)
        {
            return Err(McpPlanError::CapabilityRefusal {
                agent: target.agent.clone(),
                reason: error.to_string(),
            });
        }
        let env_vars = apply_secret_policy(
            &target.agent,
            &target.path,
            &request.env.home,
            &mut rendered.transport,
            &mut warnings,
        );
        if !rendered.unsupported.is_empty() {
            warnings.push(format!(
                "{} drops unsupported MCP fields: {:?}",
                target.agent, rendered.unsupported
            ));
        }
        let mut source = target.source.clone();
        match request.intent {
            McpIntent::Add { slug, definition }
            | McpIntent::Edit { slug, definition }
            | McpIntent::CopyToScope { slug, definition } => {
                let op = projection_upsert(
                    slug,
                    definition,
                    true,
                    rendered.transport.clone(),
                    env_vars.clone(),
                );
                source = project_once(
                    &target.agent,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &source,
                    op,
                )?;
            }
            McpIntent::SetEnabledForAgent { enabled, .. } => {
                let op = if !enabled && target.agent == AgentId::ClaudeCode {
                    at_adapters::mcp_projection::McpProjectionOperation::Remove {
                        name: target.slug.clone(),
                    }
                } else {
                    projection_upsert(
                        &target.slug,
                        request.definition,
                        *enabled,
                        rendered.transport.clone(),
                        env_vars.clone(),
                    )
                };
                source = project_once(
                    &target.agent,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &source,
                    op,
                )?;
            }
            McpIntent::Remove => {
                // The delete plan enumerates every installed agent so it also cleans up an
                // entry whose deployment row was lost; an agent whose file never held this
                // server simply has nothing to remove.
                let present = at_adapters::mcp_projection::contains(
                    &target.agent,
                    &source,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &target.slug,
                )
                .map_err(|error| McpPlanError::CapabilityRefusal {
                    agent: target.agent.clone(),
                    reason: error.to_string(),
                })?;
                if !present {
                    continue;
                }
                source = project_once(
                    &target.agent,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &source,
                    at_adapters::mcp_projection::McpProjectionOperation::Remove {
                        name: target.slug.clone(),
                    },
                )?;
            }
            McpIntent::Rename { from, to } => {
                source = project_once(
                    &target.agent,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &source,
                    at_adapters::mcp_projection::McpProjectionOperation::Remove {
                        name: from.clone(),
                    },
                )?;
                source = project_once(
                    &target.agent,
                    &target.path,
                    target.scope,
                    request.env.project.as_deref(),
                    &source,
                    projection_upsert(
                        to,
                        request.definition,
                        true,
                        rendered.transport.clone(),
                        env_vars,
                    ),
                )?;
            }
            McpIntent::DisableInRepo { .. }
            | McpIntent::Promote { .. }
            | McpIntent::MoveToScope { .. } => {
                return Err(McpPlanError::CapabilityRefusal {
                    agent: target.agent.clone(),
                    reason:
                        "intent requires a canonical store/tombstone plan, not a native projection"
                            .to_owned(),
                });
            }
        }
        let _adapter = request.registry.get(&target.agent)?.ok_or_else(|| {
            McpPlanError::CapabilityRefusal {
                agent: target.agent.clone(),
                reason: "agent adapter is not registered".to_owned(),
            }
        })?;
        native_ops.push(FileOp::Write {
            path: target.path.clone(),
            bytes: source.into_bytes(),
            expected: target.expected.clone(),
            follow_symlink: at_adapters::adapter::follows_native_config_symlink(&target.agent),
            backup: true,
        });
    }
    let kind = match request.intent {
        McpIntent::Promote { .. } => "promote",
        McpIntent::MoveToScope { .. } => "move",
        McpIntent::Remove => "delete",
        _ => "edit_mcp",
    };
    let preview_id = uuid::Uuid::now_v7().to_string();
    let operation_kind = OperationKind::parse(kind)?;
    let now = now_seconds();
    let store = make_change_set(
        &store_ops,
        &preview_id,
        &operation_kind,
        &request,
        now,
        store_summary(&request),
    )?;
    let native = make_change_set(
        &native_ops,
        &preview_id,
        &operation_kind,
        &request,
        now,
        native_summary(&request),
    )?;
    let store_rollback = store
        .as_ref()
        .map(|set| rollback_change_set(set, &request, now))
        .transpose()?;
    Ok(McpPlan {
        store,
        native,
        store_rollback,
        warnings,
        status,
    })
}

fn make_change_set(
    ops: &[FileOp],
    preview_id: &str,
    operation_kind: &OperationKind,
    request: &McpPlanRequest<'_>,
    now: u64,
    summary: String,
) -> Result<Option<ChangeSet>, McpPlanError> {
    if ops.is_empty() {
        return Ok(None);
    }
    let change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: preview_id.to_owned(),
        operation_kind: operation_kind.clone(),
        scope: Some(request.scope),
        project_id: request.project_id.clone(),
        summary,
        ops: ops.to_vec(),
        created_at_unix_seconds: now,
    };
    change_set.validate()?;
    Ok(Some(change_set))
}

fn rollback_change_set(
    forward: &ChangeSet,
    request: &McpPlanRequest<'_>,
    now: u64,
) -> Result<ChangeSet, McpPlanError> {
    let mut ops = Vec::with_capacity(forward.ops.len());
    for op in forward.ops.iter().rev() {
        let FileOp::Write { path, bytes, .. } = op else {
            return Err(McpPlanError::RollbackUnsupported);
        };
        let before = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(McpPlanError::RollbackRead {
                    path: path.clone(),
                    source,
                });
            }
        };
        let expected = ExpectedState::hash(at_fs::sha256_hex(bytes))?;
        ops.push(match before {
            Some(previous) => FileOp::Write {
                path: path.clone(),
                bytes: previous,
                expected,
                follow_symlink: false,
                backup: true,
            },
            None => FileOp::Delete {
                path: path.clone(),
                expected,
            },
        });
    }
    let rollback = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: forward.preview_id.clone(),
        operation_kind: forward.operation_kind.clone(),
        scope: forward.scope,
        project_id: request.project_id.clone(),
        summary: "Rollback MCP store after native deployment failure".to_owned(),
        ops,
        created_at_unix_seconds: now,
    };
    rollback.validate()?;
    Ok(rollback)
}
fn canonical_store_ops(request: &McpPlanRequest<'_>) -> Result<Vec<FileOp>, McpPlanError> {
    let layout = request
        .store_layout
        .ok_or(McpPlanError::MissingStoreLayout)?;
    // A first-run store has no `store.json` yet; the plan extends an empty manifest and writes
    // it create-only. The reviewed `ChangeSet` also carries the store skeleton, so the item and
    // its store land as one operation.
    let manifest_plan = at_store::manifest::StoreManifest::load_for_plan(layout)?;
    let mut loaded = at_store::mcp_store::McpStore::load(layout)?;
    let mut manifest_ops = ManifestEdits::default();
    match request.intent {
        McpIntent::Add { slug, definition } | McpIntent::Edit { slug, definition } => {
            loaded.store.set(slug.clone(), definition.clone());
            manifest_ops.upsert(slug, definition)?;
        }
        McpIntent::Rename { from, to } => {
            if loaded.store.get(to).is_some() {
                return Err(McpPlanError::DuplicateId(to.clone()));
            }
            // A rename keeps the item's opaque identity: deployments and history stay attached
            // to the same row instead of forking a second item.
            let parsed_from = at_store::layout::Slug::parse(from.as_str())
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
            let renamed_id = manifest_plan
                .manifest
                .item_by_slug(at_model::ItemKind::McpServer, &parsed_from)
                .map(|item| item.id);
            loaded
                .store
                .remove(from)
                .ok_or_else(|| McpPlanError::MissingStoreEntry(from.clone()))?;
            loaded.store.set(to.clone(), request.definition.clone());
            manifest_ops.remove_slug(from);
            manifest_ops.upsert_with_id(to, request.definition, renamed_id)?;
        }
        McpIntent::Remove => {
            let target = request
                .targets
                .first()
                .ok_or(McpPlanError::MissingStoreLayout)?;
            let slug = target.slug.as_str();
            if loaded.store.remove(slug).is_none() {
                return Err(McpPlanError::MissingStoreEntry(slug.to_owned()));
            }
            manifest_ops.remove_item(target.item_id);
        }
        _ => return Err(McpPlanError::MissingStoreLayout),
    }
    let mut ops = vec![FileOp::Write {
        path: layout.mcp_file(),
        bytes: loaded.store.to_bytes()?,
        expected: model_expected(at_store::mcp_store::expected_state(&loaded))?,
        follow_symlink: false,
        backup: true,
    }];
    if !manifest_ops.is_empty() {
        let mut manifest = manifest_plan.manifest;
        let expected = match manifest_plan.hash {
            Some(hash) => ExpectedState::hash(hash.as_str())?,
            None => ExpectedState::Absent,
        };
        manifest_ops.apply(&mut manifest)?;
        ops.push(FileOp::Write {
            path: layout.manifest_path(),
            bytes: manifest.to_bytes()?,
            expected,
            follow_symlink: false,
            backup: true,
        });
    }
    Ok(ops)
}

/// Manifest changes one canonical MCP intent needs, applied to `store.json` in the same plan.
///
/// `mcp.json` is content only; the item identity, slug and per-entry hash live in the manifest
/// (contract decision 3). Every command that changes `mcp.json` must therefore record the
/// matching item, otherwise the next `store.json` rebuild cannot match the entry, allocates a
/// new [`ItemId`] and every deployment row keeps pointing at the old one.
#[derive(Default)]
struct ManifestEdits {
    upserts: Vec<(
        String,
        String,
        at_store::manifest::ContentHash,
        Option<ItemId>,
    )>,
    removed_slugs: Vec<String>,
    removed_ids: Vec<ItemId>,
}

impl ManifestEdits {
    fn upsert(&mut self, slug: &str, definition: &McpServerDef) -> Result<(), McpPlanError> {
        self.upsert_with_id(slug, definition, None)
    }

    fn upsert_with_id(
        &mut self,
        slug: &str,
        definition: &McpServerDef,
        item_id: Option<ItemId>,
    ) -> Result<(), McpPlanError> {
        let value = serde_json::to_value(definition)
            .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
        let hash = at_store::manifest::hash_mcp_entry(&value)
            .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
        self.upserts.push((
            slug.to_owned(),
            definition.display_name.clone(),
            hash,
            item_id,
        ));
        Ok(())
    }

    fn remove_slug(&mut self, slug: &str) {
        self.removed_slugs.push(slug.to_owned());
    }

    fn remove_item(&mut self, item_id: ItemId) {
        self.removed_ids.push(item_id);
    }

    fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removed_slugs.is_empty() && self.removed_ids.is_empty()
    }

    fn apply(&self, manifest: &mut at_store::manifest::StoreManifest) -> Result<(), McpPlanError> {
        for slug in &self.removed_slugs {
            let parsed = at_store::layout::Slug::parse(slug)
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
            if let Some(item) = manifest
                .item_by_slug(at_model::ItemKind::McpServer, &parsed)
                .map(|item| item.id)
            {
                manifest.remove_item(item);
            }
        }
        for item_id in &self.removed_ids {
            if manifest.remove_item(*item_id).is_none() {
                return Err(McpPlanError::MissingStoreEntry(item_id.to_string()));
            }
        }
        for (slug, name, hash, item_id) in &self.upserts {
            upsert_manifest_item(manifest, slug, name, hash, *item_id)?;
        }
        Ok(())
    }
}

/// Inserts or refreshes the manifest record for one canonical MCP entry.
fn upsert_manifest_item(
    manifest: &mut at_store::manifest::StoreManifest,
    slug: &str,
    name: &str,
    hash: &at_store::manifest::ContentHash,
    item_id: Option<ItemId>,
) -> Result<(), McpPlanError> {
    let slug = at_store::layout::Slug::parse(slug)
        .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
    match manifest
        .item_by_slug(at_model::ItemKind::McpServer, &slug)
        .cloned()
    {
        Some(mut item) => {
            item.hash = hash.clone();
            item.name = Some(name.to_owned());
            manifest
                .update_item(item)
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
        }
        None => {
            let path = at_store::manifest::StorePath::parse("mcp.json")
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
            manifest
                .insert_item(
                    at_store::manifest::ManifestItem::new(
                        item_id.unwrap_or_else(ItemId::new),
                        at_model::ItemKind::McpServer,
                        slug,
                        path,
                        hash.clone(),
                    )
                    .with_name(name),
                )
                .map_err(|error| McpPlanError::InvalidItem(error.to_string()))?;
        }
    }
    Ok(())
}

fn tombstone_op(
    project: &std::path::Path,
    request: &McpPlanRequest<'_>,
) -> Result<FileOp, McpPlanError> {
    let first = request
        .targets
        .first()
        .ok_or_else(|| McpPlanError::MissingStoreEntry("MCP item id".to_owned()))?;
    let layout = at_store::layout::StoreLayout::new(Scope::Project, project.join(".config-studio"));
    let mut loaded = at_store::manifest::StoreManifest::load(&layout)?;
    let agents = if request.targets.is_empty() {
        at_store::manifest::DisabledAgents::All
    } else {
        at_store::manifest::DisabledAgents::Only(
            request
                .targets
                .iter()
                .map(|target| target.agent.clone())
                .collect(),
        )
    };
    loaded
        .manifest
        .disable_inherited(first.item_id, at_model::ItemKind::McpServer, agents)?;
    let bytes = loaded.manifest.to_bytes()?;
    Ok(FileOp::Write {
        path: layout.manifest_path(),
        bytes,
        expected: ExpectedState::hash(loaded.hash.as_str())?,
        follow_symlink: false,
        backup: true,
    })
}
fn scope_move_ops(request: &McpPlanRequest<'_>) -> Result<Vec<FileOp>, McpPlanError> {
    let source_layout = request
        .store_layout
        .ok_or(McpPlanError::MissingStoreLayout)?;
    let destination_layout = request
        .target_store_layout
        .ok_or(McpPlanError::MissingStoreLayout)?;
    if source_layout.root() == destination_layout.root() {
        return Err(McpPlanError::MissingStoreLayout);
    }
    let slug = request
        .targets
        .first()
        .map(|target| target.slug.as_str())
        .ok_or_else(|| McpPlanError::MissingStoreEntry("MCP slug".to_owned()))?;
    let mut source = at_store::mcp_store::McpStore::load(source_layout)?;
    let mut destination = at_store::mcp_store::McpStore::load(destination_layout)?;
    if destination.store.get(slug).is_some() {
        return Err(McpPlanError::DuplicateId(slug.to_owned()));
    }
    let definition = source
        .store
        .get(slug)
        .cloned()
        .ok_or_else(|| McpPlanError::MissingStoreEntry(slug.to_owned()))?;
    destination.store.set(slug, definition);
    let mut ops = vec![FileOp::Write {
        path: destination_layout.mcp_file(),
        bytes: destination.store.to_bytes()?,
        expected: model_expected(at_store::mcp_store::expected_state(&destination))?,
        follow_symlink: false,
        backup: true,
    }];
    if matches!(request.intent, McpIntent::MoveToScope { .. }) {
        source.store.remove(slug);
        ops.push(FileOp::Write {
            path: source_layout.mcp_file(),
            bytes: source.store.to_bytes()?,
            expected: model_expected(at_store::mcp_store::expected_state(&source))?,
            follow_symlink: false,
            backup: true,
        });
    }
    Ok(ops)
}

fn model_expected(expected: at_fs::ExpectedState) -> Result<ExpectedState, McpPlanError> {
    match expected {
        at_fs::ExpectedState::Absent => Ok(ExpectedState::Absent),
        at_fs::ExpectedState::Hash(hash) | at_fs::ExpectedState::HashAndMtime { hash, .. } => {
            Ok(ExpectedState::hash(hash)?)
        }
    }
}
/// One user-facing sentence for the store phase of a plan.
///
/// The summary becomes the operation-history line when the plan is applied, so it names
/// the server and the action instead of planner internals.
fn store_summary(request: &McpPlanRequest<'_>) -> String {
    let name = request.definition.display_name.as_str();
    match request.intent {
        McpIntent::Add { .. } => format!("Added MCP server {name}"),
        McpIntent::Edit { .. } => format!("Updated MCP server {name}"),
        McpIntent::Rename { from, to } => format!("Renamed MCP server {from} to {to}"),
        McpIntent::SetEnabledForAgent { enabled, .. } => enabled_summary(*enabled, name),
        McpIntent::DisableInRepo { .. } => format!("Disabled MCP server {name} in this repo"),
        McpIntent::Promote { target } => {
            format!(
                "Promoted MCP server {name} to {} scope",
                scope_title(*target)
            )
        }
        McpIntent::MoveToScope { target } => {
            format!("Moved MCP server {name} to {} scope", scope_title(*target))
        }
        McpIntent::CopyToScope { .. } => format!("Copied MCP server {name} to another scope"),
        McpIntent::Remove => format!("Removed MCP server {name}"),
    }
}

/// One user-facing sentence for the native phase of a plan.
fn native_summary(request: &McpPlanRequest<'_>) -> String {
    let name = request.definition.display_name.as_str();
    let agents = targets_label(request.targets);
    match request.intent {
        McpIntent::Add { .. } => format!("Wrote MCP server {name} to {agents}"),
        McpIntent::Edit { .. } => format!("Updated MCP server {name} in {agents}"),
        McpIntent::Rename { from, to } => {
            format!("Renamed MCP server {from} to {to} in {agents}")
        }
        McpIntent::SetEnabledForAgent { enabled, agent, .. } => format!(
            "{} MCP server {name} for {}",
            if *enabled { "Enabled" } else { "Disabled" },
            agent.default_display_name()
        ),
        McpIntent::DisableInRepo { .. } => {
            format!("Disabled MCP server {name} in this repo for {agents}")
        }
        McpIntent::Promote { .. } | McpIntent::MoveToScope { .. } => {
            format!("Updated MCP server {name} in {agents}")
        }
        McpIntent::CopyToScope { .. } => format!("Wrote MCP server {name} to {agents}"),
        McpIntent::Remove => format!("Removed MCP server {name} from {agents}"),
    }
}

fn enabled_summary(enabled: bool, name: &str) -> String {
    format!(
        "{} MCP server {name}",
        if enabled { "Enabled" } else { "Disabled" }
    )
}

/// Display names of the agents one plan writes to, joined for a sentence.
fn targets_label(targets: &[McpTarget]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for target in targets {
        let name = target.agent.default_display_name();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    match names.len() {
        0 => "no agents".to_owned(),
        1 => names[0].to_owned(),
        _ => names
            .split_last()
            .map(|(last, rest)| format!("{} and {last}", rest.join(", ")))
            .unwrap_or_else(|| names.join(", ")),
    }
}

/// Title-cased scope name used in user-facing summaries.
fn scope_title(scope: at_model::Scope) -> &'static str {
    match scope {
        at_model::Scope::Global => "Global",
        at_model::Scope::User => "User",
        at_model::Scope::Project => "Project",
        at_model::Scope::Local => "Local",
    }
}

fn projection_upsert(
    name: &str,
    definition: &McpServerDef,
    enabled: bool,
    transport: at_model::McpTransport,
    env_vars: Vec<String>,
) -> at_adapters::mcp_projection::McpProjectionOperation {
    at_adapters::mcp_projection::McpProjectionOperation::Upsert {
        name: name.to_owned(),
        transport: Box::new(transport),
        enabled,
        extra: definition.extra.clone(),
        env_vars,
    }
}

/// What one destination agent can do with a secret-valued field.
enum SecretFieldPolicy {
    /// Keep the `${VAR}` reference in the native document.
    Reference,
    /// Drop the field and pass the variable through the agent's environment instead.
    PassThrough(String),
    /// Drop the field and refuse the agent; the reason is shown to the user.
    Refused(String),
}

/// Classifies every secret-valued field of one transport for one destination agent.
fn secret_field_policies(
    agent: &AgentId,
    path: &std::path::Path,
    home: &std::path::Path,
    transport: &at_model::McpTransport,
) -> Vec<(String, SecretFieldPolicy)> {
    let fields = match transport {
        at_model::McpTransport::Stdio { env, .. } => env,
        at_model::McpTransport::Http { headers, .. } => headers,
    };
    let Some(capabilities) = capabilities(agent, home) else {
        return fields
            .iter()
            .filter(|(_, value)| matches!(value, EnvValue::Secret(_)))
            .map(|(field, _)| {
                (
                    field.clone(),
                    SecretFieldPolicy::Refused(format!(
                        "{agent}: refused secret field `{field}` because launch behavior is unverified"
                    )),
                )
            })
            .collect();
    };
    fields
        .iter()
        .filter_map(|(field, value)| {
            let EnvValue::Secret(secret) = value else {
                return None;
            };
            let policy = match plan_secret_injection(
                &capabilities,
                path,
                &secret.env_name,
                &secret.keychain_account,
            ) {
                SecretInjection::ConfigReference { .. } => SecretFieldPolicy::Reference,
                SecretInjection::EnvironmentPassThrough { variable } => {
                    SecretFieldPolicy::PassThrough(variable)
                }
                SecretInjection::Refused {
                    variable,
                    shell_snippet,
                } => SecretFieldPolicy::Refused(format!(
                    "{agent}: refused secret `{variable}` for field `{field}`; {shell_snippet}"
                )),
                _ => SecretFieldPolicy::Refused(format!(
                    "{agent}: refused secret field `{field}` because launch policy is unknown"
                )),
            };
            Some((field.clone(), policy))
        })
        .collect()
}

fn apply_secret_policy(
    agent: &AgentId,
    path: &std::path::Path,
    home: &std::path::Path,
    transport: &mut at_model::McpTransport,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut env_vars = std::collections::BTreeSet::new();
    for (field, policy) in secret_field_policies(agent, path, home, transport) {
        match policy {
            SecretFieldPolicy::Reference => {}
            SecretFieldPolicy::PassThrough(variable) => {
                drop_secret_field(transport, &field);
                env_vars.insert(variable);
            }
            SecretFieldPolicy::Refused(reason) => {
                drop_secret_field(transport, &field);
                warnings.push(reason);
            }
        }
    }
    env_vars.into_iter().collect()
}

/// The reason a destination agent cannot reference this definition's secrets, when there is one.
///
/// A copy refuses such an agent instead of writing a native document whose secret field was
/// dropped: a configuration that only looks valid is worse than no configuration.
fn secret_refusal(
    agent: &AgentId,
    path: &std::path::Path,
    home: &std::path::Path,
    transport: &at_model::McpTransport,
) -> Option<String> {
    secret_field_policies(agent, path, home, transport)
        .into_iter()
        .find_map(|(_, policy)| match policy {
            SecretFieldPolicy::Refused(reason) => Some(reason),
            SecretFieldPolicy::Reference | SecretFieldPolicy::PassThrough(_) => None,
        })
}

/// Removes one field from a rendered transport.
fn drop_secret_field(transport: &mut at_model::McpTransport, field: &str) {
    match transport {
        at_model::McpTransport::Stdio { env, .. } => {
            env.remove(field);
        }
        at_model::McpTransport::Http { headers, .. } => {
            headers.remove(field);
        }
    }
}
fn project_once(
    agent: &AgentId,
    path: &std::path::Path,
    scope: Scope,
    project: Option<&std::path::Path>,
    source: &str,
    operation: at_adapters::mcp_projection::McpProjectionOperation,
) -> Result<String, McpPlanError> {
    let projected = at_adapters::mcp_projection::project(
        agent,
        &at_adapters::mcp_projection::McpProjectionRequest {
            source: source.to_owned(),
            path: path.to_path_buf(),
            scope,
            project: project.map(std::path::Path::to_path_buf),
            operation,
        },
    )
    .map_err(|error| McpPlanError::CapabilityRefusal {
        agent: agent.clone(),
        reason: error.to_string(),
    })?;
    String::from_utf8(projected).map_err(|error| McpPlanError::CapabilityRefusal {
        agent: agent.clone(),
        reason: error.to_string(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{EnvValue, McpTransport};
    use std::{collections::BTreeMap, path::PathBuf};

    fn fixture() -> McpServerDef {
        McpServerDef {
            display_name: "Fixture server".to_owned(),
            description: None,
            transport: McpTransport::Stdio {
                command: "fixture-server".to_owned(),
                args: vec!["--safe".to_owned()],
                cwd: None,
                env: BTreeMap::<String, EnvValue>::new(),
            },
            extra: BTreeMap::new(),
        }
    }
    fn target(agent: AgentId, path: &str, source: &str) -> McpTarget {
        McpTarget {
            agent,
            scope: Scope::User,
            item_id: ItemId::new(),
            slug: "fixture".to_owned(),
            path: PathBuf::from(path),
            source: source.to_owned(),
            expected: ExpectedState::hash(at_fs::sha256_hex(source.as_bytes())).expect("hash"),
            unsupported_acknowledged: false,
            installed: true,
        }
    }

    fn store_layout(temp: &tempfile::TempDir) -> at_store::layout::StoreLayout {
        let layout = at_store::layout::StoreLayout::new(Scope::User, temp.path());
        at_store::layout::init_store(Scope::User, temp.path()).expect("initialised store");
        layout
    }

    #[test]
    fn add_projects_to_three_detected_native_files() {
        let registry = AgentAdapterRegistry::register();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let definition = fixture();
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let targets = [
            target(
                AgentId::ClaudeCode,
                "/fixture/home/.claude.json",
                "{\"mcpServers\":{}}",
            ),
            target(AgentId::Codex, "/fixture/home/.codex/config.toml", ""),
            target(AgentId::Omp, "/fixture/home/.omp/agent/mcp.json", "{}"),
        ];
        let plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: &targets,
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        })
        .expect("plan");
        let native = plan.native.as_ref().expect("native changeset");
        let store = plan.store.as_ref().expect("canonical store changeset");
        assert_eq!(native.ops.len(), 3);
        assert_eq!(
            store.ops.len(),
            2,
            "the canonical phase writes mcp.json and its store.json record"
        );
        assert!(
            store
                .ops
                .iter()
                .filter_map(|op| op.destination())
                .any(|path| path.ends_with("store.json")),
            "the plan records the item identity"
        );
        assert_eq!(native.preview_id, store.preview_id);
        assert_eq!(plan.apply_order().count(), 2);
        let paths: Vec<_> = native
            .ops
            .iter()
            .filter_map(|op| op.destination())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with(".claude.json")));
        assert!(paths.iter().any(|p| p.ends_with("config.toml")));
        assert!(paths.iter().any(|p| p.ends_with("mcp.json")));
    }

    #[test]
    fn lock_and_capability_refusal_happen_before_file_operations() {
        let registry = AgentAdapterRegistry::register();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let definition = fixture();
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let target = target(AgentId::Codex, "/fixture/config.toml", "");
        let make_request = |locked_reason| McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: std::slice::from_ref(&target),
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason,
            store_layout: Some(&store),
            target_store_layout: None,
        };
        assert!(matches!(
            plan(make_request(Some("organization policy"))),
            Err(McpPlanError::Locked(_))
        ));
        let plan = plan(make_request(None)).expect("supported plan");
        assert_eq!(plan.native.expect("native").ops.len(), 1);
        assert_eq!(
            plan.store.expect("store").ops.len(),
            2,
            "adding also records the item identity in store.json"
        );
    }
    #[test]
    fn rename_collision_returns_duplicate_id_before_file_ops() {
        use std::fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let mut canonical = at_store::mcp_store::McpStore::empty();
        canonical.set("from", fixture());
        canonical.set("to", fixture());
        fs::write(store.mcp_file(), canonical.to_bytes().expect("mcp bytes")).expect("seed store");
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let intent = McpIntent::Rename {
            from: "from".to_owned(),
            to: "to".to_owned(),
        };
        let target = target(AgentId::Codex, "/fixture/config.toml", "");
        let result = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: std::slice::from_ref(&target),
            definition: &fixture(),
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        });
        assert!(matches!(result, Err(McpPlanError::DuplicateId(id)) if id == "to"));
    }

    #[test]
    fn remove_plans_canonical_manifest_and_native_changesets() {
        use std::fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let mut canonical = at_store::mcp_store::McpStore::empty();
        canonical.set("fixture", fixture());
        fs::write(store.mcp_file(), canonical.to_bytes().expect("MCP bytes"))
            .expect("seed MCP store");
        let item_id = ItemId::new();
        let mut manifest =
            at_store::manifest::StoreManifest::new(Scope::User, at_store::layout::StoreId::new());
        manifest
            .insert_item(at_store::manifest::ManifestItem::new(
                item_id,
                at_model::ItemKind::McpServer,
                at_store::layout::Slug::parse("fixture").expect("slug"),
                at_store::manifest::StorePath::parse("mcp.json").expect("path"),
                at_store::manifest::ContentHash::from_bytes(b"fixture"),
            ))
            .expect("manifest item");
        fs::write(
            store.manifest_path(),
            manifest.to_bytes().expect("manifest bytes"),
        )
        .expect("seed manifest");

        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let definition = fixture();
        let intent = McpIntent::Remove;
        // The native file still holds the entry, so the removal is planned for it.
        let target = McpTarget {
            item_id,
            ..target(
                AgentId::Codex,
                "/fixture/config.toml",
                "[mcp_servers.fixture]\ncommand = \"x\"\n",
            )
        };
        let remove_plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: std::slice::from_ref(&target),
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        })
        .expect("remove plan");

        let store_plan = remove_plan.store.as_ref().expect("canonical removal");
        assert_eq!(store_plan.ops.len(), 2);
        assert!(remove_plan.native.is_some());
        assert_eq!(remove_plan.apply_order().count(), 2);

        // An agent file that never held the entry has nothing to remove; only the canonical
        // store and manifest change.
        let absent = McpTarget {
            agent: AgentId::Codex,
            scope: Scope::User,
            item_id,
            slug: "fixture".to_owned(),
            path: PathBuf::from("/fixture/config.toml"),
            source: String::new(),
            expected: ExpectedState::hash(at_fs::sha256_hex(b"")).expect("hash"),
            unsupported_acknowledged: false,
            installed: true,
        };
        let absent_plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: std::slice::from_ref(&absent),
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        })
        .expect("remove plan without a native target");
        assert!(
            absent_plan.native.is_none(),
            "an absent entry produces no native write"
        );
    }

    #[test]
    fn disable_in_repo_for_codex_plans_only_manifest_tombstone_with_warning() {
        use std::fs;

        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path();
        let root = project.join(".config-studio");
        fs::create_dir_all(&root).expect("project store");
        let item_id = ItemId::new();
        let mut manifest = at_store::manifest::StoreManifest::new(
            Scope::Project,
            at_store::layout::StoreId::new(),
        );
        let item = at_store::manifest::ManifestItem::new(
            item_id,
            at_model::ItemKind::McpServer,
            at_store::layout::Slug::parse("fixture").expect("slug"),
            at_store::manifest::StorePath::parse("mcp.json").expect("path"),
            at_store::manifest::ContentHash::from_bytes(b"fixture"),
        );
        manifest.insert_item(item).expect("manifest item");
        fs::write(
            root.join("store.json"),
            manifest.to_bytes().expect("manifest bytes"),
        )
        .expect("seed manifest");
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: Some(project.to_path_buf()),
            variables: BTreeMap::new(),
        };
        // The native file still holds the entry, so the removal is planned for it.
        let target = McpTarget {
            item_id,
            ..target(
                AgentId::Codex,
                "/fixture/config.toml",
                "[mcp_servers.fixture]\ncommand = \"x\"\n",
            )
        };
        let intent = McpIntent::DisableInRepo {
            project: project.to_path_buf(),
        };
        let plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: std::slice::from_ref(&target),
            definition: &fixture(),
            scope: Scope::Project,
            project_id: Some("project-fixture".to_owned()),
            locked_reason: None,
            store_layout: None,
            target_store_layout: None,
        })
        .expect("tombstone plan");
        assert!(plan.native.is_none());
        let store = plan.store.expect("store ChangeSet");
        assert_eq!(store.ops.len(), 1);
        assert!(matches!(
            plan.status,
            McpPlanStatus::NotSupported {
                agent: AgentId::Codex,
                ..
            }
        ));
        assert!(!plan.warnings.is_empty());
        let FileOp::Write { path, bytes, .. } = &store.ops[0] else {
            panic!("manifest tombstone should be a write");
        };
        assert_eq!(path, &root.join("store.json"));
        let written = at_store::manifest::StoreManifest::parse(bytes).expect("updated manifest");
        assert!(written.disabled_for(item_id).is_some());
    }
    #[test]
    fn secret_policy_uses_verified_codex_env_vars_and_refuses_unverified_omp_refs() {
        let registry = AgentAdapterRegistry::register();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let mut definition = fixture();
        definition.transport = McpTransport::Stdio {
            command: "fixture-server".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::from([(
                "TOKEN".to_owned(),
                EnvValue::Secret(at_model::SecretRef {
                    env_name: "API_TOKEN".to_owned(),
                    keychain_account: "fixture-account".to_owned(),
                }),
            )]),
        };
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let targets = [
            target(AgentId::Codex, "/fixture/home/.codex/config.toml", ""),
            target(AgentId::Omp, "/fixture/home/.omp/agent/mcp.json", "{}"),
        ];
        let plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: &targets,
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        })
        .expect("secret plan");
        let native = plan.native.expect("native ChangeSet");
        let codex = native
            .ops
            .iter()
            .find(|op| {
                op.destination()
                    .is_some_and(|path| path.ends_with("config.toml"))
            })
            .expect("Codex target");
        let FileOp::Write { bytes, .. } = codex else {
            panic!("Codex projection should be a write");
        };
        let text = String::from_utf8_lossy(bytes);
        assert!(text.contains("env_vars = [\"API_TOKEN\"]"));
        assert!(!text.contains("fixture-account"));
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("refused secret"))
        );
        let warning = plan
            .warnings
            .iter()
            .find(|warning| warning.contains("refused secret"))
            .expect("refusal warning");
        assert!(warning.contains("export API_TOKEN=$(security find-generic-password"));
        assert!(warning.contains("fixture-account"));
        assert!(!warning.contains("SENTINEL"));
    }

    #[test]
    fn secret_shapes_per_agent_are_table_driven_and_never_contain_the_sentinel() {
        const SENTINEL: &str = "sk-sentinel-value-9f3c";
        let registry = AgentAdapterRegistry::register();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let mut definition = fixture();
        definition.transport = McpTransport::Stdio {
            command: "fixture-server".to_owned(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::from([(
                "TOKEN".to_owned(),
                EnvValue::Secret(at_model::SecretRef {
                    env_name: "API_TOKEN".to_owned(),
                    keychain_account: SENTINEL.to_owned(),
                }),
            )]),
        };
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let targets = [
            target(
                AgentId::ClaudeCode,
                "/fixture/home/.mcp.json",
                "{\"mcpServers\":{}}",
            ),
            target(AgentId::Codex, "/fixture/home/.codex/config.toml", ""),
            target(AgentId::Omp, "/fixture/home/.omp/agent/mcp.json", "{}"),
        ];
        let plan = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: &targets,
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(&store),
            target_store_layout: None,
        })
        .expect("secret plan");
        let native = plan.native.expect("native ChangeSet");
        let rendered = |suffix: &str| -> String {
            let operation = native
                .ops
                .iter()
                .find(|operation| {
                    operation
                        .destination()
                        .is_some_and(|path| path.ends_with(suffix))
                })
                .expect("one native write per target");
            let at_model::FileOp::Write { bytes, .. } = operation else {
                panic!("native secrets are written, never linked");
            };
            String::from_utf8(bytes.clone()).expect("native documents are UTF-8")
        };

        let claude = rendered(".mcp.json");
        assert!(
            claude.contains("\"TOKEN\": \"${API_TOKEN}\""),
            "Claude keeps the documented .mcp.json reference: {claude}"
        );

        let codex = rendered("config.toml");
        assert!(
            codex.contains("env_vars = [\"API_TOKEN\"]"),
            "Codex forwards the variable by name: {codex}"
        );
        assert!(
            !codex.contains("${"),
            "Codex must not promise interpolation: {codex}"
        );
        assert!(
            !codex.contains("TOKEN ="),
            "Codex has no env entry for the secret: {codex}"
        );

        let omp = rendered("mcp.json");
        assert!(
            !omp.contains("${"),
            "omp must not receive a reference: {omp}"
        );
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("API_TOKEN")
                    && warning.contains("security find-generic-password")
                    && warning.contains(SENTINEL)),
            "the refusal warning names the variable and the snippet: {:?}",
            plan.warnings
        );
        for (agent, document) in [("claude_code", &claude), ("codex", &codex), ("omp", &omp)] {
            assert!(
                !document.contains(SENTINEL),
                "{agent} wrote the secret sentinel: {document}"
            );
        }
    }

    fn three_agent_plan(
        store: &at_store::layout::StoreLayout,
        intent: &McpIntent,
        definition: &McpServerDef,
    ) -> McpPlan {
        plan_with_sources(
            store,
            intent,
            definition,
            &["{\"mcpServers\":{}}", "", "{}"],
        )
    }

    fn plan_with_sources(
        store: &at_store::layout::StoreLayout,
        intent: &McpIntent,
        definition: &McpServerDef,
        sources: &[&str; 3],
    ) -> McpPlan {
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let targets = [
            target(
                AgentId::ClaudeCode,
                "/fixture/home/.claude.json",
                sources[0],
            ),
            target(
                AgentId::Codex,
                "/fixture/home/.codex/config.toml",
                sources[1],
            ),
            target(
                AgentId::Omp,
                "/fixture/home/.omp/agent/mcp.json",
                sources[2],
            ),
        ];
        plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent,
            targets: &targets,
            definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: None,
            store_layout: Some(store),
            target_store_layout: None,
        })
        .expect("plan")
    }

    fn rendered(plan: &McpPlan, suffix: &str) -> String {
        let native = plan.native.as_ref().expect("native ChangeSet");
        let op = native
            .ops
            .iter()
            .find(|op| op.destination().is_some_and(|path| path.ends_with(suffix)))
            .expect("target file");
        let FileOp::Write { bytes, .. } = op else {
            panic!("native projection should be a write");
        };
        String::from_utf8(bytes.clone()).expect("UTF-8 native document")
    }

    #[test]
    fn add_renders_the_exact_native_bytes_for_every_agent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let definition = fixture();
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let plan = three_agent_plan(&store, &intent, &definition);
        let native = plan.native.as_ref().expect("native ChangeSet");
        let mut paths: Vec<_> = native
            .ops
            .iter()
            .filter_map(|op| op.destination().map(|path| path.to_path_buf()))
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/fixture/home/.claude.json"),
                PathBuf::from("/fixture/home/.codex/config.toml"),
                PathBuf::from("/fixture/home/.omp/agent/mcp.json"),
            ]
        );
        assert_eq!(
            rendered(&plan, ".claude.json"),
            "{\"mcpServers\":{\n    \"fixture\": {\n      \"type\": \"stdio\",\n      \"command\": \
             \"fixture-server\",\n      \"args\": [\"--safe\"]\n    }\n  }}"
        );
        assert_eq!(
            rendered(&plan, "config.toml"),
            "[mcp_servers.fixture]\ncommand = \"fixture-server\"\nargs = \
             [\"--safe\"]\nenv = {}\nenabled = true\n"
        );
        assert_eq!(
            rendered(&plan, "mcp.json"),
            "{\n  \"mcpServers\": {\n    \"fixture\": {\n      \"type\": \"stdio\",\n      \
             \"command\": \"fixture-server\",\n      \"args\": [\"--safe\"],\n      \
             \"enabled\": true\n    }\n  }\n}"
        );
    }

    #[test]
    fn disable_uses_native_flags_and_reenable_restores_identical_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store_layout(&temp);
        let definition = fixture();
        let add = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        let added = three_agent_plan(&store, &add, &definition);
        let original = [
            rendered(&added, ".claude.json"),
            rendered(&added, "config.toml"),
            rendered(&added, "mcp.json"),
        ];
        let sources = [&original[0][..], &original[1][..], &original[2][..]];
        let disable = |agent: AgentId| McpIntent::SetEnabledForAgent {
            agent,
            enabled: false,
        };
        let codex = plan_with_sources(&store, &disable(AgentId::Codex), &definition, &sources);
        assert!(rendered(&codex, "config.toml").contains("enabled = false"));
        let omp = plan_with_sources(&store, &disable(AgentId::Omp), &definition, &sources);
        assert!(rendered(&omp, "mcp.json").contains("\"enabled\": false"));
        let claude =
            plan_with_sources(&store, &disable(AgentId::ClaudeCode), &definition, &sources);
        let removed = rendered(&claude, ".claude.json");
        assert!(!removed.contains("\"fixture\""));
        for plan in [&codex, &omp] {
            let native = plan.native.as_ref().expect("native");
            assert_eq!(native.ops.len(), 1, "only the addressed agent changes");
        }
        let enable = McpIntent::SetEnabledForAgent {
            agent: AgentId::ClaudeCode,
            enabled: true,
        };
        let restored = plan_with_sources(
            &store,
            &enable,
            &definition,
            &[&removed, &original[1], &original[2]],
        );
        assert_eq!(rendered(&restored, ".claude.json"), original[0]);
    }

    #[test]
    fn lock_refusal_short_circuits_before_reading_any_target_or_store() {
        let registry = AgentAdapterRegistry::register();
        let temp = tempfile::tempdir().expect("tempdir");
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let definition = fixture();
        let intent = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: definition.clone(),
        };
        // Neither the canonical store nor any native document exists in this fixture; a plan
        // that reached either would fail with a store or adapter error instead of `Locked`.
        let missing_store =
            at_store::layout::StoreLayout::new(Scope::User, temp.path().join("absent-store"));
        let targets = [target(
            AgentId::Codex,
            "/fixture/home/.codex/config.toml",
            "",
        )];
        let result = plan(McpPlanRequest {
            registry: &registry,
            env: &env,
            intent: &intent,
            targets: &targets,
            definition: &definition,
            scope: Scope::User,
            project_id: None,
            locked_reason: Some("organization policy"),
            store_layout: Some(&missing_store),
            target_store_layout: None,
        });
        assert!(
            matches!(result, Err(McpPlanError::Locked(reason)) if reason == "organization policy")
        );
        assert!(!missing_store.root().exists());
    }

    /// Sandbox with a User, Project and Global store on separate roots.
    struct CopySandbox {
        _dir: tempfile::TempDir,
        paths: at_store::paths::AppPaths,
        project: PathBuf,
    }

    impl CopySandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = std::fs::canonicalize(dir.path()).expect("canonical root");
            let home = root.join("home");
            let support = root.join("support");
            let user_store = home.join(at_store::paths::STORE_DIR_NAME);
            let global = root.join("global");
            let project = root.join("project");
            for path in [&home, &support, &user_store, &global, &project] {
                std::fs::create_dir_all(path).expect("sandbox directory");
            }
            let paths = at_store::paths::AppPaths::resolve(&at_store::paths::PathEnvironment {
                home: at_store::paths::HomeSource::Injected(home),
                overrides: at_store::paths::PathOverrides {
                    home: Some(support),
                    user_store: Some(user_store),
                    project_store: None,
                    global_store: Some(global),
                },
                project_root: Some(project.clone()),
            })
            .expect("app paths");
            let sandbox = Self {
                _dir: dir,
                paths,
                project,
            };
            // A copy target needs an existing store; the User and Global stores are created on
            // demand by the fixtures that read them.
            let _ = sandbox.layout(Scope::Project);
            sandbox
        }

        fn project_paths(&self) -> at_store::paths::AppPaths {
            self.paths
                .for_project(&self.project)
                .expect("project paths")
        }

        fn layout(&self, scope: Scope) -> at_store::layout::StoreLayout {
            let paths = match scope {
                Scope::Project | Scope::Local => self.project_paths(),
                _ => self.paths.clone(),
            };
            let layout =
                at_store::layout::StoreLayout::for_scope(&paths, scope).expect("store layout");
            if !layout.is_ready() {
                at_store::layout::init_store(scope, layout.root()).expect("initialised store");
            }
            layout
        }

        /// Seeds one canonical definition and its manifest record.
        fn seed(&self, scope: Scope, slug: &str, definition: &McpServerDef) -> ItemId {
            let layout = self.layout(scope);
            let mut store = at_store::mcp_store::McpStore::load(&layout)
                .expect("loaded store")
                .store;
            store.set(slug, definition.clone());
            std::fs::write(layout.mcp_file(), store.to_bytes().expect("store bytes"))
                .expect("seed mcp.json");
            let mut manifest = at_store::manifest::StoreManifest::load(&layout)
                .expect("manifest")
                .manifest;
            let id = ItemId::new();
            let value = serde_json::to_value(definition).expect("definition json");
            let hash = at_store::manifest::hash_mcp_entry(&value).expect("entry hash");
            manifest
                .insert_item(
                    at_store::manifest::ManifestItem::new(
                        id,
                        at_model::ItemKind::McpServer,
                        at_store::layout::Slug::parse(slug).expect("slug"),
                        at_store::manifest::StorePath::parse("mcp.json").expect("path"),
                        hash,
                    )
                    .with_name(definition.display_name.clone()),
                )
                .expect("manifest item");
            std::fs::write(
                layout.manifest_path(),
                manifest.to_bytes().expect("manifest bytes"),
            )
            .expect("seed store.json");
            id
        }

        /// The project-root MCP document omp reads at Project scope.
        fn omp_project_document(&self) -> PathBuf {
            self.project.join("mcp.json")
        }

        fn copy(
            &self,
            item_id: ItemId,
            source: Scope,
            target_scope: Scope,
            on_collision: Option<McpCopyResolution>,
            installed: &[AgentId],
            locked_reason: Option<&str>,
        ) -> Result<McpItemPlan, McpPlanError> {
            let paths = self.project_paths();
            plan_item(McpItemPlanRequest {
                paths: &paths,
                scope: source,
                project_id: Some(String::from("project-sandbox")),
                item_id,
                action: McpItemAction::CopyToScope {
                    target_scope,
                    project_id: Some(String::from("project-sandbox")),
                    on_collision,
                },
                installed,
                deployed: &[],
                locked_reason,
            })
        }

        /// Plans enabling or disabling one server for `agent` at `scope`.
        fn set_enabled(
            &self,
            item_id: ItemId,
            scope: Scope,
            agent: AgentId,
            enabled: bool,
        ) -> Result<McpItemPlan, McpPlanError> {
            let paths = match scope {
                Scope::Project | Scope::Local => self.project_paths(),
                _ => self.paths.clone(),
            };
            plan_item(McpItemPlanRequest {
                paths: &paths,
                scope,
                project_id: None,
                item_id,
                action: McpItemAction::SetEnabled {
                    agent: agent.clone(),
                    enabled,
                },
                installed: &[agent],
                deployed: &[],
                locked_reason: None,
            })
        }
    }

    fn secret_definition() -> McpServerDef {
        let mut definition = fixture();
        definition.transport = McpTransport::Stdio {
            command: "fixture-server".to_owned(),
            args: vec!["--safe".to_owned()],
            cwd: None,
            env: BTreeMap::from([(
                "TOKEN".to_owned(),
                EnvValue::Secret(at_model::SecretRef {
                    env_name: "API_TOKEN".to_owned(),
                    keychain_account: "fixture-account".to_owned(),
                }),
            )]),
        };
        definition
    }

    fn native_text(plan: &McpItemPlan) -> String {
        let native = plan.plan.native.as_ref().expect("native ChangeSet");
        let FileOp::Write { bytes, .. } = &native.ops[0] else {
            panic!("native projection should be a write");
        };
        String::from_utf8(bytes.clone()).expect("UTF-8 native document")
    }

    #[test]
    fn a_project_copy_is_a_new_item_with_a_minimal_native_edit() {
        let sandbox = CopySandbox::new();
        let definition = fixture();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        let omp = sandbox.omp_project_document();
        std::fs::write(
            &omp,
            b"{\n  // kept by hand\n  \"theme\": \"dark\",\n  \"mcpServers\": {}\n}\n",
        )
        .expect("seed project document");

        let plan = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                None,
                &[AgentId::Omp],
                None,
            )
            .expect("project copy plan");
        let copy = plan.copy.as_ref().expect("copy summary");
        assert_eq!(copy.source_scope, Scope::User);
        assert_eq!(copy.target_scope, Scope::Project);
        assert_eq!(copy.target_slug, "fixture");
        assert_ne!(copy.target_item_id, source_id, "a copy is a new item");
        assert!(!copy.replaced);
        assert_eq!(copy.agents, vec![AgentId::Omp]);
        assert!(copy.refused_agents.is_empty());

        let store = plan.plan.store.as_ref().expect("destination store set");
        assert_eq!(store.scope, Some(Scope::Project));
        assert_eq!(store.ops.len(), 2, "mcp.json and its store.json record");
        let written = store
            .ops
            .iter()
            .find_map(|op| match op {
                FileOp::Write { path, bytes, .. } if path.ends_with("mcp.json") => {
                    Some(bytes.clone())
                }
                _ => None,
            })
            .expect("destination mcp.json write");
        let parsed = at_store::mcp_store::McpStore::parse(&written).expect("parsed store");
        assert_eq!(
            parsed.get("fixture"),
            Some(&definition),
            "the definition is copied by reference"
        );
        let manifest_bytes = store
            .ops
            .iter()
            .find_map(|op| match op {
                FileOp::Write { path, bytes, .. } if path.ends_with("store.json") => {
                    Some(bytes.clone())
                }
                _ => None,
            })
            .expect("manifest write");
        let manifest = at_store::manifest::StoreManifest::parse(&manifest_bytes).expect("manifest");
        let recorded = manifest
            .item_by_slug(
                at_model::ItemKind::McpServer,
                &at_store::layout::Slug::parse("fixture").expect("slug"),
            )
            .expect("manifest record");
        assert_eq!(recorded.id, copy.target_item_id);

        let native = plan.plan.native.as_ref().expect("native ChangeSet");
        assert_eq!(native.scope, Some(Scope::Project));
        assert_eq!(
            native.preview_id, store.preview_id,
            "one preview covers both phases"
        );
        let text = native_text(&plan);
        assert!(
            text.contains("// kept by hand"),
            "the comment survives: {text}"
        );
        assert!(
            text.contains("\"theme\": \"dark\""),
            "unrelated keys survive: {text}"
        );
        assert!(text.contains("\"fixture\""), "the copy is written: {text}");
        assert_eq!(
            plan.plan.merged().expect("one operation").ops.len(),
            3,
            "one apply and one undo cover the store and the native document"
        );
        assert_eq!(
            std::fs::read_to_string(&omp).expect("native document"),
            "{\n  // kept by hand\n  \"theme\": \"dark\",\n  \"mcpServers\": {}\n}\n",
            "planning writes nothing"
        );
    }

    #[test]
    fn a_secret_definition_is_copied_as_references_and_refused_per_agent() {
        let sandbox = CopySandbox::new();
        let definition = secret_definition();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        sandbox.layout(Scope::Global);

        let plan = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Global,
                None,
                &[AgentId::Codex, AgentId::Omp],
                None,
            )
            .expect("partial copy plan");
        let copy = plan.copy.as_ref().expect("copy summary");
        assert_eq!(copy.target_scope, Scope::Global);
        assert_ne!(copy.target_item_id, source_id, "a copy is a new item");
        assert_eq!(copy.agents, vec![AgentId::Codex]);
        assert_eq!(copy.refused_agents.len(), 1);
        assert_eq!(copy.refused_agents[0].agent, AgentId::Omp);
        assert!(
            copy.refused_agents[0].reason.contains("API_TOKEN"),
            "the refusal names the secret: {:?}",
            copy.refused_agents[0].reason
        );

        // The canonical copy keeps the reference; it never resolves a secret value.
        let store = plan.plan.store.as_ref().expect("destination store set");
        let written = store
            .ops
            .iter()
            .find_map(|op| match op {
                FileOp::Write { path, bytes, .. } if path.ends_with("mcp.json") => {
                    Some(bytes.clone())
                }
                _ => None,
            })
            .expect("destination mcp.json write");
        let parsed = at_store::mcp_store::McpStore::parse(&written).expect("parsed store");
        assert_eq!(
            parsed.get("fixture"),
            Some(&definition),
            "the store copy keeps the secret reference, never its value"
        );
        let native = native_text(&plan);
        assert!(
            native.contains("env_vars = [\"API_TOKEN\"]"),
            "Codex forwards the variable by name: {native}"
        );
        assert!(
            !native.contains("fixture-account"),
            "no secret material reaches a config: {native}"
        );
        assert!(
            matches!(
                sandbox.copy(
                    source_id,
                    Scope::User,
                    Scope::Global,
                    None,
                    &[AgentId::Omp],
                    None
                ),
                Err(McpPlanError::NoSupportedAgent { .. })
            ),
            "a copy no agent can load is refused instead of written"
        );
    }

    #[test]
    fn copy_refusals_are_typed_and_write_nothing() {
        let sandbox = CopySandbox::new();
        let definition = fixture();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        assert!(matches!(
            sandbox.copy(source_id, Scope::User, Scope::User, None, &[], None),
            Err(McpPlanError::SameScope)
        ));
        assert!(matches!(
            sandbox.copy(
                source_id,
                Scope::User,
                Scope::Project,
                None,
                &[],
                Some("organization policy")
            ),
            Err(McpPlanError::Locked(reason)) if reason == "organization policy"
        ));
        // Local MCP needs a registered project identity before anything is planned.
        let paths = sandbox.paths.clone();
        let no_project = plan_item(McpItemPlanRequest {
            paths: &paths,
            scope: Scope::User,
            project_id: None,
            item_id: source_id,
            action: McpItemAction::CopyToScope {
                target_scope: Scope::Project,
                project_id: None,
                on_collision: None,
            },
            installed: &[],
            deployed: &[],
            locked_reason: None,
        });
        assert!(matches!(
            no_project,
            Err(McpPlanError::MissingProject {
                scope: Scope::Project
            })
        ));
        // A destination scope without a store is planned against an empty store: the caller
        // merges the store skeleton into the same reviewed ChangeSet, so a first copy into a
        // fresh scope no longer needs a separate scope-setup step.
        let local = plan_item(McpItemPlanRequest {
            paths: &paths,
            scope: Scope::User,
            project_id: Some(String::from("project-sandbox")),
            item_id: source_id,
            action: McpItemAction::CopyToScope {
                target_scope: Scope::Local,
                project_id: Some(String::from("project-sandbox")),
                on_collision: None,
            },
            installed: &[],
            deployed: &[],
            locked_reason: None,
        });
        assert!(
            !matches!(local, Err(McpPlanError::DestinationStoreMissing { .. })),
            "an absent destination store is planned, not refused"
        );
    }

    #[test]
    fn a_different_project_definition_needs_replace_or_rename() {
        let sandbox = CopySandbox::new();
        let definition = fixture();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        let mut different = fixture();
        different.description = Some("project copy".to_owned());
        let project_id = sandbox.seed(Scope::Project, "fixture", &different);

        let error = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                None,
                &[AgentId::Omp],
                None,
            )
            .expect_err("a different definition needs a decision");
        assert!(
            matches!(&error, McpPlanError::Collision { slug, item_id, .. } if slug == "fixture" && *item_id == project_id),
            "{error:?}"
        );

        let replaced = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                Some(McpCopyResolution::Replace),
                &[AgentId::Omp],
                None,
            )
            .expect("replace plan");
        let copy = replaced.copy.as_ref().expect("copy summary");
        assert!(copy.replaced);
        assert_eq!(
            copy.target_item_id, project_id,
            "Replace keeps the destination identity"
        );

        let renamed = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                Some(McpCopyResolution::Rename(String::from("fixture-copy"))),
                &[AgentId::Omp],
                None,
            )
            .expect("rename plan");
        let copy = renamed.copy.as_ref().expect("copy summary");
        assert_eq!(copy.target_slug, "fixture-copy");
        assert_ne!(copy.target_item_id, project_id);

        let free = sandbox.seed(Scope::User, "free-fixture", &definition);
        assert!(matches!(
            sandbox.copy(
                free,
                Scope::User,
                Scope::Project,
                Some(McpCopyResolution::Replace),
                &[AgentId::Omp],
                None
            ),
            Err(McpPlanError::NoTargetToReplace(_))
        ));
        // The identical definition is already in the destination scope, so there is no copy to make.
        sandbox.seed(Scope::Project, "existing", &definition);
        let duplicate_source = sandbox.seed(Scope::User, "existing", &definition);
        assert!(matches!(
            sandbox.copy(
                duplicate_source,
                Scope::User,
                Scope::Project,
                None,
                &[AgentId::Omp],
                None
            ),
            Err(McpPlanError::DuplicateId(slug)) if slug == "existing"
        ));
    }

    #[test]
    fn a_native_entry_agen_tuna_does_not_own_is_never_overwritten() {
        let sandbox = CopySandbox::new();
        let definition = fixture();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        std::fs::write(
            sandbox.omp_project_document(),
            b"{\"mcpServers\":{\"fixture\":{\"command\":\"hand-written\"}}}",
        )
        .expect("seed hand-written native entry");

        let error = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                None,
                &[AgentId::Omp],
                None,
            )
            .expect_err("an unmanaged native entry is refused");
        assert!(
            matches!(&error, McpPlanError::UnmanagedNative { agent, slug, .. } if agent == &AgentId::Omp && slug == "fixture"),
            "{error:?}"
        );

        // The same document entry is replaced once the destination store owns the definition.
        let mut project_definition = fixture();
        project_definition.description = Some("managed in the project".to_owned());
        sandbox.seed(Scope::Project, "fixture", &project_definition);
        let replaced = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Project,
                Some(McpCopyResolution::Replace),
                &[AgentId::Omp],
                None,
            )
            .expect("replace plan");
        let text = native_text(&replaced);
        assert!(
            text.contains("fixture-server"),
            "Replace writes the managed definition: {text}"
        );
    }

    #[test]
    fn plan_summaries_read_as_user_copy_instead_of_planner_jargon() {
        let registry = at_adapters::adapter::AgentAdapterRegistry::register();
        let environment = at_adapters::adapter::AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: Default::default(),
        };
        let definition = fixture();
        let targets = vec![target(AgentId::ClaudeCode, "/fixture/claude.json", "{}")];
        fn request_for<'a>(
            registry: &'a at_adapters::adapter::AgentAdapterRegistry,
            environment: &'a at_adapters::adapter::AgentEnvironment,
            intent: &'a McpIntent,
            targets: &'a [McpTarget],
            definition: &'a McpServerDef,
        ) -> McpPlanRequest<'a> {
            McpPlanRequest {
                registry,
                env: environment,
                intent,
                targets,
                definition,
                scope: Scope::User,
                project_id: None,
                locked_reason: None,
                store_layout: None,
                target_store_layout: None,
            }
        }

        let enabled = McpIntent::SetEnabledForAgent {
            agent: AgentId::ClaudeCode,
            enabled: true,
        };
        let enabled_request = request_for(&registry, &environment, &enabled, &targets, &definition);
        assert_eq!(
            store_summary(&enabled_request),
            "Enabled MCP server Fixture server"
        );
        assert_eq!(
            native_summary(&enabled_request),
            "Enabled MCP server Fixture server for Claude Code"
        );
        assert!(
            !native_summary(&enabled_request).contains("Plan MCP"),
            "planner jargon is gone"
        );

        let disabled = McpIntent::SetEnabledForAgent {
            agent: AgentId::ClaudeCode,
            enabled: false,
        };
        let disabled_request =
            request_for(&registry, &environment, &disabled, &targets, &definition);
        assert_eq!(
            store_summary(&disabled_request),
            "Disabled MCP server Fixture server"
        );
        assert_eq!(
            native_summary(&disabled_request),
            "Disabled MCP server Fixture server for Claude Code"
        );

        let add = McpIntent::Add {
            slug: "fixture".to_owned(),
            definition: fixture(),
        };
        let add_request = request_for(&registry, &environment, &add, &targets, &definition);
        assert_eq!(
            store_summary(&add_request),
            "Added MCP server Fixture server"
        );
        assert_eq!(
            native_summary(&add_request),
            "Wrote MCP server Fixture server to Claude Code"
        );
    }

    #[test]
    fn enabling_and_disabling_a_server_follows_a_symlinked_claude_config() {
        use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};

        let sandbox = CopySandbox::new();
        let definition = fixture();
        let id = sandbox.seed(Scope::User, "fixture", &definition);
        let home = sandbox.paths.home_dir().expect("home");
        let target = home.join("dotfiles/claude.json");
        std::fs::create_dir_all(target.parent().expect("dotfiles")).expect("dotfiles dir");
        std::fs::write(&target, b"{\"mcpServers\":{}}\n").expect("seed dotfile");
        let link = home.join(".claude.json");
        std::os::unix::fs::symlink(&target, &link).expect("config symlink");
        let database = at_db::Database::open_in_memory().expect("database");
        let roots = ApplyRoots::new([home.to_path_buf()]).expect("authorized root");
        let backup_root = home.join("backups");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let user_layout = sandbox.layout(Scope::User);
        let mut context = ApplyContext {
            store_root: user_layout.root(),
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let enable = sandbox
            .set_enabled(id, Scope::User, AgentId::ClaudeCode, true)
            .expect("enable plan");
        apply_item_plan(&enable, &mut context, &roots).expect("enable applies");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("config link")
                .file_type()
                .is_symlink(),
            "the dotfile link survives the write"
        );
        let enabled = String::from_utf8(std::fs::read(&target).expect("dotfile bytes"))
            .expect("UTF-8 document");
        assert!(
            enabled.contains("\"fixture\""),
            "the target holds the enabled server: {enabled}"
        );

        let disable = sandbox
            .set_enabled(id, Scope::User, AgentId::ClaudeCode, false)
            .expect("disable plan");
        apply_item_plan(&disable, &mut context, &roots).expect("disable applies");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("config link")
                .file_type()
                .is_symlink(),
            "the dotfile link still survives"
        );
        let disabled = String::from_utf8(std::fs::read(&target).expect("dotfile bytes"))
            .expect("UTF-8 document");
        assert!(
            !disabled.contains("\"fixture\""),
            "the target no longer holds the server: {disabled}"
        );
    }

    #[test]
    fn a_store_only_plan_commits_its_journal() {
        use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};

        let sandbox = CopySandbox::new();
        let database = at_db::Database::open_in_memory().expect("database");
        let store_root = sandbox.layout(Scope::User).root().to_path_buf();
        std::fs::create_dir_all(&store_root).expect("store root");
        let home = sandbox.paths.home_dir().expect("home");
        let roots = ApplyRoots::new([home.to_path_buf()]).expect("authorized root");
        let backup_root = home.join("backups");
        let plan = McpItemPlan {
            plan: McpPlan {
                store: Some(ChangeSet {
                    id: "store-only".to_owned(),
                    preview_id: "preview".to_owned(),
                    operation_kind: OperationKind::parse("edit_mcp").expect("known kind"),
                    scope: Some(Scope::User),
                    project_id: None,
                    summary: "store-only plan".to_owned(),
                    ops: vec![FileOp::Write {
                        path: PathBuf::from("store-marker.json"),
                        bytes: b"{\"mcpServers\":{}}\n".to_vec(),
                        expected: ExpectedState::Absent,
                        follow_symlink: false,
                        backup: true,
                    }],
                    created_at_unix_seconds: 0,
                }),
                native: None,
                store_rollback: None,
                warnings: Vec::new(),
                status: McpPlanStatus::Ready,
            },
            slug: "fixture".to_owned(),
            agents: Vec::new(),
            skipped_agents: Vec::new(),
            copy: None,
        };
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store_root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let outcome = apply_item_plan(&plan, &mut context, &roots).expect("store-only apply");

        assert!(
            outcome.rollback_handle.is_none(),
            "the store phase is concluded inside the plan"
        );
        let journal_dir = store_root.join(".state/journal");
        let journals = std::fs::read_dir(&journal_dir)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(journals, 0, "a committed store phase leaves no journal");
    }

    #[test]
    fn a_plan_whose_native_documents_land_commits_its_store_journal() {
        use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};

        let sandbox = CopySandbox::new();
        let database = at_db::Database::open_in_memory().expect("database");
        let store_root = sandbox.layout(Scope::User).root().to_path_buf();
        std::fs::create_dir_all(&store_root).expect("store root");
        let home = sandbox.paths.home_dir().expect("home");
        let native_path = home.join("native.json");
        let roots = ApplyRoots::new([home.to_path_buf()]).expect("authorized root");
        let backup_root = home.join("backups");
        let plan = McpItemPlan {
            plan: McpPlan {
                store: Some(ChangeSet {
                    id: "store-phase".to_owned(),
                    preview_id: "preview".to_owned(),
                    operation_kind: OperationKind::parse("edit_mcp").expect("known kind"),
                    scope: Some(Scope::User),
                    project_id: None,
                    summary: "store phase".to_owned(),
                    ops: vec![FileOp::Write {
                        path: PathBuf::from("store-marker.json"),
                        bytes: b"{\"mcpServers\":{}}\n".to_vec(),
                        expected: ExpectedState::Absent,
                        follow_symlink: false,
                        backup: true,
                    }],
                    created_at_unix_seconds: 0,
                }),
                native: Some(ChangeSet {
                    id: "native-phase".to_owned(),
                    preview_id: "preview".to_owned(),
                    operation_kind: OperationKind::parse("edit_mcp").expect("known kind"),
                    scope: Some(Scope::User),
                    project_id: None,
                    summary: "native phase".to_owned(),
                    ops: vec![FileOp::Write {
                        path: native_path,
                        bytes: b"{\"mcpServers\":{}}\n".to_vec(),
                        expected: ExpectedState::Absent,
                        follow_symlink: false,
                        backup: true,
                    }],
                    created_at_unix_seconds: 0,
                }),
                store_rollback: None,
                warnings: Vec::new(),
                status: McpPlanStatus::Ready,
            },
            slug: "fixture".to_owned(),
            agents: Vec::new(),
            skipped_agents: Vec::new(),
            copy: None,
        };
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store_root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let outcome = apply_item_plan(&plan, &mut context, &roots).expect("two-phase apply");

        assert_eq!(outcome.completed, 1);
        let journal_dir = store_root.join(".state/journal");
        let journals = std::fs::read_dir(&journal_dir)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(journals, 0, "the committed store phase leaves no journal");
    }

    #[test]
    fn copying_into_global_edits_codex_toml_without_rewriting_unrelated_lines() {
        let sandbox = CopySandbox::new();
        let definition = fixture();
        let source_id = sandbox.seed(Scope::User, "fixture", &definition);
        sandbox.layout(Scope::Global);
        let codex = sandbox
            .paths
            .home_dir()
            .expect("home")
            .join(".codex/config.toml");
        std::fs::create_dir_all(codex.parent().expect("config dir")).expect("codex dir");
        std::fs::write(
            &codex,
            "# kept by hand\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .expect("seed codex config");

        let plan = sandbox
            .copy(
                source_id,
                Scope::User,
                Scope::Global,
                None,
                &[AgentId::Codex],
                None,
            )
            .expect("global copy plan");
        let text = native_text(&plan);
        assert!(
            text.contains("# kept by hand"),
            "the comment survives: {text}"
        );
        assert!(
            text.contains("model = \"gpt-5\""),
            "unrelated keys survive: {text}"
        );
        assert!(
            text.contains("[mcp_servers.other]"),
            "unrelated servers survive: {text}"
        );
        assert!(
            text.contains("[mcp_servers.fixture]"),
            "the copy is written: {text}"
        );
        let removed = text
            .lines()
            .filter(|line| line.starts_with('-') && !line.starts_with("---"))
            .count();
        assert_eq!(
            removed, 0,
            "a copy adds lines instead of reserializing: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&codex).expect("codex config"),
            "# kept by hand\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"other\"\n",
            "planning writes nothing"
        );
    }

    #[test]
    fn native_failure_rolls_back_the_store_phase_and_its_history() {
        use at_db::ops::{OperationStatus, operation_detail};
        use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};
        use at_migrate::undo::{UndoOperationRequest, UndoOutcome, undo_operation};
        use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};

        let sandbox = tempfile::tempdir().expect("tempdir");
        let store_root = sandbox.path().join("store");
        std::fs::create_dir_all(&store_root).expect("store dir");
        std::fs::write(store_root.join("mcp.json"), b"before").expect("seed store doc");
        let native = sandbox.path().join("home/.claude.json");
        std::fs::create_dir_all(native.parent().expect("native dir")).expect("native dir");
        std::fs::write(&native, b"native before").expect("seed native doc");
        let store_set = ChangeSet {
            id: "cs-store".to_owned(),
            preview_id: "preview-store".to_owned(),
            operation_kind: OperationKind::parse("edit_mcp").expect("kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "store phase".to_owned(),
            ops: vec![FileOp::Write {
                path: PathBuf::from("mcp.json"),
                bytes: b"after".to_vec(),
                expected: ExpectedState::hash(at_fs::sha256_hex(b"before")).expect("hash"),
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: 0,
        };
        let native_set = ChangeSet {
            id: "cs-native".to_owned(),
            preview_id: "preview-native".to_owned(),
            operation_kind: OperationKind::parse("edit_mcp").expect("kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "native phase".to_owned(),
            ops: vec![FileOp::Write {
                path: native.clone(),
                bytes: b"native after".to_vec(),
                expected: ExpectedState::hash(at_fs::sha256_hex(b"stale")).expect("hash"),
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: 0,
        };
        let item_plan = McpItemPlan {
            plan: McpPlan {
                store: Some(store_set),
                native: Some(native_set),
                store_rollback: None,
                warnings: Vec::new(),
                status: McpPlanStatus::Ready,
            },
            slug: "fixture".to_owned(),
            agents: Vec::new(),
            skipped_agents: Vec::new(),
            copy: None,
        };
        let database = at_db::Database::open_in_memory().expect("database");
        let authorized = ApplyRoots::new([sandbox.path().to_path_buf()]).expect("roots");
        let backup_root = sandbox.path().join("backups");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store_root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let error = apply_item_plan(&item_plan, &mut context, &authorized)
            .expect_err("the native document is stale");

        assert!(
            matches!(error, at_fs::ChangeSetApplyError::Stale(_)),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read(store_root.join("mcp.json")).expect("store doc"),
            b"before",
            "the completed store phase is rolled back"
        );
        assert_eq!(
            std::fs::read(&native).expect("native doc"),
            b"native before"
        );
        let operation: String = database
            .connection()
            .query_row(
                "SELECT id FROM operations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("operation row");
        let detail = operation_detail(database.connection(), &operation)
            .expect("detail")
            .expect("the operation exists");
        assert_eq!(detail.operation.status, Some(OperationStatus::Failed));
        assert_eq!(detail.operation.error_kind.as_deref(), Some("rolled_back"));
        assert!(!detail.files.is_empty(), "the store write was recorded");
        for file in &detail.files {
            assert_eq!(
                file.before_hash, file.after_hash,
                "row {} describes the restored state",
                file.id
            );
        }
        let undo = undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store_root,
                backup_root: &backup_root,
                authorized_roots: &authorized,
                actor: at_db::ops::OperationActor::Ui,
                force: false,
            },
            &operation,
        )
        .expect("undo runs");
        assert_eq!(undo, UndoOutcome::NotUndoable);
    }
}
