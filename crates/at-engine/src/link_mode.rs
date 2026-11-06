//! Per-agent deployment mode: Link (default) or Copy.
//!
//! Decision 18 of the technical-story contract: the setting lives in the TF-023 settings table
//! under exactly `link_mode.<agent>`; there is no `agent.<id>.deployment_mode` key. Known agents
//! default to [`LinkMode::Link`]; an agent created as custom defaults to [`LinkMode::Copy`]
//! (its `link_mode.<agent>` row is written when the agent is created, so this module needs no
//! special case at read time).
//!
//! Changing the mode edits future deployments only. [`set_link_mode`] returns a
//! [`MigrationOffer`] counting how many existing deployments are currently links; applying that
//! migration is a separate, user-confirmed operation and never happens implicitly.
//!
//! Persistence goes through the [`LinkModeStore`] seam so this crate stays free of rusqlite;
//! at-db implements it over `settings::set_link_mode` / `settings::load`, and the operations log
//! records old and new values with no file writes.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::RwLock;

use at_model::{AgentId, LinkMode};

/// Errors the link-mode operations can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LinkModeError {
    /// The store could not be read or written.
    #[error("link mode store: {message}")]
    Store {
        /// What went wrong, already redacted by the store implementation.
        message: String,
    },
    /// The store reported a value this build does not understand.
    #[error("stored link mode for `{agent}` is `{value}`; expected link or copy")]
    UnknownStoredValue {
        /// The agent whose row is bad.
        agent: AgentId,
        /// The stored text.
        value: String,
    },
}

/// Persistence seam for per-agent link modes.
///
/// Implementations must be atomic per call: [`LinkModeStore::set`] either stores the new mode
/// and its audit entry together, or stores nothing. Reads must reflect the most recent write,
/// including writes made by another process (the CLI and the GUI share the database).
pub trait LinkModeStore: Send + Sync {
    /// Every explicitly stored `link_mode.<agent>` row, keyed by agent.
    ///
    /// Absent agents fall back to their default; the store does not need to materialise
    /// defaults.
    fn load_all(&self) -> Result<BTreeMap<AgentId, LinkMode>, LinkModeError>;

    /// Persists `mode` for `agent`, replacing any previous value.
    ///
    /// Implementations record the change in the operations log with the old and new values and
    /// write no files.
    fn set(&self, agent: &AgentId, mode: LinkMode) -> Result<(), LinkModeError>;
}

/// In-process [`LinkModeStore`]. Modes last as long as the value.
///
/// Used by tests and by callers that run before the database exists. It is not a file: it
/// never touches disk and never sees another process's settings.
#[derive(Debug, Default)]
pub struct MemoryLinkModeStore {
    modes: RwLock<BTreeMap<AgentId, LinkMode>>,
}

impl MemoryLinkModeStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn poisoned() -> LinkModeError {
        LinkModeError::Store {
            message: "in-process link mode store was poisoned by a panicking writer".to_owned(),
        }
    }
}

impl LinkModeStore for MemoryLinkModeStore {
    fn load_all(&self) -> Result<BTreeMap<AgentId, LinkMode>, LinkModeError> {
        let modes = self.modes.read().map_err(|_| Self::poisoned())?;
        Ok(modes.clone())
    }

    fn set(&self, agent: &AgentId, mode: LinkMode) -> Result<(), LinkModeError> {
        let mut modes = self.modes.write().map_err(|_| Self::poisoned())?;
        modes.insert(agent.clone(), mode);
        Ok(())
    }
}

/// The default deployment mode for an agent with no stored `link_mode.<agent>` row.
///
/// Built-in agents default to Link; custom agents default to Copy. This mirrors the rule the
/// agent-creation path applies when it writes the explicit row, exposed here so callers that
/// only have an [`AgentId`] get the same answer without a database round-trip.
#[must_use]
pub fn default_link_mode(agent: &AgentId) -> LinkMode {
    match agent {
        AgentId::Custom(_) => LinkMode::Copy,
        _ => LinkMode::Link,
    }
}

/// The effective deployment mode for `agent`: its stored row, else [`default_link_mode`].
///
/// # Errors
/// [`LinkModeError::Store`] when the store cannot be read, or
/// [`LinkModeError::UnknownStoredValue`] when a row holds text this build rejects. An unknown
/// stored value leaves the previous effective value untouched: the error is returned before any
/// write, and the store row itself is preserved for the next load to warn about.
pub fn link_mode(store: &dyn LinkModeStore, agent: &AgentId) -> Result<LinkMode, LinkModeError> {
    Ok(store
        .load_all()?
        .get(agent)
        .copied()
        .unwrap_or_else(|| default_link_mode(agent)))
}

