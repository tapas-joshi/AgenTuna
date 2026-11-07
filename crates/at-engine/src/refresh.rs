//! Derived agent-registry refresh shared by the desktop app and the CLI (TA-040, TA-070).
//!
//! The `agents` and `agent_paths` rows are a derived index rebuilt from the injected machine
//! context, never user data: detection upserts one row per built-in adapter and records every
//! probed candidate path. Keeping the write path here — instead of in the Tauri shell — lets the
//! app watcher, `migrate apply`, `adopt`, `migrate scan` and the CLI read commands all record
//! exactly
//! the same detection result, so the GUI and CLI cannot disagree about what is installed.
//!
//! `managed` is deliberately untouched: [`upsert_agent`] preserves the user's "let AgenTuna
//! manage this agent" choice across every detection pass.

use std::collections::BTreeMap;
use std::path::Path;

use at_adapters::adapter::{
    AgentAdapterRegistry, AgentDetection, AgentDetectionWithVersion, AgentEnvironment, AgentError,
    AgentPathCandidate, DetectionState,
};
use at_db::agents::{
    AgentPathFormat, AgentPathKind, AgentPathProbeSpec, AgentPresence, list_agents_on_mac,
    record_agent_path_probe, upsert_agent,
};
use at_model::{Agent, AgentId};
use at_store::paths::AppPaths;
use rusqlite::Connection;

/// One adapter detection result ready for persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgent {
    /// Stable agent identity reported by the adapter.
    pub id: AgentId,
    /// Display name used by the Agents screen and CLI table.
    pub display_name: String,
    /// Parsed version when a caller supplied a probe; detection alone leaves it absent.
    pub version: Option<String>,
    /// `true` only when the adapter found the agent itself, matching the Agents screen's
    /// distinction between `Installed` and `ConfiguredOnly`.
    pub installed: bool,
    /// `true` when the caller also probed the executable, making [`Self::installed`] and
    /// [`Self::version`] authoritative. A filesystem-only pass never downgrades values that an
    /// earlier probed pass recorded.
    pub probed: bool,
    /// Ordered candidate paths reported by the adapter, including missing ones.
    pub candidates: Vec<AgentPathCandidate>,
}

impl DetectedAgent {
    /// Builds the authoritative persistence record from the app's detection pass.
    #[must_use]
    pub fn from_detection(agent: &AgentId, detection: &AgentDetectionWithVersion) -> Self {
        let mut detected =
            Self::from_presence(agent, &detection.presence, detection.version.clone());
        detected.probed = true;
        detected
    }

    /// Builds the persistence record from a bare presence detection.
    #[must_use]
    pub fn from_presence(
        agent: &AgentId,
        presence: &AgentDetection,
        version: Option<String>,
    ) -> Self {
        Self {
            id: agent.clone(),
            display_name: agent.to_string(),
            version,
            installed: matches!(presence.state, DetectionState::Installed),
            probed: false,
            candidates: presence.candidates.clone(),
        }
    }
}

/// Failure while refreshing the derived agent registry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RefreshError {
    /// The adapter registry could not enumerate adapters.
    #[error(transparent)]
    Adapter(#[from] AgentError),
    /// Persisting the detected rows failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
}

/// Builds the machine context detection uses from the resolved application paths.
///
/// `PATH` is passed through for version probes; detection itself only reads filesystem
/// candidates, so a missing `PATH` still yields a complete registry.
#[must_use]
pub fn agent_environment(paths: &AppPaths) -> AgentEnvironment {
    let home = paths
        .home_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.user_store().to_path_buf());
    let project = paths
        .project_store()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let mut variables = BTreeMap::new();
    if let Ok(value) = std::env::var("PATH") {
        variables.insert(String::from("PATH"), value);
    }
    AgentEnvironment {
        home,
        project,
        variables,
    }
}

