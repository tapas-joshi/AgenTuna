//! Typed effective-configuration and why-trace query commands.

use std::collections::BTreeMap;
use std::str::FromStr;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_engine::effective::{AgentLoads, EffectiveQuery};
use at_engine::locks::{LockId, LockRecord, LockStore, MemoryLockStore};
use at_engine::resolve::{ResolveInput, resolve};
use at_model::{AgentId, ItemId, ItemKind, ItemState, Scope};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::{
    commands::{CommandError, CommandErrorCode},
    state::AppState,
};

/// Request filters for the effective-configuration query.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct EffectiveQueryRequest {
    /// Highest scope included in the query.
    pub scope: String,
    /// Optional item-kind filter (`skill`, `mcp_server`, or `rule`).
    pub kind: Option<String>,
    /// Optional agent filter.
    pub agent: Option<String>,
}

/// Effective configuration rows and recoverable store diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct EffectiveQueryView {
    /// Resolved effective items, sorted by kind then display name.
    pub items: Vec<at_engine::effective::EffectiveItem>,
    /// Items disabled here, sorted by kind, name, scope and agent.
    pub disabled_here: Vec<at_engine::effective::EffectiveDisabled>,
    /// Non-fatal lock/tombstone resolution warnings.
    pub diagnostics: Vec<at_engine::effective::EffectiveDiagnostic>,
    /// Store-entry diagnostics that did not prevent this query.
    pub load_diagnostics: Vec<at_engine::loader::LoadDiagnostic>,
}

/// Input to the scope explanation query.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct WhyTraceRequest {
    /// AgenTuna item identity.
    pub item_id: String,
    /// Scope selected in the application.
    pub scope: String,
    /// Agent whose effective state is explained.
    pub agent: String,
}

/// Why-trace result with recoverable loading diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct WhyTraceView {
    /// Four-scope explanation.
    pub trace: at_engine::why::WhyTrace,
    /// Store-entry diagnostics that did not prevent this query.
    pub load_diagnostics: Vec<at_engine::loader::LoadDiagnostic>,
}

/// Queries effective configuration through the shared engine projection.
#[tauri::command]
pub fn effective_query(
    state: State<'_, AppState>,
    request: EffectiveQueryRequest,
) -> Result<EffectiveQueryView, CommandError> {
    let scope = parse_scope(&request.scope)?;
    let kind = request.kind.as_deref().map(parse_kind).transpose()?;
    let agent_filter = request.agent.as_deref().map(parse_agent).transpose()?;
    let (loaded, active_project, project_id) = active_scopes(&state)?;
    let locks = load_locks(&state)?;
    let environment = adapter_environment(&state, active_project.as_deref())?;
    let observations =
        at_engine::loader::observe_agents(&AgentAdapterRegistry::register(), &environment)
            .map_err(|error| {
                CommandError::internal(format!("agent adapters unavailable: {error}"))
            })?;
    let mut agents = Vec::with_capacity(observations.len());
    for observation in observations {
        let deployments = at_db::deployments::list_deployments_for_agent(
            state
                .database()
                .map_err(CommandError::unavailable)?
                .connection(),
            &observation.agent,
        )
        .map_err(|error| {
            CommandError::internal(format!("cannot read agent deployments: {error}"))
        })?;
        let mut observed = BTreeMap::new();
        for definition in loaded.layers.iter().flat_map(|layer| &layer.definitions) {
            let deployment = deployments
                .iter()
                .filter(|row| Some(row.item_id) == definition.item_id)
                .filter(|row| row.scope.precedence() <= scope.precedence())
                .filter(|row| match row.scope {
                    Scope::Global | Scope::User => row.project_id.is_none(),
                    Scope::Project | Scope::Local => {
                        row.project_id.as_deref() == project_id.as_deref()
                    }
                })
                .max_by_key(|row| row.scope.precedence());
            if let Some(deployment) = deployment {
                observed.insert(definition.logical_id(), deployment.state);
            } else if observation.native.iter().any(|native| {
                native.kind == definition.kind
                    && at_model::normalized_name(&native.name) == definition.normalized_name()
            }) {
                observed.insert(definition.logical_id(), ItemState::Native);
            }
        }
        agents.push(AgentLoads {
            agent: observation.agent,
            installed: observation.installed,
            loaded: observed,
        });
    }
    let result = at_engine::effective::query(EffectiveQuery {
        scope,
        layers: &loaded.layers,
        agent_filter: agent_filter.as_ref(),
        kind_filter: kind,
        agents: &agents,
        locks: Some(&locks),
        tombstones: Some(&loaded.tombstones),
    });
    Ok(EffectiveQueryView {
        items: result.items,
        disabled_here: result.disabled_here,
        diagnostics: result.diagnostics,
        load_diagnostics: loaded.diagnostics,
    })
}

