//! Reviewed plans for enabling or disabling one agent's skill deployment.

use std::path::PathBuf;

use at_model::{AgentId, ChangeSet, FileOp, LinkMode};
use thiserror::Error;

use crate::deploy::{self, DeployPlanError, DeployWarning, PlanRequest};

/// One user-requested skill enablement change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetSkillAgentEnabled {
    /// Stable skill identity.
    pub item_id: at_model::ItemId,
    /// Scope containing the canonical skill.
    pub scope: at_model::Scope,
    /// Agent whose deployment is being changed.
    pub agent: AgentId,
    /// Desired state.
    pub enabled: bool,
}

/// Inputs shared by deployment planning and the disable-manifest plan.
#[derive(Debug, Clone)]
pub struct SkillAgentPlanRequest {
    /// Requested state transition.
    pub intent: SetSkillAgentEnabled,
    /// Path, mode and shared-root data for the common deployment planner.
    pub deployment: PlanRequest,
    /// Whether the adapter detected the agent as installed/configured.
    pub agent_installed: bool,
    /// Global lock reason, when present.
    pub lock_reason: Option<String>,
    /// Whether the deployment already matches the requested state.
    pub already_enabled: bool,
    /// Disable tombstone ChangeSet prepared from the selected manifest.
    pub disable_change_set: Option<ChangeSet>,
}

/// Details of an owned deployment to remove after the reviewed tombstone plan applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRemoval {
    /// Exact adapter target path.
    pub target: PathBuf,
    /// Canonical source path used to validate link ownership.
    pub source: PathBuf,
    /// Last recorded materialization mode.
    pub mode: LinkMode,
    /// Other agents coupled to the same native discovery root.
    pub coupled_agents: Vec<AgentId>,
}

/// A single reviewed transition, including any coupled-root warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillAgentPlan {
    /// Filesystem and manifest operations to review and apply.
    pub change_set: Option<ChangeSet>,
    /// Deployment materialization selected by the common planner.
    pub mode_used: Option<LinkMode>,
    /// Agents affected by the shared root.
    pub coupled_agents: Vec<AgentId>,
    /// Warnings that must be shown before confirmation.
    pub warnings: Vec<DeployWarning>,
    /// True when the request already matches the indexed state.
    pub no_op: bool,
    /// Existing AgenTuna-owned deployment to remove after the reviewed manifest applies.
    pub removal: Option<SkillRemoval>,
}

/// Why the single-agent skill intent could not be planned.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillAgentPlanError {
    /// Agent detection says the selected agent is unavailable.
    #[error("agent `{agent}` is not installed")]
    AgentNotInstalled {
        /// Agent requested by the user.
        agent: AgentId,
    },
    /// The item is locked at Global scope.
    #[error("skill {item_id} is locked: {reason}")]
    Locked {
        /// Stable item identity.
        item_id: at_model::ItemId,
        /// Recorded lock reason.
        reason: String,
    },
    /// The disable plan targets a path that is not an AgenTuna deployment.
    #[error("native conflict at {path}")]
    NativeConflict {
        /// Unmanaged target path.
        path: PathBuf,
    },
    /// Disable requires the reviewed manifest ChangeSet.
    #[error("a reviewed disable-manifest ChangeSet is required")]
    MissingDisablePlan,
    /// Agent is installed but not managed, so AgenTuna will not create a deployment.
    #[error("agent `{agent}` is not managed by AgenTuna")]
    AgentUnmanaged {
        /// Agent that is not managed.
        agent: AgentId,
    },
    /// The common deployment planner refused the request.
    #[error(transparent)]
    Deployment(#[from] DeployPlanError),
}