/// Every agent's effective mode, keyed by agent. Agents with no stored row are absent; callers
/// that need the full picture iterate their agent list and call [`link_mode`].
///
/// # Errors
/// [`LinkModeError::Store`] when the store cannot be read.
pub fn agent_settings(
    store: &dyn LinkModeStore,
) -> Result<BTreeMap<AgentId, LinkMode>, LinkModeError> {
    store.load_all()
}

/// What a mode switch would do to deployments that already exist.
///
/// Returned by [`set_link_mode`]; applying the migration is a separate user-confirmed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationOffer {
    /// How many of the agent's current deployments are materialised as links and would become
    /// copies (or stay links) after the switch.
    pub currently_linked: usize,
}

impl fmt::Display for MigrationOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let noun = if self.currently_linked == 1 {
            "deployment is"
        } else {
            "deployments are"
        };
        write!(
            f,
            "{} {noun} currently linked; switching mode does not rewrite them",
            self.currently_linked
        )
    }
}

/// Counts deployments for `agent` whose stored mode is a link (`symlink` or `junction`).
///
/// The count is non-blocking information for the [`MigrationOffer`]; it never writes.
pub fn count_linked_deployments(
    deployments: &[(AgentId, Option<String>)],
    agent: &AgentId,
) -> usize {
    deployments
        .iter()
        .filter(|(deployment_agent, mode)| {
            deployment_agent == agent
                && mode
                    .as_deref()
                    .is_some_and(|stored| matches!(stored, "symlink" | "junction"))
        })
        .count()
}

/// Sets the deployment mode for `agent` and reports what already-linked deployments exist.
///
/// The write goes through [`LinkModeStore::set`], which persists `link_mode.<agent>` in the
/// settings table and records the change in the operations log with old and new values. No
/// files are touched: existing deployments keep their current materialisation until a future
/// deploy or an explicit migration rewrites them.
///
/// # Errors
/// [`LinkModeError::Store`] when the read or write fails. A failure leaves the previous value
/// intact because the store writes atomically.
pub fn set_link_mode(
    store: &dyn LinkModeStore,
    agent: &AgentId,
    mode: LinkMode,
    deployments: &[(AgentId, Option<String>)],
) -> Result<MigrationOffer, LinkModeError> {
    // Read first so an unknown stored value fails before the write; the store's own parse
    // rejects unknown text on load, so this is defensive.
    let _previous = link_mode(store, agent)?;
    let offer = MigrationOffer {
        currently_linked: count_linked_deployments(deployments, agent),
    };
    store.set(agent, mode)?;
    Ok(offer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agents_default_to_link() {
        for agent in [
            AgentId::ClaudeCode,
            AgentId::Codex,
            AgentId::Omp,
            AgentId::Cursor,
        ] {
            assert_eq!(default_link_mode(&agent), LinkMode::Link);
        }
    }

    #[test]
    fn custom_agents_default_to_copy() {
        let agent = AgentId::custom("my-tool").expect("valid custom id");
        assert_eq!(default_link_mode(&agent), LinkMode::Copy);
    }

    #[test]
    fn set_persists_and_returns_offer() {
        let store = MemoryLinkModeStore::new();
        let agent = AgentId::Codex;
        let deployments = vec![
            (agent.clone(), Some("symlink".to_owned())),
            (agent.clone(), Some("copy".to_owned())),
            (agent.clone(), Some("junction".to_owned())),
            (AgentId::Omp, Some("symlink".to_owned())),
        ];
        let offer =
            set_link_mode(&store, &agent, LinkMode::Copy, &deployments).expect("set succeeds");
        assert_eq!(
            offer.currently_linked, 2,
            "symlink + junction for codex only"
        );
        assert_eq!(
            link_mode(&store, &agent).expect("read"),
            LinkMode::Copy,
            "mode persisted"
        );
    }

    #[test]
    fn switching_mode_does_not_rewrite_deployments() {
        let store = MemoryLinkModeStore::new();
        let agent = AgentId::ClaudeCode;
        // Simulate an existing linked deployment: the store has no row yet, the deployment
        // list reports a symlink. After set_link_mode(Copy), the deployment row is unchanged
        // (the function only writes settings).
        let deployments = vec![(agent.clone(), Some("symlink".to_owned()))];
        let offer = set_link_mode(&store, &agent, LinkMode::Copy, &deployments).expect("set");
        assert_eq!(offer.currently_linked, 1);
        // The deployment list passed in is immutable input; nothing rewrote it.
        assert_eq!(deployments[0].1.as_deref(), Some("symlink"));
    }

    #[test]
    fn agent_settings_returns_stored_rows() {
        let store = MemoryLinkModeStore::new();
        let codex = AgentId::Codex;
        store.set(&codex, LinkMode::Copy).expect("set");
        let settings = agent_settings(&store).expect("load");
        assert_eq!(settings.get(&codex), Some(&LinkMode::Copy));
        assert!(
            !settings.contains_key(&AgentId::Omp),
            "absent agents have no row"
        );
    }
}
