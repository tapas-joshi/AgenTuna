//! Reviewed skill deployment plans using adapter paths and at-fs primitives.

use std::path::PathBuf;

use at_model::{
    AgentId, ChangeSet, DeployMode, ExpectedState, FileOp, ItemId, LinkMode, OperationKind, Scope,
};
use thiserror::Error;

/// A deployment could not safely be planned.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeployPlanError {
    /// Destination is already occupied by a path not owned by AgenTuna.
    #[error("native conflict at {path}")]
    NativeConflict {
        /// Occupied path.
        path: PathBuf,
    },
    /// Invalid ChangeSet metadata.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// Canonical source does not lie beneath the active store root.
    #[error("skill source is outside store root: {path}")]
    SourceOutsideStore {
        /// Untrusted source path.
        path: PathBuf,
    },
}

/// Non-fatal deployment plan notice.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeployWarning {
    /// Symlink creation is unsupported and copy was selected instead.
    SymlinkUnsupported {
        /// Destination where links are unavailable.
        path: PathBuf,
    },
    /// Multiple agents consume the same skill root.
    SharedRoot {
        /// Shared destination root.
        path: PathBuf,
        /// Other agents consuming this root.
        agents: Vec<AgentId>,
    },
}

/// One concrete deployment result, including policy metadata for the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPlan {
    /// Reviewed filesystem operations; absent when the managed flag skips deployment.
    pub change_set: Option<ChangeSet>,
    /// Agents whose discovery root is coupled to this destination.
    pub coupled_agents: Vec<AgentId>,
    /// Warnings to show in the preview.
    pub warnings: Vec<DeployWarning>,
    /// Materialisation actually planned, including fallback.
    pub mode_used: Option<LinkMode>,
    /// Whether destination was skipped because the agent is unmanaged.
    pub skipped_unmanaged: bool,
}

/// Inputs required to plan a single skill deployment.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// Item identity.
    pub item_id: ItemId,
    /// Scope being deployed.
    pub scope: Scope,
    /// Agent receiving the deployment.
    pub agent: AgentId,
    /// Active canonical store root used to resolve source paths during apply.
    pub store_root: PathBuf,
    /// Canonical source directory inside the store root.
    pub source: PathBuf,
    /// Native destination directory.
    pub destination: PathBuf,
    /// User-selected link/copy preference.
    pub mode: LinkMode,
    /// Whether symlinks are supported at the target volume.
    pub link_supported: bool,
    /// Whether the selected agent is managed by AgenTuna.
    pub managed: bool,
    /// Whether destination is already recorded as AgenTuna-managed.
    pub destination_managed: bool,
    /// Agents that read the destination root.
    pub shared_agents: Vec<AgentId>,
    /// Project identity, if applicable.
    pub project_id: Option<String>,
    /// Preview summary.
    pub summary: String,
}

