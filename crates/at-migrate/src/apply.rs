//! Build and apply a reviewed migration plan as one journalled ChangeSet.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::undo::{UndoError, UndoOperationRequest, UndoOutcome, undo_operation};
use at_fs::{ApplyContext as FsApplyContext, ApplyRoots, ChangeSetApplyError};
use at_model::{ChangeSet, DeployMode, ExpectedState, FileOp, ItemId, ItemKind, ItemState};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ContentHash, ManifestItem, StoreManifest, StorePath};

use crate::plan::{
    MigrationAction, MigrationPlan, PlanError, ScopeStores, plans_hash, validate_plan,
};

/// Inputs required to apply a reviewed plan. Paths are injected by the caller.
pub struct ApplyPlanContext<'a> {
    /// Root of the AgenTuna store receiving the migrated items.
    pub store_root: &'a Path,
    /// Operation backup root.
    pub backup_root: &'a Path,
    /// Existing roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Database for the operation journal and migration metadata.
    pub database: &'a mut at_db::Database,
    /// Active project id when migrating Project or Local scope items.
    pub project_id: Option<&'a str>,
}

/// Inputs required to apply one reviewed selection that may span scopes.
pub struct ApplyPlansContext<'a> {
    /// Store root per receiving scope; the first plan's scope root holds the journal and lock.
    pub stores: &'a ScopeStores,
    /// Operation backup root.
    pub backup_root: &'a Path,
    /// Existing roots authorized for absolute native paths.
    pub authorized_roots: &'a ApplyRoots,
    /// Database for the operation journal and migration metadata.
    pub database: &'a mut at_db::Database,
    /// Active project id when migrating Project or Local scope items.
    pub project_id: Option<&'a str>,
}

/// One plan's filesystem and metadata contribution to a shared ChangeSet.
struct PreparedPlan {
    ops: Vec<FileOp>,
    touched: Vec<PathBuf>,
    items: Vec<CanonicalMigrationItem>,
    manifest: Option<(PathBuf, PathBuf, Vec<u8>, ExpectedState)>,
    sources: Vec<(PathBuf, String)>,
}

struct CanonicalMigrationItem {
    id: ItemId,
    kind: ItemKind,
    scope: at_model::Scope,
    native_scope: at_model::Scope,
    slug: String,
    name: String,
    source_path: PathBuf,
    store_path: PathBuf,
    content_hash: String,
    agents: Vec<at_model::AgentId>,
    target_path: PathBuf,
    mode: &'static str,
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
    /// Deviations from the reviewed plan that completed instead of failing.
    pub warnings: Vec<MigrationWarning>,
}

/// A reviewed mode that the platform refused, replaced by a working equivalent.
///
/// The apply continues: the deployment row records the mode actually written, so the item is
/// never left in a state the UI cannot explain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationWarning {
    /// The platform refused the symbolic link, so a manifest-bearing copy was written.
    ///
    /// This is the `EPERM`/`EXDEV`/network-volume/Windows-without-privilege path. The copy
    /// carries [`at_fs::COPY_MANIFEST_FILE`] and the deployment row records `mode = copy`.
    CopyFallback {
        /// Agent path that received the copy instead of a link.
        path: PathBuf,
        /// Mode observed at that path after the write (`copy`).
        observed_mode: &'static str,
    },
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
    /// An agent folder refused access, so nothing was backed up or replaced.
    #[error("cannot write agent folder `{path}`: permission denied; {required}")]
    PermissionDenied {
        /// The existing folder that must be writable, named so the user can fix it.
        path: PathBuf,
        /// What the user has to grant.
        required: &'static str,
    },
    /// Canonical store metadata could not be loaded or updated.
    #[error("canonical store update failed: {0}")]
    Store(String),
    /// A reviewed ChangeSet failed preflight, apply, journalling or rollback.
    #[error(transparent)]
    Apply(#[from] ChangeSetApplyError),
    /// The operation kind could not be parsed.
    #[error("invalid migration operation kind: {0}")]
    OperationKind(String),
    /// The system clock predates the Unix epoch.
    #[error("the system clock is before the Unix epoch")]
    Clock,
    /// Migration index rows could not be committed.
    #[error("migration metadata failed: {0}")]
    Metadata(String),
    /// A failed phase could not be rolled back, so both failures are reported.
    #[error("migration failed: {cause}; filesystem rollback failed: {rollback}")]
    Rollback {
        /// Failure that began the rollback.
        cause: String,
        /// Why the filesystem rollback could not restore what the phase wrote.
        rollback: String,
    },
    /// A project-scoped migration requires an active project identity.
    #[error("project-scope item `{0}` requires a project id")]
    MissingProjectId(String),
    /// No plan was supplied to apply.
    #[error("an empty plan selection cannot be applied")]
    EmptyPlans,
    /// No store root was supplied for a scope a plan writes into.
    #[error("no {scope} store is available to receive the migrated items")]
    MissingStore {
        /// Receiving scope whose store root was not injected.
        scope: at_model::Scope,
    },
}

