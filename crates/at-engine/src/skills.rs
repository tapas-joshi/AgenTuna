//! Skills list composition over indexed canonical rows and live native discovery.

use std::collections::BTreeMap;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment, AgentError};
use at_db::skills_index::{SkillFilter, SkillRow, list_skills};
use at_model::{AgentId, DeploymentState, Scope};
use rusqlite::Connection;
use thiserror::Error;

/// Per-agent state displayed by the Skills index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillCellState {
    /// AgenTuna owns a deployment for this agent.
    Linked,
    /// The canonical skill is not deployed to this installed agent.
    NotLinked,
    /// A skill was discovered in the agent's native folder.
    Native {
        /// Exact native skill folder path.
        path: PathBuf,
    },
    /// No skill or deployment exists for this agent.
    None,
    /// A Global lock denies changes to the item.
    Locked {
        /// Reason stored with the lock.
        reason: String,
    },
    /// A closer canonical scope wins for the active project.
    Shadowed {
        /// Higher-precedence winning scope when the index contains it.
        by: Option<Scope>,
    },
    /// Agent detection or native discovery failed.
    Error {
        /// Readable discovery or deployment error.
        message: String,
        /// Action the user can take to resolve the error.
        remediation: String,
    },
}

/// One composed row for the Skills screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillIndexRow {
    /// Database projection for canonical fields and indexed deployment state.
    pub skill: SkillRow,
    /// Per-agent state, augmented by live native paths.
    pub agents: BTreeMap<AgentId, SkillCellState>,
}

/// Primary status badge, with scope shadowing taking precedence over source updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillBadge {
    /// A higher-precedence canonical copy wins.
    Shadowed,
    /// A tracked source has a newer commit.
    UpdateAvailable,
    /// No exceptional status applies.
    Normal,
}

impl SkillIndexRow {
    /// Returns the single primary badge, preferring Shadowed over Update available.
    #[must_use]
    pub fn badge(&self) -> SkillBadge {
        if self.skill.shadowed_by.is_some() {
            SkillBadge::Shadowed
        } else if self.skill.update_available {
            SkillBadge::UpdateAvailable
        } else {
            SkillBadge::Normal
        }
    }
}

/// Complete Skills index response with filter chip counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsIndex {
    /// Sorted canonical and live native rows.
    pub rows: Vec<SkillIndexRow>,
    /// Number of rows matching each list chip.
    pub counts: SkillCounts,
}

/// Counts used by the Skills screen filters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkillCounts {
    /// All canonical and live native rows.
    pub all: usize,
    /// Native-only rows.
    pub not_in_agentuna: usize,
    /// Canonical copies shadowed by a closer scope.
    pub shadowed: usize,
}

/// Failure while loading indexed skill data or discovering an adapter's native skills.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillsIndexError {
    /// The SQLite projection could not be read.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// The adapter registry failed.
    #[error(transparent)]
    Adapter(#[from] AgentError),
}