/// Explains one managed item through the engine's four-scope trace.
#[tauri::command]
pub fn why_trace(
    state: State<'_, AppState>,
    request: WhyTraceRequest,
) -> Result<WhyTraceView, CommandError> {
    let item_id = ItemId::from_str(&request.item_id)
        .map_err(|_| CommandError::invalid_argument("item_id", "expected a valid item id"))?;
    let active_scope = parse_scope(&request.scope)?;
    let agent = parse_agent(&request.agent)?;
    let (loaded, active_project, _) = active_scopes(&state)?;
    let locks = load_locks(&state)?;
    let environment = adapter_environment(&state, active_project.as_deref())?;
    let observations =
        at_engine::loader::observe_agents(&AgentAdapterRegistry::register(), &environment)
            .map_err(|error| {
                CommandError::internal(format!("agent adapters unavailable: {error}"))
            })?;
    let native = observations
        .iter()
        .find(|observation| observation.agent == agent)
        .map(|observation| observation.native.as_slice())
        .unwrap_or_default();
    let resolution = resolve(
        ResolveInput::new(&loaded.layers)
            .with_locks(&locks)
            .with_tombstones(&loaded.tombstones)
            .for_agent(&agent),
    );
    let mut evidence = Vec::with_capacity(4);
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        let definition = loaded
            .layers
            .iter()
            .find(|layer| layer.scope == scope)
            .and_then(|layer| {
                layer
                    .definitions
                    .iter()
                    .find(|definition| definition.item_id == Some(item_id))
            });
        let winner = resolution
            .items
            .iter()
            .find(|item| item.definition.item_id == Some(item_id));
        let shadowed = resolution
            .shadowed
            .iter()
            .any(|item| item.definition.item_id == Some(item_id));
        let disabled = resolution
            .disabled
            .iter()
            .any(|item| item.item_id == item_id && item.scope == scope);
        let lock_reason =
            winner.and_then(|item| item.lock.as_ref().map(|lock| lock.reason.clone()));
        let rendering_path = definition.and_then(|definition| {
            native
                .iter()
                .find(|item| {
                    item.kind == definition.kind
                        && at_model::normalized_name(&item.name) == definition.normalized_name()
                })
                .map(|item| item.path.clone())
        });
        let location = definition.map(|definition| at_engine::why::DocumentLocation {
            path: definition.path.clone(),
            byte_span: None,
            line: None,
            rendering_path,
        });
        evidence.push((
            scope,
            at_engine::why::TraceEvidence {
                defined: definition.is_some(),
                disabled,
                winning: winner.is_some_and(|item| item.scope == scope),
                shadowed,
                lock_reason,
                unreadable: None,
                location,
            },
        ));
    }
    let evidence: [(Scope, at_engine::why::TraceEvidence); 4] = evidence
        .try_into()
        .map_err(|_| CommandError::internal("could not assemble four-scope trace"))?;
    let trace = at_engine::why::trace(item_id, active_scope, agent, evidence);
    Ok(WhyTraceView {
        trace,
        load_diagnostics: loaded.diagnostics,
    })
}

fn active_scopes(
    state: &AppState,
) -> Result<
    (
        at_engine::loader::LoadedScopes,
        Option<std::path::PathBuf>,
        Option<String>,
    ),
    CommandError,
> {
    let project = {
        let database = state.database().map_err(CommandError::unavailable)?;
        let connection = database.connection();
        let settings = at_db::settings::load(connection).map_err(|error| {
            CommandError::internal(format!("cannot read application settings: {error}"))
        })?;
        match settings.last_project_id {
            Some(id) => {
                let project = at_db::projects::find_project_by_id(connection, &id)
                    .map_err(|error| {
                        CommandError::internal(format!("cannot read active project: {error}"))
                    })?
                    .ok_or_else(|| {
                        CommandError::unavailable("selected project no longer exists")
                    })?;
                Some((std::path::PathBuf::from(project.path), project.id))
            }
            None => None,
        }
    };
    let (project_path, project_id) = project
        .map(|(path, id)| (Some(path), Some(id)))
        .unwrap_or((None, None));
    let loaded = state
        .load_scope_snapshots(project_path.as_deref())
        .map_err(|error| CommandError::new(CommandErrorCode::Unavailable, error.to_string()))?;
    Ok((loaded, project_path, project_id))
}

fn load_locks(state: &AppState) -> Result<MemoryLockStore, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let rows = at_db::locks::list_locks(database.connection())
        .map_err(|error| CommandError::internal(format!("cannot read item locks: {error}")))?;
    let store = MemoryLockStore::new();
    for row in rows {
        let locked_at = time::OffsetDateTime::parse(
            &row.locked_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| CommandError::internal("stored lock timestamp is invalid"))?;
        store
            .put(LockRecord {
                id: LockId::new(),
                lock: at_model::Lock {
                    scope: row.scope,
                    item_id: row.item_id,
                    locked_at,
                    reason: row
                        .reason
                        .ok_or_else(|| CommandError::internal("stored lock has no reason"))?,
                    locked_by: row
                        .locked_by
                        .ok_or_else(|| CommandError::internal("stored lock has no owner"))?,
                },
            })
            .map_err(|error| CommandError::internal(format!("cannot load item locks: {error}")))?;
    }
    Ok(store)
}

fn adapter_environment(
    state: &AppState,
    project: Option<&std::path::Path>,
) -> Result<AgentEnvironment, CommandError> {
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?;
    Ok(AgentEnvironment {
        home: home.to_path_buf(),
        project: project.map(std::path::Path::to_path_buf),
        variables: BTreeMap::new(),
    })
}

fn parse_scope(value: &str) -> Result<Scope, CommandError> {
    Scope::from_str(value).map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))
}

fn parse_kind(value: &str) -> Result<ItemKind, CommandError> {
    ItemKind::from_str(value)
        .map_err(|_| CommandError::invalid_argument("kind", "unknown item kind"))
}

fn parse_agent(value: &str) -> Result<AgentId, CommandError> {
    AgentId::from_str(value).map_err(|_| CommandError::invalid_argument("agent", "unknown agent"))
}