/// Applies the reviewed plan through one at-fs journal and operation record.
///
/// Skill sources are copied into the store. `LeaveAndWatch` leaves native originals in
/// place; replacement plans move each original tree into the operation backup area before
/// creating a link or a manifest-bearing copy deployment. Plan and source hashes are
/// rechecked immediately before the ChangeSet is submitted to at-fs.
///
/// The whole migration is one operation: the canonical store files and manifest, the
/// `items`/`deployments`/`native_items` rows and the native links or copies either all
/// commit or none of them do. A metadata failure after the filesystem apply rolls the
/// filesystem back; a filesystem failure records no metadata at all.
///
/// A platform that refuses the link (`EPERM`, `EXDEV`, a network volume, Windows without
/// the symlink privilege) falls back to a manifest-bearing copy for that item, records
/// `mode = copy` in the deployment row and reports [`MigrationWarning::CopyFallback`]
/// instead of failing. On Windows a directory may be replaced by an NTFS junction, which
/// [`at_fs`] documents as reviewable-only code behind `cfg(windows)`; the junction mode is
/// stored as `junction` in the deployment row and no macOS or Linux run produces it.
///
/// Two agents that read one physical skills root (Codex and omp share
/// `<project>/.agents/skills`) get one planned step for that path and one deployment row
/// per agent pointing at it: the effect is coupled, and the plan says so instead of
/// offering two independent switches.
///
/// # Errors
/// Returns [`ApplyPlanError::Plan`] for stale input, [`ApplyPlanError::PermissionDenied`]
/// when an agent folder refuses access (nothing is written), or a typed filesystem/apply
/// failure.
pub fn apply_plan(
    plan: &MigrationPlan,
    context: &mut ApplyPlanContext<'_>,
) -> Result<MigrationApplyOutcome, ApplyPlanError> {
    apply_plan_with_actor(plan, context, at_db::ops::OperationActor::Ui)
}

/// Applies a reviewed plan and records the supplied operation actor.
///
/// # Errors
/// Returns the same failures as [`apply_plan`].
pub fn apply_plan_with_actor(
    plan: &MigrationPlan,
    context: &mut ApplyPlanContext<'_>,
    actor: at_db::ops::OperationActor,
) -> Result<MigrationApplyOutcome, ApplyPlanError> {
    let stores = ScopeStores::single(plan.scope, context.store_root.to_path_buf());
    let mut scoped = ApplyPlansContext {
        stores: &stores,
        backup_root: context.backup_root,
        authorized_roots: context.authorized_roots,
        database: &mut *context.database,
        project_id: context.project_id,
    };
    apply_plans_with_actor(std::slice::from_ref(plan), &mut scoped, actor)
}

/// Applies every reviewed plan as one journalled ChangeSet and one operation record.
///
/// A selection that spans scopes is reviewed as one plan per scope and applied here as a
/// single operation: one journal, one operation row and therefore one Undo, even though the
/// items land in several stores. The first plan's store root holds the journal and lock.
///
/// # Errors
/// Returns [`ApplyPlanError::EmptyPlans`] for an empty selection,
/// [`ApplyPlanError::MissingStore`] when a receiving scope has no injected root,
/// [`ApplyPlanError::MissingProjectId`] for Project or Local items without a project, or the
/// same failures as [`apply_plan`].
pub fn apply_plans(
    plans: &[MigrationPlan],
    context: &mut ApplyPlansContext<'_>,
) -> Result<MigrationApplyOutcome, ApplyPlanError> {
    apply_plans_with_actor(plans, context, at_db::ops::OperationActor::Ui)
}

