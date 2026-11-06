//! Reviewed skill promote, move and duplicate plans and transactional application.
//!
//! Planning returns content-bound ChangeSets; application journals canonical and native
//! deployment phases, then updates the database identity and deployment index.

use std::fs;
use std::path::{Path, PathBuf};

use at_db::shared::DeploymentCell;
use at_model::{
    ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, ItemState, LinkMode, OperationKind, Scope,
};
use at_store::{
    SkillPackage,
    layout::{Slug, StoreLayout},
    manifest::{ContentHash, ManifestItem, StoreManifest, StorePath, hash_skill_folder},
};
use thiserror::Error;

/// Requested skill transfer action.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkillTransfer {
    /// Copy to a broader scope and retain the source copy.
    Promote,
    /// Copy to the target scope, then remove the source copy after it is backed up.
    Move,
    /// Copy within one scope with a newly allocated identity and slug.
    Duplicate {
        /// Slug assigned to the duplicate.
        slug: Slug,
    },
}

/// Explicit user decision for a destination collision.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClashResolution {
    /// Replace the existing managed target after backup.
    Replace,
    /// Choose a different slug instead of replacing the existing target.
    Rename(Slug),
}

/// Destination collision that blocks planning until explicitly resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverwritePrompt {
    /// Existing skill directory.
    pub target_path: PathBuf,
    /// Existing target package hash.
    pub target_hash: ContentHash,
    /// Source package hash.
    pub source_hash: ContentHash,
}

/// Reviewed skill transfer ChangeSets and identity/deployment information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTransferPlan {
    /// Canonical package and manifest phase.
    pub change_set: ChangeSet,
    /// Native deployment relink phase. It runs only after the canonical target exists.
    pub deployment_change_set: Option<ChangeSet>,
    /// Stable item identity after the operation (same for promote/move, new for duplicate).
    pub target_item_id: ItemId,
    /// Source identity used for DB re-homing or duplicate creation.
    pub source_item_id: ItemId,
    /// Source scope copy remains after apply.
    pub keeps_source: bool,
    /// Source scope of the canonical item before apply.
    pub source_scope: Scope,
    /// Target skill slug.
    pub target_slug: Slug,
    /// Native deployment rows observed during planning.
    pub deployments: Vec<DeploymentCell>,
    /// Target store identity for apply-time DB updates.
    pub target_store_root: PathBuf,
    /// Target project identity for Project and Local items.
    pub target_project_id: Option<String>,
}

