//! Exact, read-only review plans for adding a skill package and deploying it to agents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};
use thiserror::Error;

/// How a skill is exposed to one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillDeploymentMode {
    /// Create a symbolic link to the canonical store package.
    Symlink,
    /// Copy the package files into the agent directory.
    Copy,
}

/// One agent's deployment destination and allowed native root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDeploymentTarget {
    /// Stable agent identity shown in review.
    pub agent: String,
    /// Absolute agent directory receiving the link or copy.
    pub target: PathBuf,
    /// Configured agent directory which authorizes `target`.
    pub allowed_root: PathBuf,
    /// Deployment strategy selected for this agent.
    pub mode: SkillDeploymentMode,
}

/// One file path that the add operation will create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSkillFile {
    /// Absolute destination path.
    pub path: PathBuf,
    /// Canonical package-relative file path.
    pub package_path: String,
}

/// A collision that must be reviewed before replacing an existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillOverwriteEntry {
    /// Existing absolute destination path.
    pub path: PathBuf,
    /// Unified diff from existing content to planned content.
    pub diff: String,
}

/// Optional, exact `git add` request. It never includes a commit operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillStagePlan {
    /// Repository root containing the project-scope files.
    pub repository_root: PathBuf,
    /// Repository-relative paths to stage, exactly matching created paths.
    pub paths: Vec<String>,
}

/// Complete filesystem plan shown during Add Skill review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillAddFilePlan {
    /// Scope of the canonical skill package.
    pub scope: Scope,
    /// Absolute canonical package directory.
    pub canonical_dir: PathBuf,
    /// Every canonical file path to create.
    pub files: Vec<PlannedSkillFile>,
    /// Agent links or copies, with their explicit source and target.
    pub deployments: Vec<SkillDeploymentTarget>,
    /// Existing-file collisions with diffs; these are never silently skipped.
    pub overwrites: Vec<SkillOverwriteEntry>,
    /// Present only when staging is allowed and explicitly requested.
    pub stage: Option<SkillStagePlan>,
    /// Whether the optional stage control is valid for this scope and repository state.
    pub stage_available: bool,
    /// Journalable ChangeSet used to apply the reviewed writes and deployments.
    pub change_set: ChangeSet,
}

/// Invalid target or source state found while building an Add Skill review plan.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillAddPlanError {
    /// A deployment destination is not an absolute path inside its configured agent root.
    #[error("deployment target is outside the configured agent directory: {0}")]
    OutsideAgentRoot(PathBuf),
    /// The requested package file could not be read safely.
    #[error("cannot inspect existing skill file {path}: {reason}")]
    ReadExisting {
        /// Existing destination.
        path: PathBuf,
        /// Redacted I/O reason.
        reason: String,
    },
    /// Staging was requested without a Project-scope repository.
    #[error("git staging is available only for Project scope in a Git repository")]
    StageUnavailable,
    /// A repository-relative staged path could not be formed.
    #[error("created path is outside the Git repository: {0}")]
    OutsideRepository(PathBuf),
    /// Canonical package is missing the required skill document.
    #[error("skill package is missing SKILL.md")]
    MissingSkillDoc,
    /// The generated write plan is not a valid ChangeSet.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// The existing destination is not a regular file.
    #[error("existing skill path is not a regular file: {0}")]
    NonFileCollision(PathBuf),
    /// A library Link target already exists at the canonical destination.
    #[error("the linked destination already exists; choose another scope or copy the skill")]
    LinkedDestinationExists,
    /// A shared physical destination cannot honor conflicting agent deployment modes.
    #[error("agents sharing a skill destination disagree on deployment mode: {0}")]
    SharedTargetModeConflict(PathBuf),
}

/// Inputs for constructing an Add Skill plan.
pub struct SkillAddPlanRequest<'a> {
    /// Canonical target store.
    pub layout: &'a at_store::layout::StoreLayout,
    /// Validated skill slug.
    pub slug: &'a at_store::layout::Slug,
    /// Validated package content.
    pub package: &'a at_store::skill_package::SkillPackage,
    /// Scope being managed.
    pub scope: Scope,
    /// Agent deployment targets.
    pub deployments: &'a [SkillDeploymentTarget],
    /// Project repository root if project scope is in a Git repository.
    pub repository: Option<&'a at_git::Repository>,
    /// Whether the user selected the optional stage action.
    pub stage_requested: bool,
}

