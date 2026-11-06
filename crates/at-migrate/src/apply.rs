//! Build and apply a reviewed migration plan as one journalled ChangeSet.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use at_fs::{ApplyContext as FsApplyContext, ApplyRoots, ChangeSetApplyError, OperationRecorder};
use at_model::{ChangeSet, DeployMode, ExpectedState, FileOp};

use crate::plan::{MigrationAction, MigrationPlan, PlanError, validate_plan};

/// Inputs required to apply a reviewed plan. Paths are injected by the caller.
pub struct ApplyPlanContext<'a> {
    /// Root of the AgenTuna store receiving the migrated items.
    pub store_root: &'a Path,
    /// Operation backup root.
    pub backup_root: &'a Path,
    /// Existing roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Operation recorder, normally backed by `at-db`.
    pub recorder: &'a mut dyn OperationRecorder,
}

/// Result of applying a migration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationApplyOutcome {
    /// Durable operation identifier returned by the recorder.
    pub operation_id: String,
    /// Number of filesystem actions completed.
    pub completed: usize,
    /// Absolute paths changed by the operation.
    pub touched_paths: Vec<PathBuf>,
}

/// Typed failures applying a migration plan.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApplyPlanError {
    /// Reviewed plan or a native source is no longer current.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// A selected migration action is not yet representable as safe file operations.
    #[error("unsupported migration action for `{0}`")]
    Unsupported(PathBuf),
    /// A store destination is not beneath the injected store root.
    #[error("migration destination escapes the store: {0}")]
    OutsideStore(PathBuf),
    /// Native source could not be read or hashed.
    #[error("cannot read migration source `{path}`: {source}")]
    SourceRead {
        /// Native source path.
        path: PathBuf,
        /// Underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// A reviewed ChangeSet failed preflight, apply, journalling or rollback.
    #[error(transparent)]
    Apply(#[from] ChangeSetApplyError),
    /// The operation kind could not be parsed.
    #[error("invalid migration operation kind: {0}")]
    OperationKind(String),
    /// The system clock predates the Unix epoch.
    #[error("system clock is before the Unix epoch")]
    Clock,
}

/// Applies the reviewed plan through one at-fs journal and operation record.
///
/// Skill sources are copied into the store. `LeaveAndWatch` leaves native originals in
/// place; replacement plans move originals into the store's operation backup area before
/// creating a link or copy deployment. Plan and source hashes are rechecked immediately
/// before the ChangeSet is submitted to at-fs.
///
/// # Errors
/// Returns [`ApplyPlanError::Plan`] for stale input, or a typed filesystem/apply failure.
pub fn apply_plan(
    plan: &MigrationPlan,
    context: &mut ApplyPlanContext<'_>,
) -> Result<MigrationApplyOutcome, ApplyPlanError> {
    validate_plan(plan)?;
    let mut ops = Vec::new();
    let mut touched = Vec::new();
    let mut sources: Vec<(PathBuf, PathBuf, String)> = Vec::new();
    for step in &plan.steps {
        match step.action {
            MigrationAction::CreateStoreItem => {
                let source = step
                    .source
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(PathBuf::new()))?;
                let destination = step
                    .destination
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?;
                let relative = destination
                    .strip_prefix(context.store_root)
                    .map_err(|_| ApplyPlanError::OutsideStore(destination.clone()))?;
                let expected = ExpectedState::Absent;
                ops.push(FileOp::Copy {
                    source: source.clone(),
                    dest: relative.to_path_buf(),
                    expected,
                });
                touched.push(destination.clone());
                sources.push((
                    source.clone(),
                    destination.clone(),
                    step.expected_source_hash.clone().unwrap_or_default(),
                ));
            }
            MigrationAction::RecordWatched => {}
            MigrationAction::WriteLink | MigrationAction::WriteCopy => {
                let source = step
                    .source
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(PathBuf::new()))?;
                let destination = step
                    .destination
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?;
                let original = plan
                    .steps
                    .iter()
                    .find(|candidate| {
                        candidate.action == MigrationAction::BackupOriginal
                            && candidate.source.as_ref() == Some(destination)
                    })
                    .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                let backup = context
                    .store_root
                    .join(".state/migration-originals")
                    .join(&plan.id)
                    .join(format!("{:06}", original.seq));
                let backup_relative = backup
                    .strip_prefix(context.store_root)
                    .map_err(|_| ApplyPlanError::OutsideStore(backup.clone()))?;
                let expected_hash = at_fs::changeset::hash_node(destination)
                    .map_err(|source| ApplyPlanError::SourceRead {
                        path: destination.clone(),
                        source,
                    })?
                    .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                let expected = ExpectedState::hash(&expected_hash)
                    .map_err(|_| ApplyPlanError::Unsupported(destination.clone()))?;
                ops.push(FileOp::Move {
                    from: destination.clone(),
                    to: backup_relative.to_path_buf(),
                    expected,
                });
                match step.action {
                    MigrationAction::WriteLink => ops.push(FileOp::Symlink {
                        link_path: destination.clone(),
                        target: source.clone(),
                        mode: DeployMode::Link,
                    }),
                    MigrationAction::WriteCopy => ops.push(FileOp::Copy {
                        source: source.clone(),
                        dest: destination.clone(),
                        expected: ExpectedState::Absent,
                    }),
                    _ => {}
                }
                touched.extend([destination.clone(), backup]);
            }
            MigrationAction::BackupOriginal => {}
            MigrationAction::RenderMcp | MigrationAction::RenderRule => {
                let path = step.source.clone().unwrap_or_default();
                return Err(ApplyPlanError::Unsupported(path));
            }
        }
    }
    for (source, _, expected) in &sources {
        let actual = hash_source(source).map_err(|source_error| ApplyPlanError::SourceRead {
            path: source.clone(),
            source: source_error,
        })?;
        if &actual != expected {
            return Err(ApplyPlanError::Plan(PlanError::StalePlan));
        }
    }
    let created_at_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApplyPlanError::Clock)?
        .as_secs();
    let operation_kind = at_model::OperationKind::parse("migrate")
        .map_err(|error| ApplyPlanError::OperationKind(error.to_string()))?;
    let change_set = ChangeSet {
        id: plan.id.clone(),
        preview_id: plan.plan_hash.clone(),
        operation_kind,
        scope: Some(plan.scope),
        project_id: None,
        summary: format!("Migrate {} native items", plan.counts.items),
        ops,
        created_at_unix_seconds,
    };
    let mut fs_context = FsApplyContext {
        store_root: context.store_root,
        backup_folder: context.backup_root,
        recorder: context.recorder,
    };
    let outcome = at_fs::apply_with_roots(&change_set, &mut fs_context, context.authorized_roots)?;
    touched.sort();
    touched.dedup();
    Ok(MigrationApplyOutcome {
        operation_id: outcome.operation_id,
        completed: outcome.completed,
        touched_paths: touched,
    })
}

fn hash_source(path: &Path) -> Result<String, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        at_store::manifest::hash_skill_folder(path)
            .map(|hash| hash.to_string())
            .map_err(std::io::Error::other)
    } else {
        let bytes = fs::read(path)?;
        Ok(at_fs::sha256_hex(&bytes))
    }
}