/// Applies every reviewed plan and records the supplied operation actor.
///
/// # Errors
/// Returns the same failures as [`apply_plans`].
pub fn apply_plans_with_actor(
    plans: &[MigrationPlan],
    context: &mut ApplyPlansContext<'_>,
    actor: at_db::ops::OperationActor,
) -> Result<MigrationApplyOutcome, ApplyPlanError> {
    let Some(first_plan) = plans.first() else {
        return Err(ApplyPlanError::EmptyPlans);
    };
    let mut store_roots = std::collections::BTreeMap::new();
    for plan in plans {
        validate_plan(plan)?;
        let Some(store_root) = context.stores.root(plan.scope) else {
            return Err(ApplyPlanError::MissingStore { scope: plan.scope });
        };
        if matches!(
            plan.scope,
            at_model::Scope::Project | at_model::Scope::Local
        ) && context.project_id.is_none()
        {
            return Err(ApplyPlanError::MissingProjectId(plan.scope.to_string()));
        }
        preflight_native_access(plan)?;
        store_roots.insert(plan.scope, store_root.to_path_buf());
    }
    let operation_root =
        store_roots
            .get(&first_plan.scope)
            .cloned()
            .ok_or(ApplyPlanError::MissingStore {
                scope: first_plan.scope,
            })?;
    // A project id belongs on the operation only when project-scoped stores received items.
    let operation_project = plans
        .iter()
        .any(|plan| {
            matches!(
                plan.scope,
                at_model::Scope::Project | at_model::Scope::Local
            )
        })
        .then_some(context.project_id)
        .flatten();
    let mut ops = Vec::new();
    let mut touched = Vec::new();
    let mut canonical_items = Vec::new();
    let mut sources = Vec::new();
    let mut manifests = Vec::new();
    for plan in plans {
        let Some(store_root) = store_roots.get(&plan.scope) else {
            return Err(ApplyPlanError::MissingStore { scope: plan.scope });
        };
        let prepared = prepare_plan(
            plan,
            store_root,
            &operation_root,
            context.project_id,
            &mut *context.database,
        )?;
        ops.extend(prepared.ops);
        touched.extend(prepared.touched);
        canonical_items.extend(prepared.items);
        sources.extend(prepared.sources);
        if let Some(manifest) = prepared.manifest {
            manifests.push(manifest);
        }
    }
    for (changeset_path, absolute_path, bytes, expected) in manifests {
        ops.push(FileOp::Write {
            path: changeset_path,
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
        touched.push(absolute_path);
    }
    for (source, expected) in &sources {
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
    let total_items = plans.iter().map(|plan| plan.counts.items).sum::<usize>();
    let mut change_set = ChangeSet {
        id: first_plan.id.clone(),
        preview_id: plans_hash(plans)?,
        operation_kind,
        scope: Some(first_plan.scope),
        project_id: operation_project.map(str::to_owned),
        summary: format!("Migrate {total_items} native items"),
        ops,
        created_at_unix_seconds,
    };
    // A receiving scope with no store yet gets its skeleton in this same reviewed set, so the
    // reviewed plan, its dry run, the apply and the undo all include the store creation. The
    // store owning the operation is addressed relatively; every other store absolutely.
    for plan in plans {
        let Some(store_root) = store_roots.get(&plan.scope) else {
            return Err(ApplyPlanError::MissingStore { scope: plan.scope });
        };
        let layout = StoreLayout::new(plan.scope, store_root);
        at_store::store_init::plan_store_init_in(
            &layout,
            &mut change_set,
            store_root != &operation_root,
        )
        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
    }
    let mut outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new_with_actor(
            context.database.connection_mut(),
            30,
            actor,
        );
        let mut fs_context = FsApplyContext {
            store_root: &operation_root,
            backup_folder: context.backup_root,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots_reversible(&change_set, &mut fs_context, context.authorized_roots)?
    };
    let applied_items = canonical_items;
    let warnings = match record_migration_metadata(
        context.database,
        &applied_items,
        &outcome.operation_id,
        context.project_id,
    ) {
        Ok(warnings) => warnings,
        Err(error) => {
            if let Some(handle) = outcome.rollback_handle.take() {
                let mut recorder = at_fs::DbOperationRecorder::new_with_actor(
                    context.database.connection_mut(),
                    30,
                    actor,
                );
                if let Err(rollback) = at_fs::rollback_committed(handle, &mut recorder) {
                    // The caller needs both: the metadata write started the rollback, and a
                    // rollback that itself failed leaves files the journal still describes.
                    return Err(ApplyPlanError::Rollback {
                        cause: error.to_string(),
                        rollback: rollback.to_string(),
                    });
                }
            }
            return Err(error);
        }
    };
    if let Some(handle) = outcome.rollback_handle.take() {
        at_fs::commit_reversible(handle)?;
    }
    touched.sort();
    touched.dedup();
    Ok(MigrationApplyOutcome {
        operation_id: outcome.operation_id,
        touched_paths: touched,
        completed: outcome.completed,
        warnings,
    })
}

/// Builds one plan's ChangeSet operations, manifest update and metadata rows.
///
/// Store paths are written relative to the operation root for a plan that owns it and absolute
/// for every other store, so one ChangeSet can span scopes while a single-scope migration keeps
/// exactly the operations it always had. Native paths are absolute either way.
fn prepare_plan(
    plan: &MigrationPlan,
    store_root: &Path,
    operation_root: &Path,
    project_id: Option<&str>,
    database: &mut at_db::Database,
) -> Result<PreparedPlan, ApplyPlanError> {
    let layout = StoreLayout::new(plan.scope, store_root);
    // A first migration into a scope can run before that scope's store directory exists (a
    // project that was just selected, a brand-new user store, an enabled Global store). The
    // plan extends an empty manifest instead of writing the store here: the merged ChangeSet
    // carries the store skeleton, so the reviewed plan, the apply and the undo all include it.
    let manifest_plan = StoreManifest::load_for_plan(&layout)
        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
    let mut manifest = manifest_plan.manifest;
    let manifest_expected = match &manifest_plan.hash {
        Some(hash) => ExpectedState::hash(hash.as_str())
            .map_err(|error| ApplyPlanError::Store(error.to_string()))?,
        None => ExpectedState::Absent,
    };
    let mut manifest_changed = false;
    let requests = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.action,
                MigrationAction::RenderMcp | MigrationAction::RenderRule
            )
        })
        .map(|step| {
            let agent = step.agents.iter().next().ok_or_else(|| {
                ApplyPlanError::Unsupported(step.source.clone().unwrap_or_default())
            })?;
            let source = step
                .source
                .as_ref()
                .ok_or_else(|| ApplyPlanError::Unsupported(PathBuf::new()))?;
            let project = matches!(
                step.scope,
                at_model::Scope::Project | at_model::Scope::Local
            )
            .then(|| source.parent())
            .flatten();
            Ok(crate::native_projection::NativeProjectionRequest {
                plan,
                step,
                store_root,
                native_source_path: source,
                agent,
                item_name: step
                    .item_name
                    .as_deref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?,
                native_item_name: step
                    .source_item_name
                    .as_deref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?,
                project,
            })
        })
        .collect::<Result<Vec<_>, ApplyPlanError>>()?;
    let projections = if requests.is_empty() {
        crate::native_projection::PreparedNativeProjectionBatch {
            items: Vec::new(),
            canonical_writes: Vec::new(),
            native_writes: Vec::new(),
        }
    } else {
        crate::native_projection::prepare_native_projections(&requests)
            .map_err(|error| ApplyPlanError::Store(error.to_string()))?
    };
    let mut projection_writes_added = false;
    let mut canonical_items = Vec::new();
    let mut ops = Vec::new();
    let mut touched = Vec::new();
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
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
                    .strip_prefix(store_root)
                    .map_err(|_| ApplyPlanError::OutsideStore(destination.clone()))?;
                push_store_item_ops(
                    source,
                    &changeset_path(operation_root, store_root, destination),
                    &mut ops,
                )?;
                touched.push(destination.clone());
                let hash = step
                    .expected_source_hash
                    .as_deref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?;
                let slug_text = relative
                    .file_name()
                    .and_then(|part| part.to_str())
                    .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                let slug = Slug::parse(slug_text)
                    .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                let manifest_path =
                    StorePath::parse(&relative.to_string_lossy().replace('\\', "/"))
                        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                let content_hash = ContentHash::parse(hash)
                    .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                let scope_project_id = if plan.scope == at_model::Scope::Project
                    || plan.scope == at_model::Scope::Local
                {
                    project_id
                } else {
                    None
                };
                let item_id = at_db::items::item_id_by_identity(
                    database.connection(),
                    ItemKind::Skill,
                    plan.scope,
                    scope_project_id,
                    slug_text,
                )
                .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?
                .unwrap_or_else(ItemId::new);
                let item_name = step
                    .item_name
                    .clone()
                    .unwrap_or_else(|| slug_text.to_owned());
                manifest
                    .insert_item(
                        ManifestItem::new(
                            item_id,
                            ItemKind::Skill,
                            slug.clone(),
                            manifest_path,
                            content_hash,
                        )
                        .with_name(item_name.clone()),
                    )
                    .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                let deploy_step = plan.steps.iter().find(|candidate| {
                    matches!(
                        candidate.action,
                        MigrationAction::WriteLink
                            | MigrationAction::WriteCopy
                            | MigrationAction::RecordWatched
                    ) && candidate
                        .source
                        .as_ref()
                        .is_some_and(|path| path == source || path == destination)
                });
                let (agents, target_path, mode) = deploy_step.map_or_else(
                    || (Vec::new(), source.clone(), "native"),
                    |candidate| {
                        let mode = match candidate.action {
                            MigrationAction::WriteCopy => "copy",
                            MigrationAction::WriteLink => "symlink",
                            _ => "native",
                        };
                        (
                            candidate.agents.iter().cloned().collect(),
                            candidate
                                .destination
                                .clone()
                                .unwrap_or_else(|| source.clone()),
                            mode,
                        )
                    },
                );
                canonical_items.push(CanonicalMigrationItem {
                    id: item_id,
                    kind: ItemKind::Skill,
                    scope: plan.scope,
                    native_scope: step.scope,
                    slug: slug_text.to_owned(),
                    name: item_name,
                    source_path: source.clone(),
                    store_path: destination.clone(),
                    content_hash: hash.to_owned(),
                    agents,
                    target_path,
                    mode,
                });
                manifest_changed = true;
                sources.push((source.clone(), hash.to_owned()));
            }
            MigrationAction::RecordWatched => {}
            // The store skeleton is merged into the same ChangeSet after the item operations
            // are assembled; this step is the reviewed line item that announces it.
            MigrationAction::CreateStore => {}
            MigrationAction::UpdateManifest => manifest_changed = true,
            MigrationAction::WriteLink | MigrationAction::WriteCopy => {
                let destination = step
                    .destination
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(PathBuf::new()))?;
                if plan.steps.iter().any(|candidate| {
                    matches!(
                        candidate.action,
                        MigrationAction::RenderMcp | MigrationAction::RenderRule
                    ) && candidate.source.as_ref() == Some(destination)
                }) {
                    continue;
                }
                let source = step
                    .source
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(PathBuf::new()))?;
                let destination = step
                    .destination
                    .as_ref()
                    .ok_or_else(|| ApplyPlanError::Unsupported(source.clone()))?;
                let _original = plan
                    .steps
                    .iter()
                    .find(|candidate| {
                        candidate.action == MigrationAction::BackupOriginal
                            && candidate.source.as_ref() == Some(destination)
                    })
                    .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                let expected_hash = at_fs::changeset::hash_node(destination)
                    .map_err(|source| ApplyPlanError::SourceRead {
                        path: destination.clone(),
                        source,
                    })?
                    .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                let expected = ExpectedState::hash(&expected_hash)
                    .map_err(|_| ApplyPlanError::Unsupported(destination.clone()))?;
                ops.push(FileOp::Delete {
                    path: destination.clone(),
                    expected,
                });
                match step.action {
                    MigrationAction::WriteLink => ops.push(FileOp::Symlink {
                        link_path: destination.clone(),
                        target: source.clone(),
                        mode: DeployMode::Link,
                    }),
                    MigrationAction::WriteCopy => ops.push(FileOp::Symlink {
                        link_path: destination.clone(),
                        target: source.clone(),
                        mode: DeployMode::Copy,
                    }),
                    _ => {}
                }
                touched.push(destination.clone());
            }
            MigrationAction::BackupOriginal => {}
            MigrationAction::RenderMcp | MigrationAction::RenderRule => {
                if projection_writes_added {
                    continue;
                }
                for projection_item in &projections.items {
                    let step = plan
                        .steps
                        .iter()
                        .find(|candidate| {
                            candidate.source.as_ref() == Some(&projection_item.source_path)
                                && candidate.item_name.as_deref().is_some_and(|name| {
                                    name.to_ascii_lowercase() == projection_item.item_name
                                })
                        })
                        .ok_or_else(|| {
                            ApplyPlanError::Unsupported(projection_item.source_path.clone())
                        })?;
                    let destination = step.destination.as_ref().ok_or_else(|| {
                        ApplyPlanError::Unsupported(projection_item.source_path.clone())
                    })?;
                    let relative = destination
                        .strip_prefix(store_root)
                        .map_err(|_| ApplyPlanError::OutsideStore(destination.clone()))?;
                    // Rule files are named `AGENTS`/`CLAUDE`; store slugs are lowercase.
                    let slug_text = projection_item.item_name.to_ascii_lowercase();
                    let slug = Slug::parse(&slug_text)
                        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                    let scope_project_id = if plan.scope == at_model::Scope::Project
                        || plan.scope == at_model::Scope::Local
                    {
                        project_id
                    } else {
                        None
                    };
                    let item_id = at_db::items::item_id_by_identity(
                        database.connection(),
                        projection_item.kind,
                        plan.scope,
                        scope_project_id,
                        slug.as_str(),
                    )
                    .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?
                    .unwrap_or_else(ItemId::new);
                    let canonical = projections
                        .canonical_writes
                        .iter()
                        .find(|write| write.path == *destination)
                        .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                    // `mcp.json` holds every server, so its file hash is not this item's
                    // hash: the manifest and the index record one canonical hash per entry
                    // (`hash_mcp_entry`). Recording the file hash made the next `store.json`
                    // rebuild miss the entry, allocate a new ItemId and orphan every
                    // deployment that still pointed at the old one.
                    let content_hash =
                        store_item_hash(projection_item.kind, &slug_text, &canonical.bytes)?;
                    let store_path =
                        StorePath::parse(&relative.to_string_lossy().replace('\\', "/"))
                            .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                    let content_hash_typed = ContentHash::parse(&content_hash)
                        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                    manifest
                        .insert_item(
                            ManifestItem::new(
                                item_id,
                                projection_item.kind,
                                slug,
                                store_path,
                                content_hash_typed,
                            )
                            .with_name(projection_item.item_name.clone()),
                        )
                        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                    canonical_items.push(CanonicalMigrationItem {
                        id: item_id,
                        kind: projection_item.kind,
                        scope: plan.scope,
                        native_scope: step.scope,
                        slug: slug_text,
                        name: projection_item.item_name.clone(),
                        source_path: projection_item.source_path.clone(),
                        store_path: destination.clone(),
                        content_hash,
                        agents: vec![projection_item.agent.clone()],
                        target_path: projection_item.source_path.clone(),
                        mode: "native",
                    });
                }
                for write in &projections.canonical_writes {
                    if !write.path.starts_with(store_root) {
                        return Err(ApplyPlanError::OutsideStore(write.path.clone()));
                    }
                    ops.push(FileOp::Write {
                        path: changeset_path(operation_root, store_root, &write.path),
                        bytes: write.bytes.clone(),
                        expected: write.expected.clone(),
                        follow_symlink: false,
                        backup: true,
                    });
                    touched.push(write.path.clone());
                }
                for write in &projections.native_writes {
                    ops.push(FileOp::Write {
                        path: write.path.clone(),
                        bytes: write.bytes.clone(),
                        expected: write.expected.clone(),
                        follow_symlink: false,
                        backup: true,
                    });
                    touched.push(write.path.clone());
                }
                manifest_changed = true;
                projection_writes_added = true;
            }
        }
    }
    let manifest = if manifest_changed {
        let manifest_path = layout.manifest_path();
        if !manifest_path.starts_with(store_root) {
            return Err(ApplyPlanError::OutsideStore(manifest_path));
        }
        let bytes = manifest
            .to_bytes()
            .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
        Some((
            changeset_path(operation_root, store_root, &manifest_path),
            manifest_path,
            bytes,
            manifest_expected,
        ))
    } else {
        None
    };
    Ok(PreparedPlan {
        ops,
        touched,
        items: canonical_items,
        manifest,
        sources,
    })
}