/// Builds the exact canonical files, deployments, overwrite diffs and optional Git stage list.
///
/// This function performs bounded read-only inspection; it creates no files and stages nothing.
///
/// # Errors
/// Returns when a destination escapes its configured root or an existing collision cannot be
/// represented safely in the review diff.
pub fn plan_skill_add(
    request: SkillAddPlanRequest<'_>,
) -> Result<SkillAddFilePlan, SkillAddPlanError> {
    if request.package.get("SKILL.md").is_none() {
        return Err(SkillAddPlanError::MissingSkillDoc);
    }
    let canonical_dir = request.layout.skill_dir(request.slug);
    let mut files = Vec::new();
    let mut overwrites = Vec::new();
    let mut ops = Vec::new();
    let mut deployment_modes = BTreeMap::<PathBuf, SkillDeploymentMode>::new();
    for package_path in request.package.paths() {
        let file =
            request
                .package
                .get(package_path)
                .ok_or_else(|| SkillAddPlanError::ReadExisting {
                    path: canonical_dir.join(package_path),
                    reason: String::from("package path disappeared"),
                })?;
        let path = canonical_dir.join(package_path);
        files.push(PlannedSkillFile {
            path: path.clone(),
            package_path: package_path.to_owned(),
        });
        let expected = inspect_collision(&path, &file.bytes, &mut overwrites)?;
        ops.push(FileOp::Write {
            path: path.clone(),
            bytes: file.bytes.clone(),
            expected,
            follow_symlink: false,
            backup: true,
        });
    }
    for deployment in request.deployments {
        if !deployment.target.is_absolute()
            || !deployment.allowed_root.is_absolute()
            || !deployment.target.starts_with(&deployment.allowed_root)
        {
            return Err(SkillAddPlanError::OutsideAgentRoot(
                deployment.target.clone(),
            ));
        }
        if let Some(previous_mode) = deployment_modes.get(&deployment.target) {
            if *previous_mode != deployment.mode {
                return Err(SkillAddPlanError::SharedTargetModeConflict(
                    deployment.target.clone(),
                ));
            }
            continue;
        }
        deployment_modes.insert(deployment.target.clone(), deployment.mode);
        if deployment.mode == SkillDeploymentMode::Copy {
            for package_path in request.package.paths() {
                let file = request.package.get(package_path).ok_or_else(|| {
                    SkillAddPlanError::ReadExisting {
                        path: deployment.target.join(package_path),
                        reason: String::from("package path disappeared"),
                    }
                })?;
                let path = deployment.target.join(package_path);
                files.push(PlannedSkillFile {
                    path: path.clone(),
                    package_path: package_path.to_owned(),
                });
                let expected = inspect_collision(&path, &file.bytes, &mut overwrites)?;
                ops.push(FileOp::Write {
                    path,
                    bytes: file.bytes.clone(),
                    expected,
                    follow_symlink: false,
                    backup: true,
                });
            }
        } else {
            files.push(PlannedSkillFile {
                path: deployment.target.clone(),
                package_path: String::from("SKILL.md"),
            });
            match std::fs::symlink_metadata(&deployment.target) {
                Ok(_) => {
                    return Err(SkillAddPlanError::NonFileCollision(
                        deployment.target.clone(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(SkillAddPlanError::ReadExisting {
                        path: deployment.target.clone(),
                        reason: error.to_string(),
                    });
                }
            }
            ops.push(FileOp::Symlink {
                link_path: deployment.target.clone(),
                target: canonical_dir.clone(),
                mode: at_model::DeployMode::Link,
            });
        }
    }
    let stage = if request.stage_requested {
        if request.scope != Scope::Project {
            return Err(SkillAddPlanError::StageUnavailable);
        }
        let repository = request
            .repository
            .ok_or(SkillAddPlanError::StageUnavailable)?;
        let mut paths = Vec::new();
        for file in &files {
            let Ok(relative) = file.path.strip_prefix(repository.root()) else {
                continue;
            };
            let Some(relative) = relative.to_str() else {
                return Err(SkillAddPlanError::OutsideRepository(file.path.clone()));
            };
            paths.push(relative.to_owned());
        }
        paths.sort();
        paths.dedup();
        Some(SkillStagePlan {
            repository_root: repository.root().to_path_buf(),
            paths,
        })
    } else {
        None
    };
    let change_set = ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: String::new(),
        operation_kind: OperationKind::parse("deploy")?,
        scope: Some(request.scope),
        project_id: None,
        summary: format!("Add skill {}", request.slug),
        ops,
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    change_set.validate()?;
    Ok(SkillAddFilePlan {
        scope: request.scope,
        canonical_dir,
        files,
        deployments: request.deployments.to_vec(),
        overwrites,
        stage,
        stage_available: request.scope == Scope::Project && request.repository.is_some(),
        change_set,
    })
}

/// Rewrites a copy plan so the canonical package is one link to `source_path`.
///
/// Library imports offer Copy or Link. [`plan_skill_add`] always plans a copied package; this
/// function converts that reviewed plan in place: the canonical directory becomes a symlink to
/// the source package, every file write under it is dropped, and agent deployments that pointed
/// at the canonical directory are retargeted to the source. The optional stage list is rebuilt
/// from the remaining planned paths.
///
/// # Errors
/// Returns [`SkillAddPlanError::LinkedDestinationExists`] when the canonical directory already
/// exists, or [`SkillAddPlanError::ReadExisting`] when it cannot be inspected.
pub fn convert_to_library_link(
    plan: &mut SkillAddFilePlan,
    source_path: &Path,
) -> Result<(), SkillAddPlanError> {
    let canonical_dir = plan.canonical_dir.clone();
    match std::fs::symlink_metadata(&canonical_dir) {
        Ok(_) => return Err(SkillAddPlanError::LinkedDestinationExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SkillAddPlanError::ReadExisting {
                path: canonical_dir,
                reason: error.to_string(),
            });
        }
    }
    let mut operations = Vec::with_capacity(plan.change_set.ops.len());
    operations.push(FileOp::Symlink {
        link_path: canonical_dir.clone(),
        target: source_path.to_path_buf(),
        mode: at_model::DeployMode::Link,
    });
    for operation in std::mem::take(&mut plan.change_set.ops) {
        match operation {
            FileOp::Write { path, .. } if path.starts_with(&canonical_dir) => {}
            FileOp::Symlink {
                link_path,
                target,
                mode,
            } if target == canonical_dir => operations.push(FileOp::Symlink {
                link_path,
                target: source_path.to_path_buf(),
                mode,
            }),
            other => operations.push(other),
        }
    }
    plan.change_set.ops = operations;
    plan.files
        .retain(|file| !file.path.starts_with(&canonical_dir));
    plan.files.push(PlannedSkillFile {
        path: canonical_dir,
        package_path: "linked skill folder".to_owned(),
    });
    if let Some(stage) = &mut plan.stage {
        let mut paths = Vec::new();
        for file in &plan.files {
            let relative = file
                .path
                .strip_prefix(&stage.repository_root)
                .map_err(|_| SkillAddPlanError::OutsideRepository(file.path.clone()))?;
            let relative = relative
                .to_str()
                .ok_or_else(|| SkillAddPlanError::OutsideRepository(file.path.clone()))?;
            paths.push(relative.to_owned());
        }
        paths.sort();
        paths.dedup();
        stage.paths = paths;
    }
    Ok(())
}

fn inspect_collision(
    path: &Path,
    planned: &[u8],
    overwrites: &mut Vec<SkillOverwriteEntry>,
) -> Result<ExpectedState, SkillAddPlanError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ExpectedState::absent()),
        Err(error) => Err(SkillAddPlanError::ReadExisting {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let existing = at_fs::symlink::read_nofollow(path).map_err(|error| {
                SkillAddPlanError::ReadExisting {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                }
            })?;
            let display = path.display().to_string();
            let diff = at_fs::diff::unified(&display, Some(&existing), planned);
            let expected = ExpectedState::hash(at_fs::sha256_hex(&existing))?;
            overwrites.push(SkillOverwriteEntry {
                path: path.to_path_buf(),
                diff,
            });
            Ok(expected)
        }
        Ok(_) => Err(SkillAddPlanError::NonFileCollision(path.to_path_buf())),
    }
}