/// Detects every registered adapter without running an external process.
///
/// # Errors
/// Returns a typed adapter error when the registry cannot be enumerated.
pub fn detect_agents(
    registry: &AgentAdapterRegistry,
    environment: &AgentEnvironment,
) -> Result<Vec<DetectedAgent>, AgentError> {
    let mut detected = Vec::new();
    for adapter in registry.adapters()? {
        let presence = adapter.detect(environment);
        detected.push(DetectedAgent::from_presence(&adapter.id(), &presence, None));
    }
    Ok(detected)
}

/// Persists one detection pass and returns how many agent rows were written.
///
/// A filesystem-only pass (`probed = false`) never downgrades an agent that a probed pass
/// recorded as installed, and keeps the probed version. The app's rescan passes probed results,
/// so uninstalling an agent still clears it there; the CLI can only add evidence.
///
/// # Errors
/// Returns the first database failure; an unrecognized candidate kind is skipped because an
/// adapter may describe a surface this schema does not store yet.
pub fn persist_detected_agents(
    connection: &Connection,
    detected: &[DetectedAgent],
) -> Result<usize, at_db::DbError> {
    let existing = list_agents_on_mac(connection)?;
    let mut written = 0;
    for agent in detected {
        let prior = existing.iter().find(|row| row.id == agent.id);
        let (installed, version) = if agent.probed {
            (agent.installed, agent.version.clone())
        } else {
            (
                agent.installed
                    || prior.is_some_and(|row| row.presence == AgentPresence::Installed),
                agent
                    .version
                    .clone()
                    .or_else(|| prior.and_then(|row| row.version.clone())),
            )
        };
        let record = Agent {
            id: agent.id.clone(),
            display_name: agent.display_name.clone(),
            detected: installed,
            version,
        };
        upsert_agent(connection, &record)?;
        for candidate in &agent.candidates {
            persist_candidate(connection, &agent.id, candidate)?;
        }
        written += 1;
    }
    Ok(written)
}

/// Detects and records every registered adapter for the supplied machine context.
///
/// # Errors
/// Returns a typed adapter or database failure.
pub fn refresh_agents_from_environment(
    connection: &Connection,
    registry: &AgentAdapterRegistry,
    environment: &AgentEnvironment,
) -> Result<usize, RefreshError> {
    let detected = detect_agents(registry, environment)?;
    Ok(persist_detected_agents(connection, &detected)?)
}

/// Detects and records every registered adapter using [`AppPaths`].
///
/// This is the single refresh entry shared by the app watcher, the migration and adoption
/// commands, `migrate scan` and the CLI read commands that report agent state.
///
/// # Errors
/// Returns a typed adapter or database failure.
pub fn refresh_agents(connection: &Connection, paths: &AppPaths) -> Result<usize, RefreshError> {
    let registry = AgentAdapterRegistry::register();
    let environment = agent_environment(paths);
    refresh_agents_from_environment(connection, &registry, &environment)
}

/// Records one candidate path with its observed usability.
///
/// The row is kept when the path is missing (`exists = 0`) and marked unverified, so the Agents
/// screen can show the last known location and offer `Set path` instead of forgetting the agent.
fn persist_candidate(
    connection: &Connection,
    agent: &AgentId,
    candidate: &AgentPathCandidate,
) -> Result<(), at_db::DbError> {
    let Some(kind) = AgentPathKind::parse(&candidate.kind) else {
        return Ok(());
    };
    let metadata = std::fs::metadata(&candidate.path);
    let exists = metadata.is_ok();
    let verified = metadata.is_ok_and(|metadata| {
        if kind == AgentPathKind::SkillsDir || candidate.format.as_deref() == Some("dir") {
            metadata.is_dir() && std::fs::read_dir(&candidate.path).is_ok()
        } else {
            metadata.is_file() && std::fs::File::open(&candidate.path).is_ok()
        }
    });
    let format = candidate.format.as_deref().and_then(AgentPathFormat::parse);
    record_agent_path_probe(
        connection,
        &AgentPathProbeSpec {
            agent,
            scope: candidate.scope,
            kind,
            path: &candidate.path,
            format,
            is_primary: candidate.primary,
            exists,
            verified,
            is_override: false,
        },
    )?;
    Ok(())
}