/// Failure while preparing a skill transfer plan.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SkillTransferError {
    /// The target Global store requires explicit admin permission.
    #[error("permission denied: writing the Global skill store requires admin permission")]
    PermissionDenied,
    /// Promote must move toward a broader (lower precedence) scope.
    #[error("promote requires a broader target scope")]
    NotBroader,
    /// Duplicate is only valid within the same scope.
    #[error("duplicate requires the same source and target scope")]
    DuplicateScopeMismatch,
    /// Move or promote must change scope.
    #[error("source and target scopes must differ")]
    SameScope,
    /// Source item cannot be found in the supplied manifest.
    #[error("source skill is missing from its store manifest")]
    MissingSource,
    /// An existing target requires a Replace or Rename decision.
    #[error("target skill already exists; explicit Replace or Rename is required")]
    Clash(OverwritePrompt),
    /// A requested Replace has no target item to replace.
    #[error("Replace was selected but no target skill exists")]
    NoTargetToReplace,
    /// A target folder exists without a canonical manifest entry; it is never overwritten.
    #[error("target skill folder is unmanaged and cannot be replaced: {0}")]
    UnmanagedTarget(PathBuf),
    /// The target folder could not be inspected safely.
    #[error("could not inspect target skill folder {path}: {source}")]
    TargetFilesystem {
        /// Target folder being inspected.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// Skill package inspection failed.
    #[error(transparent)]
    Package(#[from] at_store::SkillPackageError),
    /// The canonical skill tree could not be hashed.
    #[error(transparent)]
    Hash(#[from] at_store::HashError),
    /// Manifest validation or serialization failed.
    #[error(transparent)]
    Manifest(#[from] at_store::manifest::ManifestError),
    /// An invalid manifest path was requested.
    #[error(transparent)]
    StorePath(#[from] at_store::manifest::StorePathError),
    /// Invalid expected state or ChangeSet.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// Skill slug validation failed.
    #[error(transparent)]
    Slug(#[from] at_store::layout::SlugError),
    /// Deployment-index query or update failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// A native deployment cannot be safely rebuilt from its stored row.
    #[error("deployment cannot be replanned safely for agent {agent}")]
    DeploymentUnavailable {
        /// Agent with a deployment that cannot be reconstructed.
        agent: at_model::AgentId,
    },
    /// Native deployment planning refused a path.
    #[error(transparent)]
    Deploy(#[from] crate::deploy::DeployPlanError),
    /// A canonical or native ChangeSet failed to apply.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// Phase two failed; phase one was rolled back or rollback itself failed.
    #[error("deployment phase failed: {cause}; canonical rollback: {rollback}")]
    PhaseTwo {
        /// Phase-two failure.
        cause: String,
        /// `complete` or the rollback error.
        rollback: String,
    },
    /// Database item re-homing failed after filesystem phases; filesystem rollback was attempted.
    #[error("database re-home failed: {cause}; canonical rollback: {rollback}")]
    Rehome {
        /// Database failure.
        cause: String,
        /// `complete` or the rollback error.
        rollback: String,
    },
}

/// Inputs to the read-only skill transfer planner.
pub struct SkillTransferRequest<'a> {
    /// Transfer intent.
    pub transfer: SkillTransfer,
    /// Source canonical store layout.
    pub source_store: &'a StoreLayout,
    /// Target canonical store layout.
    pub target_store: &'a StoreLayout,
    /// Stable item identity at the source.
    pub item_id: ItemId,
    /// Parsed source manifest snapshot.
    pub source_manifest: &'a StoreManifest,
    /// Parsed target manifest snapshot.
    pub target_manifest: &'a StoreManifest,
    /// Exact original source manifest bytes, used for concurrency protection.
    pub source_manifest_bytes: &'a [u8],
    /// Exact original target manifest bytes, used for concurrency protection.
    pub target_manifest_bytes: &'a [u8],
    /// Whether the caller has the explicit permission to write Global.
    pub global_permission: bool,
    /// Explicit collision decision, absent until the prompt is answered.
    pub resolution: Option<ClashResolution>,
    /// Database connection used to snapshot item deployment rows.
    pub connection: &'a rusqlite::Connection,
    /// Owning project id when the target is Project or Local scope.
    pub target_project_id: Option<String>,
}

/// Preflights package state and returns a content-bound ChangeSet without writing files.
///
/// Promote and Move retain the source ItemId at the target. Duplicate allocates a new ItemId.
/// Existing targets are refused unless the caller supplies an explicit Replace or Rename choice.
/// Global writes require `global_permission` before any package or manifest work is planned.
///
/// # Errors
/// Returns a typed permission, scope, collision, package, manifest or ChangeSet error.
pub fn plan_skill_transfer(
    request: SkillTransferRequest<'_>,
) -> Result<SkillTransferPlan, SkillTransferError> {
    let SkillTransferRequest {
        transfer,
        source_store,
        target_store,
        item_id,
        source_manifest,
        target_manifest,
        source_manifest_bytes,
        target_manifest_bytes,
        global_permission,
        resolution,
        connection,
        target_project_id,
    } = request;
    let deployments = at_db::deployments::list_deployments_for_item(connection, &item_id)?;
    if (target_manifest.scope() == Scope::Global || source_manifest.scope() == Scope::Global)
        && !global_permission
    {
        return Err(SkillTransferError::PermissionDenied);
    }
    let source_item = source_manifest
        .item(item_id)
        .filter(|item| item.kind == ItemKind::Skill)
        .ok_or(SkillTransferError::MissingSource)?
        .clone();
    if source_manifest.scope() != source_store.scope()
        || target_manifest.scope() != target_store.scope()
    {
        return Err(SkillTransferError::MissingSource);
    }
    let source_path = source_store.root().join(source_item.path.as_str());
    let loaded = SkillPackage::read(&source_path)?;
    let source_hash = loaded.hash;

    let (mut target_slug, target_item_id, keeps_source, operation) = match transfer {
        SkillTransfer::Promote => {
            if target_store.scope().precedence() >= source_store.scope().precedence() {
                return Err(SkillTransferError::NotBroader);
            }
            (source_item.slug.clone(), item_id, true, "promote")
        }
        SkillTransfer::Move => {
            if source_store.scope() == target_store.scope() {
                return Err(SkillTransferError::SameScope);
            }
            (source_item.slug.clone(), item_id, false, "move")
        }
        SkillTransfer::Duplicate { slug } => {
            if source_store.scope() != target_store.scope() {
                return Err(SkillTransferError::DuplicateScopeMismatch);
            }
            (slug, ItemId::new(), true, "edit_skill")
        }
    };
    let mut target_item = target_manifest
        .item_by_slug(ItemKind::Skill, &target_slug)
        .cloned();
    let mut target_path = target_store.skill_dir(&target_slug);
    if target_item.is_none() {
        match fs::symlink_metadata(&target_path) {
            Ok(_) => return Err(SkillTransferError::UnmanagedTarget(target_path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SkillTransferError::TargetFilesystem {
                    path: target_path,
                    source,
                });
            }
        }
    }
    if target_item.is_some() {
        match resolution {
            Some(ClashResolution::Replace) => {}
            Some(ClashResolution::Rename(slug)) => {
                target_slug = slug;
                target_item = target_manifest
                    .item_by_slug(ItemKind::Skill, &target_slug)
                    .cloned();
                target_path = target_store.skill_dir(&target_slug);
                if let Some(rename_clash) = &target_item {
                    return Err(SkillTransferError::Clash(OverwritePrompt {
                        target_path,
                        target_hash: rename_clash.hash.clone(),
                        source_hash,
                    }));
                }
            }
            None => {
                let target_hash = hash_skill_folder(&target_path)?;
                return Err(SkillTransferError::Clash(OverwritePrompt {
                    target_path,
                    target_hash,
                    source_hash,
                }));
            }
        }
    } else if matches!(resolution, Some(ClashResolution::Replace)) {
        return Err(SkillTransferError::NoTargetToReplace);
    }
    let target_relative = StorePath::parse(&format!("skills/{target_slug}"))?;
    let mut source_next = source_manifest.clone();
    let mut target_next = target_manifest.clone();
    if let Some(replaced) = &target_item {
        target_next.remove_item(replaced.id);
    }
    let target_record = ManifestItem {
        id: target_item_id,
        kind: ItemKind::Skill,
        slug: target_slug.clone(),
        path: target_relative,
        hash: source_hash.clone(),
        name: source_item.name.clone(),
        source: source_item.source.clone(),
    };
    target_next
        .insert_item(target_record)
        .map_err(at_store::manifest::ManifestError::Validation)?;
    if !keeps_source {
        source_next.remove_item(item_id);
    }

    let existing_target = target_item
        .as_ref()
        .map(|_| SkillPackage::read(&target_path))
        .transpose()?;
    let mut ops = Vec::new();
    if target_item.is_none() {
        ops.push(FileOp::Copy {
            source: source_path.clone(),
            dest: target_path.clone(),
            expected: ExpectedState::absent(),
        });
    } else if let Some(existing) = &existing_target {
        for relative in loaded.package.paths() {
            let file = loaded
                .package
                .get(relative)
                .ok_or(SkillTransferError::MissingSource)?;
            let previous = existing.package.get(relative);
            if previous.is_some_and(|old| old.bytes == file.bytes) {
                continue;
            }
            ops.push(FileOp::Write {
                path: target_path.join(relative),
                bytes: file.bytes.clone(),
                expected: match previous {
                    Some(old) => ExpectedState::hash(at_fs::sha256_hex(&old.bytes))?,
                    None => ExpectedState::absent(),
                },
                follow_symlink: false,
                backup: true,
            });
        }
        for relative in existing.package.paths() {
            if loaded.package.get(relative).is_none() {
                let file = existing
                    .package
                    .get(relative)
                    .ok_or(SkillTransferError::MissingSource)?;
                ops.push(FileOp::Delete {
                    path: target_path.join(relative),
                    expected: ExpectedState::hash(at_fs::sha256_hex(&file.bytes))?,
                });
            }
        }
    }
    ops.push(FileOp::Write {
        path: target_store.manifest_path(),
        bytes: target_next.to_bytes()?,
        expected: ExpectedState::hash(at_fs::sha256_hex(target_manifest_bytes))?,
        follow_symlink: false,
        backup: true,
    });
    if !keeps_source {
        ops.push(FileOp::Delete {
            path: source_path.clone(),
            expected: ExpectedState::hash(node_hash(&source_path).map_err(|source| {
                SkillTransferError::TargetFilesystem {
                    path: source_path.clone(),
                    source,
                }
            })?)?,
        });
        ops.push(FileOp::Write {
            path: source_store.manifest_path(),
            bytes: source_next.to_bytes()?,
            expected: ExpectedState::hash(at_fs::sha256_hex(source_manifest_bytes))?,
            follow_symlink: false,
            backup: true,
        });
    }
    let operation_id = format!("op_{}", uuid::Uuid::now_v7());
    let preview_id = format!("preview_{}", uuid::Uuid::now_v7());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let change_set = ChangeSet {
        id: operation_id.clone(),
        preview_id: preview_id.clone(),
        operation_kind: OperationKind::parse(operation)?,
        scope: Some(target_store.scope()),
        project_id: target_project_id.clone(),
        summary: format!("{operation} {} to {}", source_item.slug, target_slug),
        ops,
        created_at_unix_seconds: now,
    };
    change_set.validate()?;
    let is_duplicate = operation == "edit_skill";
    let deployment_change_set = if is_duplicate {
        None
    } else {
        plan_deployment_relink(DeploymentRelinkRequest {
            deployments: &deployments,
            source_scope: source_store.scope(),
            target_scope: target_store.scope(),
            item_id,
            canonical_target: &target_path,
            target_store,
            target_project_id: target_project_id.as_deref(),
            operation_id: &operation_id,
            preview_id: &preview_id,
            now,
        })?
    };
    Ok(SkillTransferPlan {
        change_set,
        deployment_change_set,
        target_item_id,
        source_item_id: item_id,
        keeps_source,
        source_scope: source_store.scope(),
        target_slug,
        deployments,
        target_store_root: target_store.root().to_path_buf(),
        target_project_id,
    })
}

struct DeploymentRelinkRequest<'a> {
    deployments: &'a [DeploymentCell],
    source_scope: Scope,
    target_scope: Scope,
    item_id: ItemId,
    canonical_target: &'a Path,
    target_store: &'a StoreLayout,
    target_project_id: Option<&'a str>,
    operation_id: &'a str,
    preview_id: &'a str,
    now: u64,
}

fn plan_deployment_relink(
    request: DeploymentRelinkRequest<'_>,
) -> Result<Option<ChangeSet>, SkillTransferError> {
    let DeploymentRelinkRequest {
        deployments,
        source_scope,
        target_scope,
        item_id,
        canonical_target,
        target_store,
        target_project_id,
        operation_id,
        preview_id,
        now,
    } = request;
    let mut seen_targets = std::collections::BTreeSet::new();
    let mut ops = Vec::new();
    for cell in deployments
        .iter()
        .filter(|cell| cell.scope == source_scope && cell.state == ItemState::Linked)
    {
        let target = cell
            .target_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            })?;
        if !seen_targets.insert(target.clone()) {
            continue;
        }
        let backup_name =
            target
                .file_name()
                .ok_or_else(|| SkillTransferError::DeploymentUnavailable {
                    agent: cell.agent_id.clone(),
                })?;
        let backup = target_store
            .backups_dir()
            .join(operation_id)
            .join("native")
            .join(cell.agent_id.to_string())
            .join(backup_name);
        let expected = ExpectedState::hash(node_hash(&target).map_err(|source| {
            SkillTransferError::TargetFilesystem {
                path: target.clone(),
                source,
            }
        })?)?;
        ops.push(FileOp::Move {
            from: target.clone(),
            to: backup,
            expected,
        });
        let mode = cell
            .link_mode
            .ok_or_else(|| SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            })?;
        let plan = crate::deploy::plan(crate::deploy::PlanRequest {
            item_id,
            scope: target_scope,
            agent: cell.agent_id.clone(),
            store_root: target_store.root().to_path_buf(),
            source: canonical_target.to_path_buf(),
            destination: target,
            mode,
            link_supported: true,
            managed: true,
            destination_managed: true,
            shared_agents: Vec::new(),
            project_id: target_project_id.map(str::to_owned),
            summary: format!("Relink skill for {}", cell.agent_id),
        })?;
        let Some(native_set) = plan.change_set else {
            return Err(SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            });
        };
        ops.extend(native_set.ops);
    }
    if ops.is_empty() {
        return Ok(None);
    }
    let set = ChangeSet {
        id: format!("{operation_id}-deploy"),
        preview_id: preview_id.to_owned(),
        operation_kind: OperationKind::parse("deploy")?,
        scope: Some(target_scope),
        project_id: target_project_id.map(str::to_owned),
        summary: "Relink skill deployments after scope transfer".to_owned(),
        ops,
        created_at_unix_seconds: now,
    };
    set.validate()?;
    Ok(Some(set))
}