/// Composes canonical skill rows with a single live native scan per registered built-in adapter.
///
/// The DB owns canonical/source/deployment state; adapter paths are read live so deleted roots
/// and native discoveries do not appear as stale database truth. Caller-owned locks are supplied
/// as row metadata by the effective-config query and can be applied by callers before rendering.
///
/// # Errors
/// Returns a typed database or adapter failure; a disappeared path is represented as an error
/// cell instead of aborting the complete index.
pub fn index(
    connection: &Connection,
    filter: &SkillFilter,
    adapters: &AgentAdapterRegistry,
    env: &AgentEnvironment,
    store_roots: &[PathBuf],
) -> Result<SkillsIndex, SkillsIndexError> {
    let locked = at_db::locks::list_locks(connection)?
        .into_iter()
        .map(|lock| {
            (
                lock.item_id,
                lock.reason
                    .unwrap_or_else(|| "No reason recorded".to_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut query_filter = filter.clone();
    query_filter.include_native = false;
    query_filter.not_in_agentuna = false;
    query_filter.shadowed_only = false;
    let mut rows = list_skills(connection, &query_filter)?
        .into_iter()
        .map(|skill| {
            let agents = skill
                .deployed
                .iter()
                .map(|(agent, state)| {
                    let locked_reason = skill
                        .item_id
                        .as_ref()
                        .and_then(|item_id| locked.get(item_id));
                    let cell = locked_reason
                        .map(|reason| SkillCellState::Locked {
                            reason: reason.clone(),
                        })
                        .unwrap_or_else(|| {
                            cell_from_deployment(
                                *state,
                                skill.shadowed_by,
                                skill.native_path.as_deref(),
                            )
                        });
                    (agent.clone(), cell)
                })
                .collect();
            SkillIndexRow { skill, agents }
        })
        .collect::<Vec<_>>();

    let registered_adapters = adapters.adapters()?;
    for adapter in registered_adapters {
        let agent_id = adapter.id();
        let native = match adapter.list_native_skills(env, store_roots) {
            Ok(native) => native,
            Err(
                error @ (AgentError::Io { .. }
                | AgentError::Permission { .. }
                | AgentError::NotInstalled { .. }),
            ) => {
                let path = match &error {
                    AgentError::Io { path, .. }
                    | AgentError::Permission { path }
                    | AgentError::NotInstalled { path, .. } => path.clone(),
                    _ => PathBuf::new(),
                };
                if filter
                    .agent
                    .as_ref()
                    .is_none_or(|wanted| wanted == &agent_id)
                {
                    let failed = SkillRow {
                        item_id: None,
                        name: agent_id.to_string(),
                        scope: filter.scope.unwrap_or(Scope::User),
                        state: Some("error".to_owned()),
                        shadowed_by: None,
                        deployed: BTreeMap::new(),
                        native_path: Some(path.to_string_lossy().into_owned()),
                        update_available: false,
                        updated_at: String::new(),
                        description: None,
                    };
                    rows.push(SkillIndexRow {
                        skill: failed,
                        agents: BTreeMap::from([(
                            agent_id.clone(),
                            SkillCellState::Error {
                                message: error.to_string(),
                                remediation:
                                    "Rescan after restoring access to the agent skills folder."
                                        .to_owned(),
                            },
                        )]),
                    });
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        for native in native {
            if filter.scope.is_some_and(|scope| scope != native.scope)
                || filter
                    .agent
                    .as_ref()
                    .is_some_and(|wanted| wanted != &agent_id)
                || filter.text.as_ref().is_some_and(|text| {
                    !native.name.to_lowercase().contains(&text.to_lowercase())
                        && !native
                            .path
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&text.to_lowercase())
                })
            {
                continue;
            }
            let mut deployed = BTreeMap::new();
            deployed.insert(agent_id.clone(), DeploymentState::Native);
            let updated_at = native
                .mtime
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().to_string())
                .unwrap_or_default();
            rows.push(SkillIndexRow {
                skill: SkillRow {
                    item_id: None,
                    name: native.name,
                    scope: native.scope,
                    state: Some(if native.valid { "native" } else { "error" }.to_owned()),
                    shadowed_by: None,
                    deployed,
                    native_path: Some(native.path.to_string_lossy().into_owned()),
                    update_available: false,
                    updated_at,
                    description: None,
                },
                agents: BTreeMap::from([(
                    agent_id.clone(),
                    SkillCellState::Native { path: native.path },
                )]),
            });
        }
    }

    rows.sort_by(|a, b| {
        a.skill
            .name
            .to_lowercase()
            .cmp(&b.skill.name.to_lowercase())
            .then_with(|| a.skill.scope.cmp(&b.skill.scope))
            .then_with(|| a.skill.native_path.cmp(&b.skill.native_path))
    });
    let counts = SkillCounts {
        all: rows.len(),
        not_in_agentuna: rows
            .iter()
            .filter(|row| {
                row.skill.item_id.is_none() && row.skill.state.as_deref() == Some("native")
            })
            .count(),
        shadowed: rows
            .iter()
            .filter(|row| row.skill.shadowed_by.is_some())
            .count(),
    };
    if filter.not_in_agentuna {
        rows.retain(|row| {
            row.skill.item_id.is_none() && row.skill.state.as_deref() == Some("native")
        });
    }
    if filter.shadowed_only {
        rows.retain(|row| row.skill.shadowed_by.is_some());
    }
    Ok(SkillsIndex { rows, counts })
}

fn cell_from_deployment(
    state: DeploymentState,
    shadowed_by: Option<Scope>,
    native_path: Option<&str>,
) -> SkillCellState {
    if let Some(by) = shadowed_by {
        return SkillCellState::Shadowed { by: Some(by) };
    }
    match state {
        DeploymentState::Linked => SkillCellState::Linked,
        DeploymentState::Off => SkillCellState::NotLinked,
        DeploymentState::Native => SkillCellState::Native {
            path: match native_path {
                Some(path) => PathBuf::from(path),
                None => PathBuf::new(),
            },
        },
        DeploymentState::Shadowed => SkillCellState::Shadowed { by: None },
        DeploymentState::None | DeploymentState::NotInstalled => SkillCellState::None,
        DeploymentState::Conflict | DeploymentState::Drifted => SkillCellState::Error {
            message: state.to_string(),
            remediation: "Review the deployment state before changing this skill.".to_owned(),
        },
    }
}
