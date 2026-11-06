//! Effective configuration projection over caller-loaded scope and agent data.
//!
//! The caller loads the canonical definitions and adapter deployment observations; this module
//! owns scope resolution, per-agent tombstone handling, filtering and the serialized result shape.

use std::collections::BTreeMap;
use std::path::PathBuf;

use at_model::{AgentId, ItemId, ItemKind, ItemState, LogicalId, Scope, ScopeSnapshot};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::locks::{LockMark, LockStore};
use crate::resolve::{ResolveDiagnostic, ResolveInput, resolve};
use crate::tombstones::TombstoneSource;

/// Caller-provided observations for one installed agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoads {
    /// Agent identity.
    pub agent: AgentId,
    /// Whether the agent is installed on this machine.
    pub installed: bool,
    /// Items the adapter confirms this agent renders, keyed by logical identity.
    pub loaded: BTreeMap<LogicalId, ItemState>,
}

/// Inputs loaded by the caller from canonical stores, adapters and agent discovery.
pub struct EffectiveQuery<'a> {
    /// Highest scope included in the query; lower-precedence scopes are excluded.
    pub scope: Scope,
    /// Definitions loaded from the available scopes.
    pub layers: &'a [ScopeSnapshot],
    /// Optional single-agent filter.
    pub agent_filter: Option<&'a AgentId>,
    /// Optional item-kind filter.
    pub kind_filter: Option<ItemKind>,
    /// Installed and detected agents with adapter load observations.
    pub agents: &'a [AgentLoads],
    /// Current Global locks, when available.
    pub locks: Option<&'a dyn LockStore>,
    /// Scope tombstones, when available.
    pub tombstones: Option<&'a dyn TombstoneSource>,
}

/// One item's effective configuration and per-agent load state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveItem {
    /// Stable logical identity.
    #[ts(type = "string")]
    pub logical_id: LogicalId,
    /// AgenTuna item id when the winning definition is managed.
    #[ts(type = "string | null")]
    pub item_id: Option<ItemId>,
    /// Item kind.
    #[ts(type = "string")]
    pub kind: ItemKind,
    /// Display name from the winning definition.
    pub name: String,
    /// Scope that supplied the winning definition.
    #[ts(type = "string")]
    pub winner_scope: Scope,
    /// Path from which the winning definition was loaded.
    pub source_path: PathBuf,
    /// Agent load and state details, ordered by agent identity.
    pub agents: Vec<EffectiveAgentState>,
    /// Number of installed agents that render this item.
    pub loads_in_agents: usize,
    /// Lock identity and reason, when the winner is locked.
    pub lock: Option<EffectiveLock>,
}

/// One agent's effective load state for an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveAgentState {
    /// Agent identity.
    #[ts(type = "string")]
    pub agent: AgentId,
    /// State recorded by the adapter observation, or `Off` when not loaded.
    #[ts(type = "string")]
    pub state: ItemState,
    /// Whether the item is loaded by this agent.
    pub loads: bool,
}

/// Lock metadata shown with an effective item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveLock {
    /// Opaque lock row identity.
    pub id: String,
    /// Recorded lock explanation.
    pub reason: String,
}

/// A disabled item entry and the tombstone's known provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveDisabled {
    /// Disabled logical identity.
    #[ts(type = "string")]
    pub logical_id: LogicalId,
    /// Item id named by the tombstone.
    #[ts(type = "string")]
    pub item_id: ItemId,
    /// Item kind.
    #[ts(type = "string")]
    pub kind: ItemKind,
    /// Display name of the disabled definition.
    pub name: String,
    /// Scope that recorded the tombstone.
    #[ts(type = "string")]
    pub scope: Scope,
    /// Agent the tombstone applies to in this result; `None` for an unfiltered query
    /// when no installed agents are available.
    #[ts(type = "string | null")]
    pub agent: Option<AgentId>,
    /// Caller-supplied explanation, absent when not stored/provided.
    pub reason: Option<String>,
    /// Store manifest path, absent when not provided by the source.
    pub tombstone_path: Option<PathBuf>,
}

/// Effective configuration rows and tombstones for the requested scope/agent/kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveConfig {
    /// Resolved effective items, sorted by kind then display name.
    pub items: Vec<EffectiveItem>,
    /// Items disabled here, sorted by kind, name, scope and agent.
    pub disabled_here: Vec<EffectiveDisabled>,
    /// Non-fatal lock/tombstone resolution warnings.
    pub diagnostics: Vec<EffectiveDiagnostic>,
}

/// Resolution warning retained in the effective-config payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EffectiveDiagnostic {
    /// A stable diagnostic kind.
    pub kind: String,
    /// Scope for a scope-specific warning.
    #[ts(type = "string | null")]
    pub scope: Option<Scope>,
    /// Redacted source error text when provided by the resolver.
    pub message: String,
}

