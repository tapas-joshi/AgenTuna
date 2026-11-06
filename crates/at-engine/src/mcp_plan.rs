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
    path::PathBuf,
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
            McpIntent::Add { slug, definition } | McpIntent::Edit { slug, definition } => {
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
            follow_symlink: target.agent == AgentId::ClaudeCode,
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let store = make_change_set(
        &store_ops,
        &preview_id,
        &operation_kind,
        &request,
        now,
        format!("Plan MCP {} (store)", intent_name(request.intent)),
    )?;
    let native = make_change_set(
        &native_ops,
        &preview_id,
        &operation_kind,
        &request,
        now,
        format!("Plan MCP {} (native)", intent_name(request.intent)),
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
    let mut loaded = at_store::mcp_store::McpStore::load(layout)?;
    match request.intent {
        McpIntent::Add { slug, definition } | McpIntent::Edit { slug, definition } => {
            loaded.store.set(slug.clone(), definition.clone());
        }
        McpIntent::Rename { from, to } => {
            if loaded.store.get(to).is_some() {
                return Err(McpPlanError::DuplicateId(to.clone()));
            }
            let definition = loaded
                .store
                .remove(from)
                .ok_or_else(|| McpPlanError::MissingStoreEntry(from.clone()))?;
            loaded.store.set(to.clone(), definition);
        }
        McpIntent::Remove => {
            let slug = request
                .targets
                .first()
                .map(|target| target.slug.as_str())
                .ok_or(McpPlanError::MissingStoreLayout)?;
            if loaded.store.remove(slug).is_none() {
                return Err(McpPlanError::MissingStoreEntry(slug.to_owned()));
            }
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
    if matches!(request.intent, McpIntent::Remove) {
        let target = request
            .targets
            .first()
            .ok_or(McpPlanError::MissingStoreLayout)?;
        let mut manifest = at_store::manifest::StoreManifest::load(layout)?;
        if manifest.manifest.remove_item(target.item_id).is_none() {
            return Err(McpPlanError::MissingStoreEntry(target.item_id.to_string()));
        }
        ops.push(FileOp::Write {
            path: layout.manifest_path(),
            bytes: manifest.manifest.to_bytes()?,
            expected: ExpectedState::hash(manifest.hash.as_str())?,
            follow_symlink: false,
            backup: true,
        });
    }
    Ok(ops)
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
fn intent_name(intent: &McpIntent) -> &'static str {
    match intent {
        McpIntent::Add { .. } => "add",
        McpIntent::Edit { .. } => "edit",
        McpIntent::Rename { .. } => "rename",
        McpIntent::SetEnabledForAgent { .. } => "toggle",
        McpIntent::DisableInRepo { .. } => "disable",
        McpIntent::Promote { .. } => "promote",
        McpIntent::MoveToScope { .. } => "move",
        McpIntent::Remove => "delete",
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

fn apply_secret_policy(
    agent: &AgentId,
    path: &std::path::Path,
    home: &std::path::Path,
    transport: &mut at_model::McpTransport,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let Some(capabilities) = capabilities(agent, home) else {
        return remove_unverified_secrets(agent, transport, warnings);
    };
    let mut env_vars = std::collections::BTreeSet::new();
    match transport {
        at_model::McpTransport::Stdio { env, .. } => {
            filter_secret_fields(env, agent, path, &capabilities, &mut env_vars, warnings);
        }
        at_model::McpTransport::Http { headers, .. } => {
            filter_secret_fields(headers, agent, path, &capabilities, &mut env_vars, warnings);
        }
    }
    env_vars.into_iter().collect()
}

fn remove_unverified_secrets(
    agent: &AgentId,
    transport: &mut at_model::McpTransport,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut remove_from = |fields: &mut std::collections::BTreeMap<String, EnvValue>| {
        let keys: Vec<_> = fields
            .iter()
            .filter_map(|(field, value)| {
                matches!(value, EnvValue::Secret(_)).then_some(field.clone())
            })
            .collect();
        for field in keys {
            fields.remove(&field);
            warnings.push(format!(
                "{agent}: refused secret field `{field}` because launch behavior is unverified"
            ));
        }
    };
    match transport {
        at_model::McpTransport::Stdio { env, .. } => remove_from(env),
        at_model::McpTransport::Http { headers, .. } => remove_from(headers),
    }
    Vec::new()
}

fn filter_secret_fields(
    fields: &mut std::collections::BTreeMap<String, EnvValue>,
    agent: &AgentId,
    path: &std::path::Path,
    capabilities: &at_adapters::mcp_capabilities::AgentMcpCapabilities,
    env_vars: &mut std::collections::BTreeSet<String>,
    warnings: &mut Vec<String>,
) {
    let keys: Vec<_> = fields.keys().cloned().collect();
    for key in keys {
        let Some(EnvValue::Secret(secret)) = fields.get(&key) else {
            continue;
        };
        match plan_secret_injection(
            capabilities,
            path,
            &secret.env_name,
            &secret.keychain_account,
        ) {
            SecretInjection::ConfigReference { .. } => {}
            SecretInjection::EnvironmentPassThrough { variable } => {
                fields.remove(&key);
                env_vars.insert(variable);
            }
            SecretInjection::Refused {
                variable,
                shell_snippet,
            } => {
                fields.remove(&key);
                warnings.push(format!(
                    "{agent}: refused secret `{variable}` for field `{key}`; {shell_snippet}"
                ));
            }
            _ => {
                fields.remove(&key);
                warnings.push(format!(
                    "{agent}: refused secret field `{key}` because launch policy is unknown"
                ));
            }
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
        use std::fs;
        let layout = at_store::layout::StoreLayout::new(Scope::User, temp.path());
        fs::create_dir_all(layout.root()).expect("store root");
        fs::write(
            layout.mcp_file(),
            at_store::mcp_store::McpStore::empty()
                .to_bytes()
                .expect("empty store"),
        )
        .expect("store file");
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
        assert_eq!(store.ops.len(), 1);
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
        assert_eq!(plan.store.expect("store").ops.len(), 1);
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
        let target = McpTarget {
            item_id,
            ..target(AgentId::Codex, "/fixture/config.toml", "")
        };
        let plan = plan(McpPlanRequest {
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

        let store_plan = plan.store.as_ref().expect("canonical removal");
        assert_eq!(store_plan.ops.len(), 2);
        assert!(plan.native.is_some());
        assert_eq!(plan.apply_order().count(), 2);
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
        let target = McpTarget {
            item_id,
            ..target(AgentId::Codex, "/fixture/config.toml", "")
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
    }
}