/// Creates a reviewed ChangeSet for one skill deployment.
///
/// `link_supported` is a non-mutating capability/preflight result. Actual writes remain the
/// responsibility of at-fs when the returned ChangeSet is applied.
pub fn plan(request: PlanRequest) -> Result<DeploymentPlan, DeployPlanError> {
    if !request.managed {
        return Ok(DeploymentPlan {
            change_set: None,
            coupled_agents: Vec::new(),
            warnings: Vec::new(),
            mode_used: None,
            skipped_unmanaged: true,
        });
    }
    if request.destination.exists() && !request.destination_managed {
        return Err(DeployPlanError::NativeConflict {
            path: request.destination,
        });
    }
    let source_relative = request
        .source
        .strip_prefix(&request.store_root)
        .ok()
        .filter(|path| {
            path.components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .map(PathBuf::from)
        .ok_or_else(|| DeployPlanError::SourceOutsideStore {
            path: request.source.clone(),
        })?;
    let mut warnings = Vec::new();
    let mut coupled_agents: Vec<_> = request
        .shared_agents
        .into_iter()
        .filter(|agent| agent != &request.agent)
        .collect();
    coupled_agents.sort();
    coupled_agents.dedup();
    if !coupled_agents.is_empty() {
        warnings.push(DeployWarning::SharedRoot {
            path: request
                .destination
                .parent()
                .unwrap_or(&request.destination)
                .to_path_buf(),
            agents: coupled_agents.clone(),
        });
    }
    let mode_used = if request.mode == LinkMode::Link && !request.link_supported {
        warnings.push(DeployWarning::SymlinkUnsupported {
            path: request.destination.clone(),
        });
        LinkMode::Copy
    } else {
        request.mode
    };
    let op = match mode_used {
        LinkMode::Link => FileOp::Symlink {
            link_path: request.destination,
            target: source_relative,
            mode: DeployMode::Link,
        },
        LinkMode::Copy => FileOp::Copy {
            source: source_relative,
            dest: request.destination,
            expected: ExpectedState::Absent,
        },
    };
    let change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: uuid::Uuid::now_v7().to_string(),
        operation_kind: OperationKind::parse("deploy")?,
        scope: Some(request.scope),
        project_id: request.project_id,
        summary: request.summary,
        ops: vec![op],
        created_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    change_set.validate()?;
    Ok(DeploymentPlan {
        change_set: Some(change_set),
        coupled_agents,
        warnings,
        mode_used: Some(mode_used),
        skipped_unmanaged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{AgentId, FileOp};

    fn request(mode: LinkMode, supported: bool) -> PlanRequest {
        PlanRequest {
            item_id: ItemId::new(),
            scope: Scope::Project,
            agent: AgentId::Codex,
            store_root: PathBuf::from("/store"),
            source: PathBuf::from("/store/skill"),
            destination: PathBuf::from("/project/.agents/skills/skill"),
            mode,
            managed: true,
            destination_managed: false,
            link_supported: supported,
            shared_agents: vec![AgentId::Codex, AgentId::Omp],
            project_id: Some("project".to_owned()),
            summary: "Deploy skill".to_owned(),
        }
    }

    #[test]
    fn plans_link_copy_fallback_and_shared_roots() {
        let link = plan(request(LinkMode::Link, true)).expect("plan");
        assert!(matches!(
            link.change_set.unwrap().ops[0],
            FileOp::Symlink { .. }
        ));
        assert_eq!(link.coupled_agents, vec![AgentId::Omp]);
        assert!(
            link.warnings
                .iter()
                .any(|w| matches!(w, DeployWarning::SharedRoot { .. }))
        );

        let fallback = plan(request(LinkMode::Link, false)).expect("fallback");
        assert_eq!(fallback.mode_used, Some(LinkMode::Copy));
        assert!(
            fallback
                .warnings
                .iter()
                .any(|w| matches!(w, DeployWarning::SymlinkUnsupported { .. }))
        );
        assert!(matches!(
            fallback.change_set.unwrap().ops[0],
            FileOp::Copy { .. }
        ));

        let copy = plan(request(LinkMode::Copy, true)).expect("copy");
        assert!(matches!(
            copy.change_set.unwrap().ops[0],
            FileOp::Copy { .. }
        ));
    }

    #[test]
    fn unmanaged_agent_is_skipped_and_unmanaged_destination_refused() {
        let mut req = request(LinkMode::Link, true);
        req.managed = false;
        assert!(plan(req).expect("skip").skipped_unmanaged);
        let mut req = request(LinkMode::Link, true);
        req.destination = std::env::temp_dir();
        assert!(matches!(
            plan(req),
            Err(DeployPlanError::NativeConflict { .. })
        ));
    }
    #[test]
    fn copy_fallback_applies_a_skill_tree_end_to_end() {
        use std::fs;

        struct Recorder;
        impl at_fs::OperationRecorder for Recorder {
            fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
                Ok("deploy-test".to_owned())
            }
            fn file_completed(
                &mut self,
                _: &str,
                _: &std::path::Path,
                _: Option<&str>,
                _: Option<&str>,
                _: Option<&std::path::Path>,
            ) -> Result<(), String> {
                Ok(())
            }
            fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
                Ok(())
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let store = temp.path().join("store");
        let target_root = temp.path().join("project-skills");
        fs::create_dir_all(store.join("skills/example")).expect("source");
        fs::create_dir_all(&target_root).expect("target");
        fs::write(store.join("skills/example/SKILL.md"), b"skill").expect("source file");
        let mut request = request(LinkMode::Link, false);
        request.store_root = store.clone();
        request.source = store.join("skills/example");
        request.destination = target_root.join("example");
        let plan = plan(request).expect("fallback plan");
        let set = plan.change_set.expect("ChangeSet");
        let roots = at_fs::ApplyRoots::new([target_root.clone()]).expect("authorized root");
        let backups = temp.path().join("backups");
        let mut recorder = Recorder;
        let mut context = at_fs::ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply fallback");
        assert_eq!(
            fs::read(target_root.join("example/SKILL.md")).expect("copied skill"),
            b"skill"
        );
        assert_eq!(plan.mode_used, Some(LinkMode::Copy));
    }
}
