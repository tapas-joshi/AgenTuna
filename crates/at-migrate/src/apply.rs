//! Build and apply a reviewed migration plan as one journalled ChangeSet.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::undo::{UndoError, UndoOperationRequest, UndoOutcome, undo_operation};
use at_fs::{ApplyContext as FsApplyContext, ApplyRoots, ChangeSetApplyError};
use at_model::{
    ChangeSet, DeployMode, ExpectedState, FileOp, ItemId, ItemKind, ItemState, SyncLabel,
};
use at_store::layout::{Slug, StoreLayout};
use at_store::manifest::{ContentHash, ManifestItem, StoreManifest, StorePath};

use crate::plan::{MigrationAction, MigrationPlan, PlanError, validate_plan};

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
    validate_plan(plan)?;
    preflight_native_access(plan)?;
    let layout = StoreLayout::new(plan.scope, context.store_root);
    let loaded_manifest =
        StoreManifest::load(&layout).map_err(|error| ApplyPlanError::Store(error.to_string()))?;
    let mut manifest = loaded_manifest.manifest;
    let manifest_expected = ExpectedState::hash(loaded_manifest.hash.as_str())
        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
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
                store_root: context.store_root,
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
                push_store_item_ops(source, relative, &mut ops)?;
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
                let item_id = at_db::items::item_id_by_identity(
                    context.database.connection(),
                    ItemKind::Skill,
                    plan.scope,
                    None,
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
                sources.push((source.clone(), destination.clone(), hash.to_owned()));
            }
            MigrationAction::RecordWatched => {}
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
                        .strip_prefix(context.store_root)
                        .map_err(|_| ApplyPlanError::OutsideStore(destination.clone()))?;
                    // Rule files are named `AGENTS`/`CLAUDE`; store slugs are lowercase.
                    let slug_text = projection_item.item_name.to_ascii_lowercase();
                    let slug = Slug::parse(&slug_text)
                        .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
                    let item_id = at_db::items::item_id_by_identity(
                        context.database.connection(),
                        projection_item.kind,
                        plan.scope,
                        None,
                        slug.as_str(),
                    )
                    .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?
                    .unwrap_or_else(ItemId::new);
                    let canonical = projections
                        .canonical_writes
                        .iter()
                        .find(|write| write.path == *destination)
                        .ok_or_else(|| ApplyPlanError::Unsupported(destination.clone()))?;
                    let content_hash = at_fs::sha256_hex(&canonical.bytes);
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
                    let relative = write
                        .path
                        .strip_prefix(context.store_root)
                        .map_err(|_| ApplyPlanError::OutsideStore(write.path.clone()))?;
                    ops.push(FileOp::Write {
                        path: relative.to_path_buf(),
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
    if manifest_changed {
        let manifest_path = layout.manifest_path();
        let relative = manifest_path
            .strip_prefix(context.store_root)
            .map_err(|_| ApplyPlanError::OutsideStore(manifest_path.clone()))?;
        let bytes = manifest
            .to_bytes()
            .map_err(|error| ApplyPlanError::Store(error.to_string()))?;
        ops.push(FileOp::Write {
            path: relative.to_path_buf(),
            bytes,
            expected: manifest_expected,
            follow_symlink: false,
            backup: true,
        });
        touched.push(manifest_path);
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
    let mut outcome = {
        let mut recorder = at_fs::DbOperationRecorder::new(context.database.connection_mut(), 30);
        let mut fs_context = FsApplyContext {
            store_root: context.store_root,
            backup_folder: context.backup_root,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots_reversible(&change_set, &mut fs_context, context.authorized_roots)?
    };
    let warnings = match record_migration_metadata(
        context.database,
        &canonical_items,
        &outcome.operation_id,
    ) {
        Ok(warnings) => warnings,
        Err(error) => {
            if let Some(handle) = outcome.rollback_handle.take() {
                at_fs::rollback_committed(handle)?;
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
        completed: outcome.completed,
        touched_paths: touched,
        warnings,
    })
}

fn record_migration_metadata(
    database: &mut at_db::Database,
    items: &[CanonicalMigrationItem],
    operation_id: &str,
) -> Result<Vec<MigrationWarning>, ApplyPlanError> {
    let transaction = database
        .connection_mut()
        .transaction()
        .map_err(|error| ApplyPlanError::Metadata(error.to_string()))?;
    let mut adopted_paths = std::collections::BTreeSet::new();
    let mut warnings = Vec::new();
    for item in items {
        let source_path = item.store_path.to_string_lossy();
        let spec = at_db::items::AdoptedItemSpec {
            proposed_id: item.id,
            kind: item.kind,
            scope: item.scope,
            project_id: None,
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
        for agent_id in &item.agents {
            let state = if mode == "native" {
                ItemState::Native
            } else {
                ItemState::Linked
            };
            let mut deployment =
                at_db::deployments::DeploymentSpec::new(&item.id, agent_id, item.scope, state)
                    .with_mode(mode)
                    .with_target_path(&target_path)
                    .with_hashes(
                        Some(&item.content_hash),
                        Some(&item.content_hash),
                        Some(&item.content_hash),
                    )
                    .with_sync_label(SyncLabel::InSync)
                    .with_operation(operation_id);
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