/// Resolves definitions and joins them with the caller's verified per-agent load observations.
///
/// Resolution is repeated per installed agent so per-agent tombstones are applied accurately.
/// Missing agent observations are never inferred as loaded. The caller should provide the exact
/// adapter-confirmed load set and only definitions from the stores participating in this scope.
#[must_use]
pub fn query(input: EffectiveQuery<'_>) -> EffectiveConfig {
    let layers: Vec<ScopeSnapshot> = input
        .layers
        .iter()
        .filter(|layer| layer.scope.precedence() <= input.scope.precedence())
        .cloned()
        .collect();
    let agents: Vec<&AgentLoads> = input
        .agents
        .iter()
        .filter(|agent| agent.installed)
        .filter(|agent| {
            input
                .agent_filter
                .is_none_or(|filter| &agent.agent == filter)
        })
        .collect();
    let mut resolved_by_id: BTreeMap<LogicalId, crate::resolve::ResolvedItem> = BTreeMap::new();
    let mut states_by_id: BTreeMap<LogicalId, Vec<EffectiveAgentState>> = BTreeMap::new();
    let mut disabled_here = Vec::new();
    let mut diagnostics = Vec::new();

    let contexts: Vec<Option<&AgentLoads>> = if agents.is_empty() && input.agent_filter.is_none() {
        vec![None]
    } else {
        agents.iter().copied().map(Some).collect()
    };
    for context in contexts {
        let mut resolve_input = ResolveInput::new(&layers);
        if let Some(agent) = context {
            resolve_input = resolve_input.for_agent(&agent.agent);
        }
        if let Some(locks) = input.locks {
            resolve_input = resolve_input.with_locks(locks);
        }
        if let Some(tombstones) = input.tombstones {
            resolve_input = resolve_input.with_tombstones(tombstones);
        }
        let resolution = resolve(resolve_input);
        for diagnostic in &resolution.diagnostics {
            let (kind, scope, message) = match diagnostic {
                ResolveDiagnostic::LockReadFailed { message } => {
                    ("lock_read_failed".to_owned(), None, message.clone())
                }
                ResolveDiagnostic::TombstoneReadFailed { scope, message } => (
                    "tombstone_read_failed".to_owned(),
                    Some(*scope),
                    message.clone(),
                ),
            };
            let row = EffectiveDiagnostic {
                kind,
                scope,
                message,
            };
            if !diagnostics.contains(&row) {
                diagnostics.push(row);
            }
        }
        for item in resolution.items {
            if input
                .kind_filter
                .is_some_and(|kind| kind != item.logical_id.kind)
            {
                continue;
            }
            if let Some(agent) = context {
                let loaded_state = agent.loaded.get(&item.logical_id).copied();
                states_by_id
                    .entry(item.logical_id.clone())
                    .or_default()
                    .push(EffectiveAgentState {
                        agent: agent.agent.clone(),
                        state: loaded_state.unwrap_or(ItemState::Off),
                        loads: loaded_state.is_some(),
                    });
            }
            resolved_by_id
                .entry(item.logical_id.clone())
                .or_insert(item);
        }
        for disabled in resolution.disabled {
            if input
                .kind_filter
                .is_some_and(|kind| kind != disabled.logical_id.kind)
            {
                continue;
            }
            if context.is_some_and(|agent| !disabled.agents.contains(&agent.agent)) {
                continue;
            }
            disabled_here.push(EffectiveDisabled {
                logical_id: disabled.logical_id,
                item_id: disabled.item_id,
                kind: disabled.definition.kind,
                name: disabled.definition.name,
                scope: disabled.scope,
                agent: context.map(|agent| agent.agent.clone()),
                reason: disabled.reason,
                tombstone_path: disabled.tombstone_path,
            });
        }
    }
    for logical_id in resolved_by_id.keys() {
        let states = states_by_id.entry(logical_id.clone()).or_default();
        for agent in &agents {
            if !states.iter().any(|state| state.agent == agent.agent) {
                states.push(EffectiveAgentState {
                    agent: agent.agent.clone(),
                    state: ItemState::Off,
                    loads: false,
                });
            }
        }
        states.sort_by(|left, right| left.agent.cmp(&right.agent));
    }

    let mut items = Vec::with_capacity(resolved_by_id.len());
    for (logical_id, item) in resolved_by_id {
        let states = states_by_id.remove(&logical_id).unwrap_or_default();
        let loads_in_agents = states.iter().filter(|state| state.loads).count();
        items.push(EffectiveItem {
            kind: logical_id.kind,
            logical_id,
            item_id: item.definition.item_id,
            name: item.definition.name,
            winner_scope: item.scope,
            source_path: item.definition.path,
            agents: states,
            loads_in_agents,
            lock: item.lock.map(|lock: LockMark| EffectiveLock {
                id: lock.id.to_string(),
                reason: lock.reason,
            }),
        });
    }
    items.sort_by_cached_key(|item| (item.kind, item.name.to_lowercase(), item.name.clone()));
    disabled_here.sort_by_cached_key(|entry| {
        (
            entry.kind,
            entry.name.to_lowercase(),
            entry.scope.precedence(),
            entry.agent.clone(),
        )
    });
    EffectiveConfig {
        items,
        disabled_here,
        diagnostics,
    }
}