/// ChangeSet path for `path` inside one plan's store.
///
/// A plan whose store owns the operation keeps relative paths, so a single-scope migration
/// produces exactly the ChangeSet it always did. Every other store is addressed absolutely so
/// one ChangeSet can carry several stores; at-fs authorizes those against the injected roots.
fn changeset_path(operation_root: &Path, store_root: &Path, path: &Path) -> PathBuf {
    if store_root == operation_root {
        path.strip_prefix(operation_root)
            .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
    } else {
        path.to_path_buf()
    }
}

fn record_migration_metadata(
    database: &mut at_db::Database,
    items: &[CanonicalMigrationItem],
    operation_id: &str,
    project_id: Option<&str>,
) -> Result<Vec<MigrationWarning>, ApplyPlanError> {
    let transaction = database
        .connection_mut()
        .transaction()
        .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
    let mut adopted_paths = std::collections::BTreeSet::new();
    let mut warnings = Vec::new();
    for item in items {
        let source_path = item.store_path.to_string_lossy();
        let item_project_id =
            if item.scope == at_model::Scope::Project || item.scope == at_model::Scope::Local {
                let Some(id) = project_id else {
                    return Err(ApplyPlanError::MissingProjectId(item.slug.clone()));
                };
                Some(id)
            } else {
                None
            };
        let spec = at_db::items::AdoptedItemSpec {
            proposed_id: item.id,
            kind: item.kind,
            scope: item.scope,
            project_id: item_project_id,
            slug: &item.slug,
            name: &item.name,
            source_path: &source_path,
            content_hash: Some(&item.content_hash),
        };
        let stored_id = at_db::items::upsert_adopted_item(&transaction, &spec)
            .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
        if stored_id != item.id {
            return Err(ApplyPlanError::Metadata(format!(
                "store manifest id {} differs from database id {} for {}",
                item.id, stored_id, item.slug
            )));
        }
        if let Some(agent_id) = item.agents.first() {
            let native_path = item.source_path.to_string_lossy();
            if adopted_paths.insert(native_path.to_string()) {
                let native = at_db::native::NativeAdoptionPath {
                    agent_id,
                    scope: item.native_scope,
                    kind: item.kind,
                    name: &item.name,
                    path: &native_path,
                    content_hash: Some(&item.content_hash),
                };
                at_db::native::adopt_paths(&transaction, &[native], &item.id)
                    .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
            }
        }
        let target_path = item.target_path.to_string_lossy();
        let store_path = item.store_path.to_string_lossy();
        let mode = observed_mode(item)?;
        if item.mode == "symlink" && mode != "symlink" {
            // The link was refused: the deployment row below records the mode that exists,
            // and the caller is told instead of discovering a silent downgrade later.
            warnings.push(MigrationWarning::CopyFallback {
                path: item.target_path.clone(),
                observed_mode: mode,
            });
        }
        let (store_hash, target_hash) = observed_hashes(item);
        for agent_id in &item.agents {
            let state = if mode == "native" {
                ItemState::Native
            } else {
                ItemState::Linked
            };
            let hashes = at_db::deployments::DeploymentHashes::new(
                Some(&store_hash),
                Some(&target_hash),
                Some(&target_hash),
            );
            let mut deployment = at_db::deployments::DeploymentSpec::applied(
                &item.id,
                agent_id,
                item.scope,
                state,
                Some(mode),
                Some(&target_path),
                hashes,
            )
            .with_operation(operation_id);
            if let Some(id) = item_project_id {
                deployment = deployment.with_project(id);
            }
            if mode == "symlink" || mode == "junction" {
                deployment = deployment.with_link_target(&store_path);
            }
            at_db::deployments::upsert_deployment(&transaction, &deployment)
                .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
        }
    }
    transaction
        .commit()
        .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
    Ok(warnings)
}