/// Plans one per-agent skill enable or disable using the shared deployment planner.
///
/// Disable plans carry the reviewed tombstone ChangeSet and the exact owned target to remove;
/// the application applies the ChangeSet with at-fs and performs the guarded removal using its
/// deployment-row snapshot. It must not present the action as agent-independent when roots match.
///
/// # Errors
/// Returns typed missing-agent, lock, native-conflict, missing-plan, or deployment errors.
pub fn plan_skill_agent_change(
    request: SkillAgentPlanRequest,
) -> Result<SkillAgentPlan, SkillAgentPlanError> {
    if !request.agent_installed {
        return Err(SkillAgentPlanError::AgentNotInstalled {
            agent: request.intent.agent,
        });
    }
    if let Some(reason) = request.lock_reason {
        return Err(SkillAgentPlanError::Locked {
            item_id: request.intent.item_id,
            reason,
        });
    }
    if request.already_enabled == request.intent.enabled {
        return Ok(SkillAgentPlan {
            change_set: None,
            mode_used: None,
            coupled_agents: Vec::new(),
            warnings: Vec::new(),
            no_op: true,
            removal: None,
        });
    }

    let mut deployment_request = request.deployment;
    deployment_request.item_id = request.intent.item_id;
    deployment_request.scope = request.intent.scope;
    deployment_request.agent = request.intent.agent.clone();
    let shared_agents = deployment_request.shared_agents.clone();
    if request.intent.enabled {
        let planned = deploy::plan(deployment_request)?;
        if planned.skipped_unmanaged {
            return Err(SkillAgentPlanError::AgentUnmanaged {
                agent: request.intent.agent,
            });
        }
        return Ok(SkillAgentPlan {
            change_set: planned.change_set,
            mode_used: planned.mode_used,
            coupled_agents: planned.coupled_agents,
            warnings: planned.warnings,
            no_op: false,
            removal: None,
        });
    }

    if deployment_request.destination.exists() && !deployment_request.destination_managed {
        return Err(SkillAgentPlanError::NativeConflict {
            path: deployment_request.destination,
        });
    }
    let change_set = request
        .disable_change_set
        .ok_or(SkillAgentPlanError::MissingDisablePlan)?;
    if !change_set.ops.iter().any(|operation| {
        matches!(
            operation,
            FileOp::Delete { path, .. } if path == &deployment_request.destination
        )
    }) {
        return Err(SkillAgentPlanError::MissingDisablePlan);
    }
    let mut coupled_agents = shared_agents
        .into_iter()
        .filter(|agent| agent != &request.intent.agent)
        .collect::<Vec<_>>();
    coupled_agents.sort();
    coupled_agents.dedup();
    let warnings = if coupled_agents.is_empty() {
        Vec::new()
    } else {
        vec![DeployWarning::SharedRoot {
            path: deployment_request
                .destination
                .parent()
                .unwrap_or(&deployment_request.destination)
                .to_path_buf(),
            agents: coupled_agents.clone(),
        }]
    };
    let removal = deployment_request
        .destination_managed
        .then(|| SkillRemoval {
            target: deployment_request.destination,
            source: deployment_request.source,
            mode: deployment_request.mode,
            coupled_agents: coupled_agents.clone(),
        });
    Ok(SkillAgentPlan {
        change_set: Some(change_set),
        mode_used: Some(deployment_request.mode),
        coupled_agents,
        warnings,
        no_op: false,
        removal,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{ExpectedState, OperationKind, Scope};
    use std::path::Path;

    #[test]
    fn copy_mode_disable_accepts_a_reviewed_directory_delete() {
        let agent = AgentId::ClaudeCode;
        let item_id = at_model::ItemId::new();
        let destination = PathBuf::from("/tmp/agentuna-test/skills/example");
        let change_set = ChangeSet {
            id: "operation".to_owned(),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("known operation"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "Disable skill".to_owned(),
            ops: vec![FileOp::Delete {
                path: destination.clone(),
                expected: ExpectedState::hash("0".repeat(64)).expect("valid digest"),
            }],
            created_at_unix_seconds: 0,
        };
        let request = SkillAgentPlanRequest {
            intent: SetSkillAgentEnabled {
                item_id,
                scope: Scope::User,
                agent: agent.clone(),
                enabled: false,
            },
            deployment: PlanRequest {
                item_id: at_model::ItemId::new(),
                scope: Scope::User,
                agent: agent.clone(),
                store_root: PathBuf::from("/tmp/agentuna-test/store"),
                source: PathBuf::from("/tmp/agentuna-test/store/skills/example"),
                destination: destination.clone(),
                mode: LinkMode::Copy,
                link_supported: false,
                managed: true,
                destination_managed: true,
                shared_agents: Vec::new(),
                project_id: None,
                summary: "Disable skill".to_owned(),
            },
            agent_installed: true,
            lock_reason: None,
            already_enabled: true,
            disable_change_set: Some(change_set),
        };

        let plan = plan_skill_agent_change(request).expect("copy deployment removal is supported");

        assert_eq!(plan.mode_used, Some(LinkMode::Copy));
        assert_eq!(plan.removal.expect("owned deployment").target, destination);
    }

    fn base_request(enabled: bool) -> SkillAgentPlanRequest {
        let destination = PathBuf::from("/tmp/agentuna-test/skills/example");
        SkillAgentPlanRequest {
            intent: SetSkillAgentEnabled {
                item_id: at_model::ItemId::new(),
                scope: Scope::User,
                agent: AgentId::ClaudeCode,
                enabled,
            },
            deployment: PlanRequest {
                item_id: at_model::ItemId::new(),
                scope: Scope::User,
                agent: AgentId::ClaudeCode,
                store_root: PathBuf::from("/tmp/agentuna-test/store"),
                source: PathBuf::from("/tmp/agentuna-test/store/skills/example"),
                destination: destination.clone(),
                mode: LinkMode::Link,
                link_supported: true,
                managed: true,
                destination_managed: false,
                shared_agents: Vec::new(),
                project_id: None,
                summary: "Set skill".to_owned(),
            },
            agent_installed: true,
            lock_reason: None,
            already_enabled: false,
            disable_change_set: None,
        }
    }

    fn reviewed_disable_change_set(destination: &Path) -> ChangeSet {
        ChangeSet {
            id: "operation".to_owned(),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("known operation"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "Disable skill".to_owned(),
            ops: vec![FileOp::Delete {
                path: destination.to_path_buf(),
                expected: ExpectedState::hash("0".repeat(64)).expect("valid digest"),
            }],
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn a_missing_agent_is_refused_without_a_plan() {
        let request = base_request(true);
        let error = plan_skill_agent_change(SkillAgentPlanRequest {
            agent_installed: false,
            ..request
        })
        .expect_err("missing agents cannot be planned");
        match error {
            SkillAgentPlanError::AgentNotInstalled { agent } => {
                assert_eq!(agent, AgentId::ClaudeCode);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_locked_item_is_refused_without_a_changeset() {
        let request = base_request(true);
        let item_id = request.intent.item_id;
        let error = plan_skill_agent_change(SkillAgentPlanRequest {
            lock_reason: Some("pinned by policy".to_owned()),
            ..request
        })
        .expect_err("locked items cannot be planned");
        match error {
            SkillAgentPlanError::Locked {
                item_id: locked,
                reason,
            } => {
                assert_eq!(locked, item_id);
                assert_eq!(reason, "pinned by policy");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn reapplying_the_current_state_is_a_no_op() {
        let request = base_request(false);
        let plan = plan_skill_agent_change(SkillAgentPlanRequest {
            already_enabled: false,
            ..request
        })
        .expect("idempotent disable plans");
        assert!(plan.no_op);
        assert!(plan.change_set.is_none());
        assert!(plan.removal.is_none());
        assert!(plan.warnings.is_empty());

        let request = base_request(true);
        let plan = plan_skill_agent_change(SkillAgentPlanRequest {
            already_enabled: true,
            ..request
        })
        .expect("idempotent enable plans");
        assert!(plan.no_op);
        assert!(plan.change_set.is_none());
    }

    #[test]
    fn an_unmanaged_disable_target_is_a_native_conflict() {
        let destination = std::env::temp_dir();
        let request = SkillAgentPlanRequest {
            already_enabled: true,
            ..base_request(false)
        };
        let error = plan_skill_agent_change(SkillAgentPlanRequest {
            disable_change_set: Some(reviewed_disable_change_set(&destination)),
            deployment: PlanRequest {
                destination: destination.clone(),
                ..request.deployment
            },
            ..request
        })
        .expect_err("an unmanaged target must be refused");
        match error {
            SkillAgentPlanError::NativeConflict { path } => assert_eq!(path, destination),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn disable_without_a_reviewed_manifest_plan_is_refused() {
        let destination = PathBuf::from("/tmp/agentuna-test/skills/example");
        let request = SkillAgentPlanRequest {
            already_enabled: true,
            ..base_request(false)
        };
        let error = plan_skill_agent_change(SkillAgentPlanRequest {
            deployment: PlanRequest {
                destination_managed: true,
                destination,
                ..request.deployment
            },
            ..request
        })
        .expect_err("disable requires the reviewed manifest plan");
        assert!(matches!(error, SkillAgentPlanError::MissingDisablePlan));
    }

    #[test]
    fn an_unmanaged_agent_enable_is_refused() {
        let request = base_request(true);
        let error = plan_skill_agent_change(SkillAgentPlanRequest {
            deployment: PlanRequest {
                managed: false,
                ..request.deployment
            },
            ..request
        })
        .expect_err("unmanaged agents cannot receive deployments");
        match error {
            SkillAgentPlanError::AgentUnmanaged { agent } => assert_eq!(agent, AgentId::ClaudeCode),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