/// Durable ids returned by applying one transfer and its native deployment updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTransferOutcome {
    /// Canonical package/manifest operation id.
    pub canonical_operation_id: String,
    /// Native deployment operation ids.
    pub deployment_operation_ids: Vec<String>,
}

/// Applies both reviewed transfer phases and re-homes the indexed item after filesystem success.
///
/// The apply context must use the target store root so native link targets resolve in phase two.
/// If deployment relinking or database re-homing fails, completed filesystem phases are rolled
/// back in reverse order before returning.
///
/// # Errors
/// Returns typed ChangeSet, database or rollback errors.
pub fn apply_skill_transfer(
    connection: &rusqlite::Connection,
    plan: &SkillTransferPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
) -> Result<SkillTransferOutcome, SkillTransferError> {
    if context.store_root != plan.target_store_root {
        return Err(at_model::ChangeSetError::InvalidPath {
            path: context.store_root.display().to_string(),
            reason: "apply context must use the target store root".to_owned(),
        }
        .into());
    }
    let canonical = at_fs::apply_with_roots_reversible(&plan.change_set, context, roots)?;
    let canonical_handle =
        canonical
            .rollback_handle
            .ok_or_else(|| SkillTransferError::PhaseTwo {
                cause: "canonical apply did not return a rollback token".to_owned(),
                rollback: "not attempted".to_owned(),
            })?;
    let mut handles = vec![canonical_handle];
    let mut deployment_ids = Vec::new();
    if let Some(deployment_set) = &plan.deployment_change_set {
        let (detach_ops, install_ops): (Vec<_>, Vec<_>) = deployment_set
            .ops
            .iter()
            .cloned()
            .partition(|op| matches!(op, FileOp::Move { .. }));
        if !detach_ops.is_empty() {
            let detach = phase_set(deployment_set, "-detach", detach_ops)?;
            match at_fs::apply_with_roots_reversible(&detach, context, roots) {
                Ok(outcome) => {
                    deployment_ids.push(outcome.operation_id);
                    if let Some(handle) = outcome.rollback_handle {
                        handles.push(handle);
                    }
                }
                Err(error) => {
                    return Err(SkillTransferError::PhaseTwo {
                        cause: error.to_string(),
                        rollback: rollback_handles(handles),
                    });
                }
            }
        }
        if !install_ops.is_empty() {
            let install = phase_set(deployment_set, "-install", install_ops)?;
            match at_fs::apply_with_roots_reversible(&install, context, roots) {
                Ok(outcome) => {
                    deployment_ids.push(outcome.operation_id);
                    if let Some(handle) = outcome.rollback_handle {
                        handles.push(handle);
                    }
                }
                Err(error) => {
                    return Err(SkillTransferError::PhaseTwo {
                        cause: error.to_string(),
                        rollback: rollback_handles(handles),
                    });
                }
            }
        }
    }
    let target_scope = plan.change_set.scope.unwrap_or(plan.source_scope);
    connection
        .execute_batch("SAVEPOINT skill_transfer_db")
        .map_err(at_db::DbError::Sqlite)?;
    let database_update =
        (|| -> Result<(), at_db::DbError> {
            if plan.target_item_id == plan.source_item_id {
                at_db::items::rehome_item(
                    connection,
                    &plan.source_item_id,
                    target_scope,
                    plan.target_slug.as_str(),
                    &format!("skills/{}", plan.target_slug),
                    plan.target_project_id.as_deref(),
                )?;
            } else {
                at_db::items::duplicate_item(
                    connection,
                    &plan.source_item_id,
                    &plan.target_item_id,
                    plan.target_slug.as_str(),
                    &format!("skills/{}", plan.target_slug),
                    target_scope,
                    plan.target_project_id.as_deref(),
                )?;
            }
            if plan.target_item_id == plan.source_item_id {
                for cell in plan.deployments.iter().filter(|cell| {
                    cell.scope == plan.source_scope && cell.state == ItemState::Linked
                }) {
                    update_deployment_scope(connection, plan, cell, &canonical.operation_id)?;
                }
            }
            Ok(())
        })();
    if let Err(error) = database_update {
        let rollback_error = connection
            .execute_batch("ROLLBACK TO skill_transfer_db; RELEASE skill_transfer_db")
            .err();
        let files = rollback_handles(handles);
        return Err(SkillTransferError::Rehome {
            cause: match rollback_error {
                Some(rollback_error) => {
                    format!("{error}; database rollback failed: {rollback_error}")
                }
                None => error.to_string(),
            },
            rollback: files,
        });
    }
    connection
        .execute_batch("RELEASE skill_transfer_db")
        .map_err(at_db::DbError::Sqlite)?;
    for handle in handles {
        at_fs::commit_reversible(handle)?;
    }
    Ok(SkillTransferOutcome {
        canonical_operation_id: canonical.operation_id,
        deployment_operation_ids: deployment_ids,
    })
}