/// Hashes of the store item and the native target as they exist after the apply.
///
/// Drift compares the deployment baseline with the bytes on disk, so it must record the
/// rendered native file (a managed block for a rule, the merged document for MCP, the tree or
/// link target for a skill) and not the source hash the plan carried. A source hash made every
/// migrated rule and MCP document look drifted immediately after a successful migration.
fn observed_hashes(item: &CanonicalMigrationItem) -> (String, String) {
    let store_hash = match item.kind {
        ItemKind::Skill => at_store::hash_skill_folder(&item.store_path)
            .map(|hash| hash.to_string())
            .unwrap_or_else(|_| item.content_hash.clone()),
        _ => fs::read(&item.store_path)
            .map(|bytes| at_fs::sha256_hex(&bytes))
            .unwrap_or_else(|_| item.content_hash.clone()),
    };
    let target = if item.kind == ItemKind::Skill {
        fs::canonicalize(&item.target_path).unwrap_or_else(|_| item.target_path.clone())
    } else {
        item.target_path.clone()
    };
    let target_hash = match item.kind {
        ItemKind::Skill => at_store::hash_skill_folder(&target)
            .map(|hash| hash.to_string())
            .unwrap_or_else(|_| item.content_hash.clone()),
        ItemKind::Rule => fs::read(&target)
            .map(|bytes| at_store::hash_rule_bytes(&bytes).to_string())
            .unwrap_or_else(|_| item.content_hash.clone()),
        _ => fs::read(&target)
            .map(|bytes| at_fs::sha256_hex(&bytes))
            .unwrap_or_else(|_| item.content_hash.clone()),
    };
    (store_hash, target_hash)
}