/// Returns an apply error unchanged or stages exactly the reviewed paths after a successful apply.
///
/// # Errors
/// Returns the original apply failure, or a typed Git error if staging fails after apply.
pub fn stage_after_success<T, E: std::fmt::Debug>(
    apply_result: Result<T, E>,
    plan: &SkillAddFilePlan,
    repository: Option<&at_git::Repository>,
    runner: &dyn at_git::LocalGitRunner,
) -> Result<T, SkillAddApplyError<E>> {
    let value = apply_result.map_err(SkillAddApplyError::Apply)?;
    if let Some(stage) = &plan.stage {
        let repository = repository.ok_or(SkillAddApplyError::RepositoryUnavailable)?;
        if repository.root() != stage.repository_root {
            return Err(SkillAddApplyError::RepositoryUnavailable);
        }
        let paths = stage.paths.iter().map(String::as_str).collect::<Vec<_>>();
        at_git::add_paths(runner, repository, &paths).map_err(SkillAddApplyError::Git)?;
    }
    Ok(value)
}

/// Failure while applying an Add Skill plan and optionally staging it.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillAddApplyError<E: std::fmt::Debug> {
    /// Applying the reviewed file plan failed; no Git stage was attempted.
    #[error("skill plan apply failed")]
    Apply(E),
    /// The reviewed repository no longer matches the stage plan.
    #[error("reviewed Git repository is unavailable")]
    RepositoryUnavailable,
    /// Git rejected staging after the files were successfully applied.
    #[error(transparent)]
    Git(#[from] at_git::LocalGitError),
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use at_store::{
        layout::{Slug, StoreLayout},
        paths::{AppPaths, PathEnvironment},
        skill_package::SkillPackage,
    };
    use tempfile::TempDir;

    #[derive(Default)]
    struct GitRecorder {
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl at_git::LocalGitRunner for GitRecorder {
        fn run(
            &self,
            call: &at_git::LocalGitCall<'_>,
        ) -> Result<at_git::GitOutput, at_git::GitError> {
            self.calls
                .lock()
                .map_err(|_| at_git::GitError::Wait(std::io::Error::other("poisoned")))?
                .push(call.args.iter().map(|arg| (*arg).to_owned()).collect());
            let stdout = if call.args == ["rev-parse", "--show-toplevel"] {
                format!("{}\n", call.repo.display()).into_bytes()
            } else {
                Vec::new()
            };
            Ok(at_git::GitOutput {
                success: true,
                code: Some(0),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    #[derive(Default)]
    struct NoopRecorder;

    impl at_fs::changeset::OperationRecorder for NoopRecorder {
        fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
            Ok(String::from("test-operation"))
        }

        fn file_completed(
            &mut self,
            _: &str,
            _: &Path,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&Path>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
            Ok(())
        }
    }

    fn collect_tree(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("read tree") {
            let entry = entry.expect("tree entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("file type");
            if file_type.is_dir() {
                collect_tree(root, &path, paths);
            } else {
                paths.push(
                    path.strip_prefix(root)
                        .expect("relative tree path")
                        .to_path_buf(),
                );
            }
        }
    }

    fn fixture() -> (TempDir, AppPaths, StoreLayout, Slug, SkillPackage) {
        let temp = TempDir::new().expect("sandbox");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        let paths = AppPaths::resolve(
            &PathEnvironment::default()
                .with_home(&home)
                .with_project_root(&project),
        )
        .expect("paths");
        let layout = StoreLayout::for_scope(&paths, Scope::Project).expect("project store");
        let slug = Slug::parse("fixture-skill").expect("slug");
        let mut package = SkillPackage::new();
        package
            .insert("SKILL.md", b"# Fixture\n".to_vec(), 0o644)
            .expect("skill doc");
        package
            .insert("scripts/run.sh", b"#!/bin/sh\n".to_vec(), 0o755)
            .expect("script");
        (temp, paths, layout, slug, package)
    }

    #[test]
    fn plan_lists_canonical_and_deployment_paths_and_overwrite_diff() {
        let (_temp, _paths, layout, slug, package) = fixture();
        let target = layout.skill_dir(&slug);
        std::fs::create_dir_all(&target).expect("existing package");
        std::fs::write(target.join("SKILL.md"), b"# Old\n").expect("existing file");
        let allowed = target
            .parent()
            .expect("skills directory")
            .parent()
            .expect("project store")
            .join("agents");
        std::fs::create_dir_all(&allowed).expect("allowed root");
        let deployment_path = allowed.join("fixture-skill");
        let deployments = [SkillDeploymentTarget {
            agent: "codex".to_owned(),
            target: deployment_path.clone(),
            allowed_root: allowed,
            mode: SkillDeploymentMode::Symlink,
        }];
        let plan = plan_skill_add(SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &deployments,
            repository: None,
            stage_requested: false,
        })
        .expect("plan");
        assert_eq!(plan.files.len(), 3);
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == target.join("SKILL.md"))
        );
        assert!(plan.files.iter().any(|file| file.path == deployment_path));
        assert_eq!(plan.overwrites.len(), 1);
        assert!(plan.overwrites[0].diff.contains("-# Old"));
        assert!(plan.overwrites[0].diff.contains("+# Fixture"));
        assert!(!plan.stage_available);
        assert!(plan.stage.is_none());
    }

    #[test]
    fn applying_plan_produces_exactly_the_listed_file_tree() {
        let (temp, _paths, layout, slug, package) = fixture();
        let target = layout.skill_dir(&slug);
        std::fs::create_dir_all(target.join("scripts")).expect("canonical package directory");
        let project_root = temp.path().join("project");
        let allowed = project_root.join("agents");
        std::fs::create_dir_all(&allowed).expect("allowed root");
        let deployment_path = allowed.join("fixture-skill");
        let deployments = [SkillDeploymentTarget {
            agent: "codex".to_owned(),
            target: deployment_path.clone(),
            allowed_root: allowed.clone(),
            mode: SkillDeploymentMode::Symlink,
        }];
        let plan = plan_skill_add(SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &deployments,
            repository: None,
            stage_requested: false,
        })
        .expect("plan");
        let project_root = temp.path().join("project");
        let roots = at_fs::changeset::ApplyRoots::new([project_root.clone()]).expect("roots");
        let mut recorder = NoopRecorder;
        let backup_folder = temp.path().join("backups");
        std::fs::create_dir_all(&backup_folder).expect("backup folder");
        let mut context = at_fs::changeset::ApplyContext {
            store_root: layout.root(),
            backup_folder: &backup_folder,
            recorder: &mut recorder,
        };
        at_fs::changeset::apply_with_roots(&plan.change_set, &mut context, &roots).expect("apply");

        let mut actual = Vec::new();
        collect_tree(&project_root, &target, &mut actual);
        actual.push(
            deployment_path
                .strip_prefix(&project_root)
                .expect("deployment under project")
                .to_path_buf(),
        );
        let mut expected = plan
            .files
            .iter()
            .map(|file| {
                file.path
                    .strip_prefix(&project_root)
                    .expect("planned file under project")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        assert!(
            std::fs::symlink_metadata(deployment_path)
                .expect("deployment link")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn global_user_and_non_repository_stage_options_are_hidden() {
        let (_temp, paths, project_layout, slug, package) = fixture();
        for scope in [Scope::Global, Scope::User] {
            let layout = StoreLayout::for_scope(&paths, scope).expect("scope store");
            let plan = plan_skill_add(SkillAddPlanRequest {
                layout: &layout,
                slug: &slug,
                package: &package,
                scope,
                deployments: &[],
                repository: None,
                stage_requested: false,
            })
            .expect("plan");
            assert!(!plan.stage_available);
            assert!(plan.stage.is_none());
        }
        let plan = plan_skill_add(SkillAddPlanRequest {
            layout: &project_layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &[],
            repository: None,
            stage_requested: false,
        })
        .expect("non-repository project plan");
        assert!(!plan.stage_available);
    }

    #[test]
    fn plan_rejects_deployment_outside_configured_agent_directory() {
        let (_temp, _paths, layout, slug, package) = fixture();
        let deployments = [SkillDeploymentTarget {
            agent: "codex".to_owned(),
            target: PathBuf::from("/outside/skill"),
            allowed_root: PathBuf::from("/configured/skills"),
            mode: SkillDeploymentMode::Symlink,
        }];
        assert!(matches!(
            plan_skill_add(SkillAddPlanRequest {
                layout: &layout,
                slug: &slug,
                package: &package,
                scope: Scope::Project,
                deployments: &deployments,
                repository: None,
                stage_requested: false,
            }),
            Err(SkillAddPlanError::OutsideAgentRoot(_))
        ));
    }

    #[test]
    fn library_link_changes_a_copy_plan_into_one_symlink_operation() {
        let (temp, _paths, layout, slug, package) = fixture();
        let source = temp.path().join("library-source").join("fixture-skill");
        std::fs::create_dir_all(&source).expect("library source");
        std::fs::write(source.join("SKILL.md"), b"# Fixture\n").expect("source document");
        let copy = plan_skill_add(SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &[],
            repository: None,
            stage_requested: false,
        })
        .expect("copy plan");
        assert!(
            copy.change_set
                .ops
                .iter()
                .all(|operation| matches!(operation, FileOp::Write { .. })),
            "Copy plans write every package file: {:?}",
            copy.change_set.ops
        );
        assert_eq!(copy.files.len(), 2);

        let mut link = copy.clone();
        convert_to_library_link(&mut link, &source).expect("link plan");
        assert_eq!(link.change_set.ops.len(), 1);
        let FileOp::Symlink {
            link_path,
            target,
            mode,
        } = &link.change_set.ops[0]
        else {
            panic!("Link mode must plan one symlink");
        };
        assert_eq!(link_path, &layout.skill_dir(&slug));
        assert_eq!(target, &source);
        assert_eq!(*mode, at_model::DeployMode::Link);
        assert_eq!(link.files.len(), 1);
        assert_eq!(link.files[0].path, layout.skill_dir(&slug));
        assert_eq!(link.files[0].package_path, "linked skill folder");
        assert_ne!(
            copy.files, link.files,
            "Copy and Link reviews must list different file operations"
        );
    }

    #[test]
    fn library_link_refuses_an_existing_destination() {
        let (_temp, _paths, layout, slug, package) = fixture();
        std::fs::create_dir_all(layout.skill_dir(&slug)).expect("existing destination");
        let mut plan = plan_skill_add(SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &[],
            repository: None,
            stage_requested: false,
        })
        .expect("copy plan");
        let source = layout.root().join("source");
        assert!(matches!(
            convert_to_library_link(&mut plan, &source),
            Err(SkillAddPlanError::LinkedDestinationExists)
        ));
    }

    #[test]
    fn stage_option_requires_project_repository_and_stages_only_after_apply() {
        let (temp, _paths, layout, slug, package) = fixture();
        let project = temp.path().join("project");
        let runner = GitRecorder::default();
        let repository = at_git::detect_repository(&runner, &project).expect("repo");
        let deployments = [];
        let plan = plan_skill_add(SkillAddPlanRequest {
            layout: &layout,
            slug: &slug,
            package: &package,
            scope: Scope::Project,
            deployments: &deployments,
            repository: Some(&repository),
            stage_requested: true,
        })
        .expect("stage plan");
        assert!(plan.stage_available);
        assert_eq!(
            plan.stage.as_ref().expect("stage").paths,
            vec![
                ".agentuna/skills/fixture-skill/SKILL.md",
                ".agentuna/skills/fixture-skill/scripts/run.sh",
            ]
        );
        runner.calls.lock().expect("calls").clear();
        assert!(
            stage_after_success(
                Err::<(), _>("apply failed"),
                &plan,
                Some(&repository),
                &runner
            )
            .is_err()
        );
        assert!(runner.calls.lock().expect("calls").is_empty());
        stage_after_success(Ok::<(), String>(()), &plan, Some(&repository), &runner)
            .expect("stage");
        assert_eq!(
            *runner.calls.lock().expect("calls"),
            vec![vec![
                "add".to_owned(),
                "--".to_owned(),
                ".agentuna/skills/fixture-skill/SKILL.md".to_owned(),
                ".agentuna/skills/fixture-skill/scripts/run.sh".to_owned(),
            ]]
        );
    }
}