fn phase_set(
    source: &ChangeSet,
    suffix: &str,
    ops: Vec<FileOp>,
) -> Result<ChangeSet, SkillTransferError> {
    let mut set = source.clone();
    set.id.push_str(suffix);
    set.ops = ops;
    set.summary = format!("{}{suffix}", source.summary);
    set.validate()?;
    Ok(set)
}

fn rollback_handles(handles: Vec<at_fs::RollbackHandle>) -> String {
    let mut errors = Vec::new();
    for handle in handles.into_iter().rev() {
        if let Err(error) = at_fs::rollback_committed(handle) {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        "complete".to_owned()
    } else {
        errors.join("; ")
    }
}

fn update_deployment_scope(
    connection: &rusqlite::Connection,
    plan: &SkillTransferPlan,
    cell: &DeploymentCell,
    operation_id: &str,
) -> Result<(), at_db::DbError> {
    let old_project = cell.project_id.as_deref();
    let off = at_db::deployments::DeploymentSpec::new(
        &plan.target_item_id,
        &cell.agent_id,
        plan.source_scope,
        ItemState::Off,
    )
    .with_mode("none")
    .with_operation(operation_id);
    let off = if let Some(project) = old_project {
        off.with_project(project)
    } else {
        off
    };
    at_db::deployments::upsert_deployment(connection, &off)?;
    let mode = match cell.link_mode {
        Some(LinkMode::Link) => "symlink",
        Some(LinkMode::Copy) => "copy",
        None => "none",
    };
    let mut target = at_db::deployments::DeploymentSpec::new(
        &plan.target_item_id,
        &cell.agent_id,
        plan.change_set.scope.unwrap_or(plan.source_scope),
        ItemState::Linked,
    )
    .with_mode(mode)
    .with_hashes(
        cell.store_hash.as_deref(),
        cell.target_hash.as_deref(),
        cell.baseline_hash.as_deref(),
    )
    .with_operation(operation_id);
    if let Some(path) = cell.target_path.as_deref() {
        target = target.with_target_path(path);
    }
    let link = (mode == "symlink").then(|| {
        plan.target_store_root
            .join("skills")
            .join(plan.target_slug.as_str())
            .to_string_lossy()
            .into_owned()
    });
    if let Some(link) = link.as_deref() {
        target = target.with_link_target(link);
    }
    if let Some(project) = plan.target_project_id.as_deref() {
        target = target.with_project(project);
    }
    at_db::deployments::upsert_deployment(connection, &target)
}
/// Computes the filesystem ChangeSet node digest used for directory preconditions.
///
/// This mirrors at-fs's node digest: sorted child names, NUL separators, child digests and
/// `0xff` separators. Package reads reject symlinks inside the skill tree.
fn node_hash(path: &Path) -> std::io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return fs::read(path).map(|bytes| at_fs::sha256_hex(&bytes));
    }
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.file_name().as_encoded_bytes());
            bytes.push(0);
            bytes.extend_from_slice(node_hash(&entry.path())?.as_bytes());
            bytes.push(0xff);
        }
        return Ok(at_fs::sha256_hex(&bytes));
    }
    if metadata.file_type().is_symlink() {
        return fs::read_link(path)
            .map(|target| at_fs::sha256_hex(target.as_os_str().as_encoded_bytes()));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "unsupported skill filesystem entry",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use at_store::{layout::StoreId, manifest::hash_skill_folder};

    struct Fixture {
        _temp: tempfile::TempDir,
        database: at_db::Database,
        source_store: StoreLayout,
        target_store: StoreLayout,
        source_manifest: StoreManifest,
        target_manifest: StoreManifest,
        source_bytes: Vec<u8>,
        target_bytes: Vec<u8>,
        id: ItemId,
        slug: Slug,
    }

    fn fixture(source_scope: Scope, target_scope: Scope, target_exists: bool) -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let database = at_db::Database::open_in_memory().expect("database");
        let source_store = StoreLayout::new(source_scope, temp.path().join("source"));
        let target_store = StoreLayout::new(target_scope, temp.path().join("target"));
        fs::create_dir_all(source_store.root().join("skills/review")).expect("source dirs");
        fs::write(
            source_store.root().join("skills/review/SKILL.md"),
            b"---\nname: review\ndescription: Review changes\n---\n",
        )
        .expect("source skill");
        let id = ItemId::new();
        let slug = Slug::parse("review").expect("slug");
        let hash = hash_skill_folder(&source_store.skill_dir(&slug)).expect("source hash");
        let source_record = ManifestItem::new(
            id,
            ItemKind::Skill,
            slug.clone(),
            StorePath::parse("skills/review").expect("source path"),
            hash,
        );
        let mut source_manifest = StoreManifest::new(source_scope, StoreId::new());
        source_manifest
            .insert_item(source_record)
            .expect("source item");
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name, source_path) \
                 VALUES (?1, 'skill', ?2, ?3, ?3, 'skills/review')",
                rusqlite::params![id.to_string(), source_scope.wire_name(), slug.as_str()],
            )
            .expect("database item");
        let source_bytes = source_manifest.to_bytes().expect("source manifest");

        let mut target_manifest = StoreManifest::new(target_scope, StoreId::new());
        if target_exists {
            fs::create_dir_all(target_store.root().join("skills/review")).expect("target dirs");
            fs::write(
                target_store.root().join("skills/review/SKILL.md"),
                b"---\nname: review\ndescription: Existing target\n---\n",
            )
            .expect("target skill");
            let target_hash =
                hash_skill_folder(&target_store.skill_dir(&slug)).expect("target hash");
            target_manifest
                .insert_item(ManifestItem::new(
                    ItemId::new(),
                    ItemKind::Skill,
                    slug.clone(),
                    StorePath::parse("skills/review").expect("target path"),
                    target_hash,
                ))
                .expect("target item");
        }
        let target_bytes = target_manifest.to_bytes().expect("target manifest");
        Fixture {
            database,
            _temp: temp,
            source_store,
            target_store,
            source_manifest,
            target_manifest,
            source_bytes,
            target_bytes,
            id,
            slug,
        }
    }

    fn request<'a>(
        fixture: &'a Fixture,
        transfer: SkillTransfer,
        resolution: Option<ClashResolution>,
        global_permission: bool,
    ) -> SkillTransferRequest<'a> {
        SkillTransferRequest {
            transfer,
            source_store: &fixture.source_store,
            target_store: &fixture.target_store,
            item_id: fixture.id,
            source_manifest: &fixture.source_manifest,
            target_manifest: &fixture.target_manifest,
            source_manifest_bytes: &fixture.source_bytes,
            target_manifest_bytes: &fixture.target_bytes,
            global_permission,
            resolution,
            connection: fixture.database.connection(),
            target_project_id: None,
        }
    }

    #[test]
    fn promote_preserves_identity_and_source_in_reviewed_set() {
        let fixture = fixture(Scope::Project, Scope::User, false);
        let plan = plan_skill_transfer(request(&fixture, SkillTransfer::Promote, None, false))
            .expect("promote plan");
        assert_eq!(plan.target_item_id, fixture.id);
        assert!(plan.keeps_source);
        assert_eq!(plan.target_slug, fixture.slug);
        assert!(plan.change_set.ops.iter().any(|op| matches!(
            op,
            FileOp::Copy { dest, .. } if dest == &fixture.target_store.skill_dir(&fixture.slug)
        )));
    }

    #[test]
    fn move_copies_target_and_removes_source_in_one_changeset() {
        let fixture = fixture(Scope::Project, Scope::User, false);
        let plan = plan_skill_transfer(request(&fixture, SkillTransfer::Move, None, false))
            .expect("move plan");
        assert!(!plan.keeps_source);
        assert!(plan.change_set.ops.iter().any(|op| matches!(
            op,
            FileOp::Delete { path, .. } if path == &fixture.source_store.skill_dir(&fixture.slug)
        )));
        assert!(plan.change_set.ops.iter().any(|op| matches!(
            op,
            FileOp::Copy { dest, .. } if dest == &fixture.target_store.skill_dir(&fixture.slug)
        )));
    }

    #[test]
    fn target_collision_requires_replace_or_rename_and_reports_hashes() {
        let fixture = fixture(Scope::Project, Scope::User, true);
        let error = plan_skill_transfer(request(&fixture, SkillTransfer::Promote, None, false))
            .expect_err("unconfirmed collision");
        let SkillTransferError::Clash(prompt) = error else {
            panic!("expected clash report");
        };
        assert_eq!(
            prompt.target_path,
            fixture.target_store.skill_dir(&fixture.slug)
        );
        assert!(
            plan_skill_transfer(request(
                &fixture,
                SkillTransfer::Promote,
                Some(ClashResolution::Replace),
                false,
            ))
            .is_ok()
        );
    }

    #[test]
    fn duplicate_allocates_identity_and_existing_slug_clashes() {
        let duplicate_fixture = fixture(Scope::User, Scope::User, false);
        let target_slug = Slug::parse("review-2").expect("target slug");
        let plan = plan_skill_transfer(request(
            &duplicate_fixture,
            SkillTransfer::Duplicate {
                slug: target_slug.clone(),
            },
            None,
            false,
        ))
        .expect("duplicate plan");
        assert_ne!(plan.target_item_id, duplicate_fixture.id);
        assert_eq!(plan.target_slug, target_slug);
        let collision = fixture(Scope::User, Scope::User, true);
        assert!(matches!(
            plan_skill_transfer(request(
                &collision,
                SkillTransfer::Duplicate {
                    slug: collision.slug.clone(),
                },
                None,
                false,
            )),
            Err(SkillTransferError::Clash(_))
        ));
    }

    #[test]
    fn global_permission_is_checked_before_planning_writes() {
        let fixture = fixture(Scope::User, Scope::Global, false);
        assert!(matches!(
            plan_skill_transfer(request(&fixture, SkillTransfer::Move, None, false,)),
            Err(SkillTransferError::PermissionDenied)
        ));
    }

    #[test]
    fn replacement_records_target_backup_expectation_and_manifest_write() {
        let fixture = fixture(Scope::Project, Scope::User, true);
        let plan = plan_skill_transfer(request(
            &fixture,
            SkillTransfer::Promote,
            Some(ClashResolution::Replace),
            false,
        ))
        .expect("replace plan");
        assert!(plan.change_set.ops.iter().any(|op| matches!(
            op,
            FileOp::Write { path, backup: true, expected: ExpectedState::Hash { .. }, .. }
                if path == &fixture.target_store.skill_dir(&fixture.slug).join("SKILL.md")
        )));
        assert!(plan.change_set.ops.iter().any(|op| matches!(
            op,
            FileOp::Write { path, backup: true, .. }
                if path == &fixture.target_store.manifest_path()
        )));
    }

    #[test]
    fn apply_move_rehomes_identity_after_canonical_change_set() {
        let fixture = fixture(Scope::Project, Scope::User, false);
        let plan = plan_skill_transfer(request(&fixture, SkillTransfer::Move, None, false))
            .expect("move plan");
        fs::create_dir_all(fixture.target_store.root()).expect("target store directory");
        fs::write(fixture.source_store.manifest_path(), &fixture.source_bytes)
            .expect("persist source manifest");
        fs::write(fixture.target_store.manifest_path(), &fixture.target_bytes)
            .expect("persist target manifest");
        let backup = fixture.target_store.backups_dir();
        fs::create_dir_all(&backup).expect("backup directory");
        let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: fixture.target_store.root(),
            backup_folder: &backup,
            recorder: &mut recorder,
        };
        let roots =
            at_fs::ApplyRoots::new([fixture._temp.path().to_path_buf()]).expect("authorized root");
        let result =
            apply_skill_transfer(fixture.database.connection(), &plan, &mut context, &roots)
                .expect("apply transfer");

        assert!(!fixture.source_store.skill_dir(&fixture.slug).exists());
        assert!(
            fixture
                .target_store
                .skill_dir(&fixture.slug)
                .join("SKILL.md")
                .exists()
        );
        assert!(!result.canonical_operation_id.is_empty());
        let (scope, slug): (String, String) = fixture
            .database
            .connection()
            .query_row(
                "SELECT scope, slug FROM items WHERE id = ?1",
                [fixture.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read moved item");
        assert_eq!(scope, Scope::User.wire_name());
        assert_eq!(slug, fixture.slug.as_str());
    }
}