fn observed_mode(item: &CanonicalMigrationItem) -> Result<&'static str, ApplyPlanError> {
    if item.mode != "symlink" {
        return Ok(item.mode);
    }
    let metadata =
        fs::symlink_metadata(&item.target_path).map_err(|source| ApplyPlanError::SourceRead {
            path: item.target_path.clone(),
            source,
        })?;
    if metadata.file_type().is_symlink() {
        return Ok("symlink");
    }
    if at_fs::CopyManifest::read(&item.target_path)
        .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?
        .is_some()
    {
        return Ok("copy");
    }
    Ok("junction")
}
/// Appends the canonical-store write for one migrated skill folder.
///
/// The store item is plain content: a [`FileOp::Copy`] directory would write
/// [`at_fs::COPY_MANIFEST_FILE`] into the item, and that manifest would become part of the
/// item's hashed content (and of every copy deployed from it). [`FileOp::CopyTree`] copies
/// the tree without it, so `hash_skill_folder(store item)` equals the reviewed source hash.
fn push_store_item_ops(
    source: &Path,
    destination_relative: &Path,
    ops: &mut Vec<FileOp>,
) -> Result<(), ApplyPlanError> {
    if !source.is_dir() {
        return Err(ApplyPlanError::Unsupported(source.to_path_buf()));
    }
    ops.push(FileOp::CopyTree {
        source: source.to_path_buf(),
        dest: destination_relative.to_path_buf(),
        expected: ExpectedState::Absent,
    });
    Ok(())
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

/// What the user has to grant when an agent folder is not writable.
const REQUIRED_WRITE_ACCESS: &str = "grant write access to this folder; nothing was written";

/// What the user has to grant when macOS protects the folder with TCC.
const REQUIRED_FULL_DISK_ACCESS: &str = "grant AgenTuna Full Disk Access in System Settings \
     → Privacy & Security (macOS protects this folder with TCC); nothing was written";

/// Folder names macOS guards with TCC inside a user's home.
const TCC_PROTECTED: [&str; 4] = ["Desktop", "Documents", "Downloads", "Library"];

/// Checks that every agent folder this plan replaces is writable, before anything is backed up.
///
/// A read-only or macOS-TCC-protected agent folder becomes
/// [`ApplyPlanError::PermissionDenied`] naming the folder, so the caller can show an
/// actionable error while every original is still untouched.
fn preflight_native_access(plan: &MigrationPlan) -> Result<(), ApplyPlanError> {
    let mut probed = std::collections::BTreeSet::new();
    for step in &plan.steps {
        if !matches!(
            step.action,
            MigrationAction::WriteLink
                | MigrationAction::WriteCopy
                | MigrationAction::RenderMcp
                | MigrationAction::RenderRule
        ) {
            continue;
        }
        let Some(destination) = step
            .destination
            .as_deref()
            .filter(|path| path.is_absolute())
        else {
            continue;
        };
        let Some(anchor) = destination.parent().and_then(nearest_existing) else {
            continue;
        };
        if !probed.insert(anchor.clone()) {
            continue;
        }
        if !writable_directory(&anchor) {
            return Err(ApplyPlanError::PermissionDenied {
                required: required_permission(&anchor),
                path: anchor,
            });
        }
    }
    Ok(())
}

/// The closest ancestor of `path` that exists, so a not-yet-created destination is still checked.
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if fs::symlink_metadata(candidate).is_ok() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

/// Whether the current user may create entries in `path`.
///
/// `access(2)` honours ACLs, which mode bits alone would miss. A check that fails for any
/// other reason returns `true` and lets the apply surface its own typed error.
fn writable_directory(path: &Path) -> bool {
    match rustix::fs::access(
        path,
        rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
    ) {
        Ok(()) => true,
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
        ),
    }
}

/// Names the permission the user has to grant for `path`, calling out macOS TCC protection.
fn required_permission(path: &Path) -> &'static str {
    let protected = path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| TCC_PROTECTED.contains(&name))
    });
    if cfg!(target_os = "macos") && protected {
        REQUIRED_FULL_DISK_ACCESS
    } else {
        REQUIRED_WRITE_ACCESS
    }
}

/// Canonical hash of one projected store item.
///
/// Rules and skills are their own file or tree, so their bytes hash directly. An MCP server is
/// one entry inside the shared `mcp.json`, so the hash covers only that entry.
fn store_item_hash(
    kind: ItemKind,
    slug: &str,
    canonical_bytes: &[u8],
) -> Result<String, ApplyPlanError> {
    if kind != ItemKind::McpServer {
        return Ok(at_fs::sha256_hex(canonical_bytes));
    }
    let store = at_store::mcp_store::McpStore::parse(canonical_bytes)
        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
    let definition = store.get(slug).ok_or_else(|| {
        ApplyPlanError::Store(format!("the projected mcp.json has no server `{slug}`"))
    })?;
    let value = serde_json::to_value(definition)
        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
    at_store::manifest::hash_mcp_entry(&value)
        .map(|hash| hash.to_string())
        .map_err(|error| ApplyPlanError::Store(error.to_string()))
}
