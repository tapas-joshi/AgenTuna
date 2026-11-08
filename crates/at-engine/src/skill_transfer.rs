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
    /// Existing managed item replaced at the target scope, when Replace was chosen.
    ///
    /// Apply removes this row so the promoted identity is unique at the target scope.
    pub replaced_item_id: Option<ItemId>,
    /// Source scope copy remains after apply.
    pub keeps_source: bool,
    /// Source scope of the canonical item before apply.
    pub source_scope: Scope,
    /// Target skill slug.
    pub target_slug: Slug,
    /// Native deployment rows observed during planning.
    pub deployments: Vec<DeploymentCell>,
    /// Every enabled agent's native link, resolved to its target scope path.
    ///
    /// Apply writes one deployment row per entry and marks the detached source rows off, so the
    /// index describes exactly the paths the plan materialises.
    pub native_deployments: Vec<PlannedNativeDeployment>,
    /// Target store identity for apply-time DB updates.
    pub target_store_root: PathBuf,
    /// Target project identity for Project and Local items.
    pub target_project_id: Option<String>,
}

/// One enabled agent whose native skill link must follow the item into the target scope.
///
/// The caller resolves `target_root` from the agent adapter, because the native skills directory
/// differs per agent and per scope (for example Codex and omp share `<project>/.agents/skills`
/// while Claude Code reads `<project>/.claude/skills`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTransferTarget {
    /// Agent whose source-scope deployment moves.
    pub agent: at_model::AgentId,
    /// Absolute native skill path recorded by the source-scope deployment row.
    pub source_path: PathBuf,
    /// Native skills directory the agent reads in the target scope.
    pub target_root: PathBuf,
    /// Materialisation recorded by the source deployment.
    pub mode: LinkMode,
    /// Whether the target filesystem supports symlinks.
    pub link_supported: bool,
}

/// One resolved native deployment: where the source link is now and where the target scope needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedNativeDeployment {
    /// Agent whose deployment moves.
    pub agent: at_model::AgentId,
    /// Absolute native skill path recorded by the source-scope deployment row.
    pub source_path: PathBuf,
    /// Absolute native skill path in the target scope.
    pub target_path: PathBuf,
    /// Materialisation to write (already downgraded to copy when links are unsupported).
    pub mode: LinkMode,
    /// Whether the source entry moves into the operation backups.
    ///
    /// A move always detaches it. A promote keeps the source package but still has to detach the
    /// link when both scopes map to the same native directory: one path cannot serve two stores.
    pub detached: bool,
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
    /// Native link destinations for every agent enabled at the source scope.
    ///
    /// A move must remove the source-scope links and deploy the item where the agents read it in
    /// the target scope; an empty list means no agent is enabled and nothing native is planned.
    pub native_targets: &'a [NativeTransferTarget],
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
        native_targets,
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
    let replaced_item_id = target_item.as_ref().map(|item| item.id);
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
    let is_duplicate = operation == "edit_skill";
    let native_deployments = if is_duplicate {
        Vec::new()
    } else {
        resolve_native_deployments(NativeDeploymentRequest {
            targets: native_targets,
            deployments: &deployments,
            source_scope: source_store.scope(),
            target_slug: &target_slug,
            keeps_source,
        })?
    };
    // Removing the source-scope link belongs to the canonical move, not to the install phase:
    // undoing one operation then restores the source package and its link together, so no
    // intermediate state leaves a link pointing at a package that does not exist yet.
    let mut detached_sources = std::collections::BTreeSet::new();
    for deployment in &native_deployments {
        if !deployment.detached || !detached_sources.insert(deployment.source_path.clone()) {
            continue;
        }
        let expected =
            ExpectedState::hash(node_hash(&deployment.source_path).map_err(|source| {
                SkillTransferError::TargetFilesystem {
                    path: deployment.source_path.clone(),
                    source,
                }
            })?)?;
        ops.push(FileOp::Delete {
            path: deployment.source_path.clone(),
            expected,
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
        summary: format!(
            "{operation} {}: {} -> {}",
            source_item.slug,
            source_path.display(),
            target_path.display()
        ),
        ops,
        created_at_unix_seconds: now,
    };
    change_set.validate()?;
    let deployment_change_set = if is_duplicate {
        None
    } else {
        plan_native_deployment_change_set(DeploymentRelinkRequest {
            deployments: &native_deployments,
            target_scope: target_store.scope(),
            item_id,
            canonical_target: &target_path,
            target_store_root: target_store.root(),
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
        replaced_item_id,
        keeps_source,
        source_scope: source_store.scope(),
        target_slug,
        deployments,
        native_deployments,
        target_store_root: target_store.root().to_path_buf(),
        target_project_id,
    })
}

struct NativeDeploymentRequest<'a> {
    targets: &'a [NativeTransferTarget],
    deployments: &'a [DeploymentCell],
    source_scope: Scope,
    target_slug: &'a Slug,
    keeps_source: bool,
}

/// Pairs every enabled source-scope deployment with the native path the target scope needs.
///
/// A linked source row without a caller-supplied target root cannot be moved safely: dropping it
/// would leave the agent reading a store the item no longer occupies, so planning refuses instead.
fn resolve_native_deployments(
    request: NativeDeploymentRequest<'_>,
) -> Result<Vec<PlannedNativeDeployment>, SkillTransferError> {
    let mut planned = Vec::new();
    for cell in request
        .deployments
        .iter()
        .filter(|cell| cell.scope == request.source_scope && cell.state == ItemState::Linked)
    {
        let source_path = cell
            .target_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            })?;
        let target = request
            .targets
            .iter()
            .find(|target| target.agent == cell.agent_id && target.source_path == source_path)
            .ok_or_else(|| SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            })?;
        if !target.target_root.is_absolute() {
            return Err(SkillTransferError::DeploymentUnavailable {
                agent: cell.agent_id.clone(),
            });
        }
        let target_path = target.target_root.join(request.target_slug.as_str());
        let mode = match target.mode {
            LinkMode::Link if !target.link_supported => LinkMode::Copy,
            mode => mode,
        };
        planned.push(PlannedNativeDeployment {
            agent: cell.agent_id.clone(),
            detached: !request.keeps_source || target_path == source_path,
            source_path,
            target_path,
            mode,
        });
    }
    Ok(planned)
}

struct DeploymentRelinkRequest<'a> {
    deployments: &'a [PlannedNativeDeployment],
    target_scope: Scope,
    item_id: ItemId,
    canonical_target: &'a Path,
    target_store_root: &'a Path,
    target_project_id: Option<&'a str>,
    operation_id: &'a str,
    preview_id: &'a str,
    now: u64,
}

/// Plans the native install phase: materialise the item where the target scope's agents read it.
///
/// The source-scope entries are detached by the canonical ChangeSet, so this set only creates
/// target-scope entries. Entries that share a native path (Codex and omp on `.agents/skills`)
/// produce one operation, not one per agent.
fn plan_native_deployment_change_set(
    request: DeploymentRelinkRequest<'_>,
) -> Result<Option<ChangeSet>, SkillTransferError> {
    let DeploymentRelinkRequest {
        deployments,
        target_scope,
        item_id,
        canonical_target,
        target_store_root,
        target_project_id,
        operation_id,
        preview_id,
        now,
    } = request;
    let mut seen_targets = std::collections::BTreeSet::new();
    let mut ops = Vec::new();
    for deployment in deployments {
        if !seen_targets.insert(deployment.target_path.clone()) {
            continue;
        }
        let plan = crate::deploy::plan(crate::deploy::PlanRequest {
            item_id,
            scope: target_scope,
            agent: deployment.agent.clone(),
            store_root: target_store_root.to_path_buf(),
            source: canonical_target.to_path_buf(),
            destination: deployment.target_path.clone(),
            mode: deployment.mode,
            link_supported: true,
            managed: true,
            // The only destination AgenTuna may already own here is the source link this plan
            // detaches first; every other existing path is a conflict the deploy planner reports.
            destination_managed: deployment.detached
                && deployment.target_path == deployment.source_path,
            shared_agents: Vec::new(),
            project_id: target_project_id.map(str::to_owned),
            summary: format!("Relink skill for {}", deployment.agent),
        })?;
        let Some(native_set) = plan.change_set else {
            return Err(SkillTransferError::DeploymentUnavailable {
                agent: deployment.agent.clone(),
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
    apply_skill_transfer_inner(connection, plan, context, roots, None)
}

/// Like [`apply_skill_transfer`], registering expected writes before atomic replacements.
pub fn apply_skill_transfer_with_registry(
    connection: &rusqlite::Connection,
    plan: &SkillTransferPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    registry: &at_fs::RecentWrites,
) -> Result<SkillTransferOutcome, SkillTransferError> {
    apply_skill_transfer_inner(connection, plan, context, roots, Some(registry))
}

fn apply_skill_transfer_inner(
    connection: &rusqlite::Connection,
    plan: &SkillTransferPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
    registry: Option<&at_fs::RecentWrites>,
) -> Result<SkillTransferOutcome, SkillTransferError> {
    if context.store_root != plan.target_store_root {
        return Err(at_model::ChangeSetError::InvalidPath {
            path: context.store_root.display().to_string(),
            reason: "apply context must use the target store root".to_owned(),
        }
        .into());
    }
    let canonical = match registry {
        Some(registry) => at_fs::apply_with_roots_reversible_and_registry(
            &plan.change_set,
            context,
            roots,
            registry,
        )?,
        None => at_fs::apply_with_roots_reversible(&plan.change_set, context, roots)?,
    };
    let canonical_handle =
        canonical
            .rollback_handle
            .ok_or_else(|| SkillTransferError::PhaseTwo {
                cause: "canonical apply did not return a rollback token".to_owned(),
                rollback: "not attempted".to_owned(),
            })?;
    let mut handles = vec![canonical_handle];
    let mut deployment_ids = Vec::new();
    let mut install_operation = None;
    if let Some(deployment_set) = &plan.deployment_change_set {
        let phase_two = match registry {
            Some(registry) => at_fs::apply_with_roots_reversible_and_registry(
                deployment_set,
                context,
                roots,
                registry,
            ),
            None => at_fs::apply_with_roots_reversible(deployment_set, context, roots),
        };
        match phase_two {
            Ok(outcome) => {
                install_operation = Some(outcome.operation_id.clone());
                deployment_ids.push(outcome.operation_id);
                if let Some(handle) = outcome.rollback_handle {
                    handles.push(handle);
                }
            }
            Err(error) => {
                return Err(SkillTransferError::PhaseTwo {
                    cause: error.to_string(),
                    rollback: rollback_handles(handles, context.recorder),
                });
            }
        }
    }
    let target_scope = plan.change_set.scope.unwrap_or(plan.source_scope);
    let canonical_package = plan
        .target_store_root
        .join("skills")
        .join(plan.target_slug.as_str());
    let store_hash = SkillPackage::read(&canonical_package)?.hash.to_string();
    connection
        .execute_batch("SAVEPOINT skill_transfer_db")
        .map_err(at_db::DbError::Sqlite)?;
    let database_update = (|| -> Result<(), at_db::DbError> {
        if plan.target_item_id == plan.source_item_id {
            if let Some(replaced) = plan.replaced_item_id {
                // Replace reuses the target path and treats the promoted identity as the
                // surviving item, so the overwritten copy's row must go before the unique
                // `(kind, scope, project, slug)` check runs. Its deployments, versions and
                // source rows cascade with it.
                at_db::items::delete_item(connection, &replaced)?;
            }
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
            update_deployment_rows(
                connection,
                plan,
                target_scope,
                &store_hash,
                install_operation.as_deref(),
                &canonical.operation_id,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = database_update {
        let rollback_error = connection
            .execute_batch("ROLLBACK TO skill_transfer_db; RELEASE skill_transfer_db")
            .err();
        let files = rollback_handles(handles, context.recorder);
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

fn rollback_handles(
    handles: Vec<at_fs::RollbackHandle>,
    recorder: &mut dyn at_fs::OperationRecorder,
) -> String {
    let mut errors = Vec::new();
    for handle in handles.into_iter().rev() {
        if let Err(error) = at_fs::rollback_committed(handle, recorder) {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        "complete".to_owned()
    } else {
        errors.join("; ")
    }
}

/// Rewrites the deployment index entries the applied transfer now describes.
///
/// A detached source row stays as an `off` tombstone owned by the canonical operation, which is
/// the one that removed the source link, so undoing it flips the row back to `linked`. One
/// `linked` row per enabled agent then names the target-scope native path, and
/// `sync_written_target` records the bytes actually materialised there — a shared root (Codex
/// and omp on `.agents/skills`) keeps one consistent hash per path.
///
/// Bulk transfers apply every phase as one operation and pass that id for both slots.
///
/// # Errors
/// Returns the database failure when a row cannot be written or the materialised target cannot
/// be hashed.
pub fn update_deployment_rows(
    connection: &rusqlite::Connection,
    plan: &SkillTransferPlan,
    target_scope: Scope,
    store_hash: &str,
    install_operation: Option<&str>,
    canonical_operation: &str,
) -> Result<(), at_db::DbError> {
    for cell in plan
        .deployments
        .iter()
        .filter(|cell| cell.scope == plan.source_scope && cell.state == ItemState::Linked)
    {
        let Some(source_text) = cell.target_path.as_deref() else {
            continue;
        };
        let detached = plan.native_deployments.iter().any(|deployment| {
            deployment.detached && deployment.source_path.to_string_lossy() == source_text
        });
        if !detached {
            continue;
        }
        let item_id = plan.target_item_id;
        let agent = cell.agent_id.clone();
        let hashes =
            at_db::deployments::DeploymentHashes::new(cell.store_hash.as_deref(), None, None);
        let mut off = at_db::deployments::DeploymentSpec::applied(
            &item_id,
            &agent,
            plan.source_scope,
            ItemState::Off,
            Some("none"),
            Some(source_text),
            hashes,
        )
        .with_operation(canonical_operation);
        if let Some(project) = cell.project_id.as_deref() {
            off = off.with_project(project);
        }
        at_db::deployments::upsert_deployment(connection, &off)?;
    }
    let canonical_package = plan
        .target_store_root
        .join("skills")
        .join(plan.target_slug.as_str());
    let link_target = canonical_package.to_string_lossy().into_owned();
    for deployment in &plan.native_deployments {
        let item_id = plan.target_item_id;
        let agent = deployment.agent.clone();
        let target_text = deployment.target_path.to_string_lossy().into_owned();
        let mode = match deployment.mode {
            LinkMode::Link => "symlink",
            LinkMode::Copy => "copy",
        };
        let hashes = at_db::deployments::DeploymentHashes::new(
            Some(store_hash),
            Some(store_hash),
            Some(store_hash),
        );
        let mut target = at_db::deployments::DeploymentSpec::applied(
            &item_id,
            &agent,
            target_scope,
            ItemState::Linked,
            Some(mode),
            Some(&target_text),
            hashes,
        )
        .with_operation(install_operation.unwrap_or(canonical_operation));
        if deployment.mode == LinkMode::Link {
            target = target.with_link_target(&link_target);
        }
        if let Some(project) = plan.target_project_id.as_deref() {
            target = target.with_project(project);
        }
        at_db::deployments::upsert_deployment(connection, &target)?;
        crate::drift::sync_written_target(
            connection,
            Some(target_scope),
            &deployment.target_path,
            Some(&canonical_package),
        )
        .map_err(|error| at_db::DbError::InvalidValue {
            field: "deployments.baseline_hash",
            reason: error.to_string(),
        })?;
    }
    Ok(())
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
            native_targets: &[],
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

    #[test]
    fn database_failure_rolls_back_every_phase_and_closes_the_operations() {
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
        fixture
            .database
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_item_update BEFORE UPDATE ON items \
                 BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
            )
            .expect("refusing trigger");
        let mut recorder = at_fs::DbOperationRecorder::new(fixture.database.connection(), 30);
        let mut context = at_fs::ApplyContext {
            store_root: fixture.target_store.root(),
            backup_folder: &backup,
            recorder: &mut recorder,
        };
        let roots =
            at_fs::ApplyRoots::new([fixture._temp.path().to_path_buf()]).expect("authorized root");

        let error =
            apply_skill_transfer(fixture.database.connection(), &plan, &mut context, &roots)
                .expect_err("the database update fails");

        assert!(
            matches!(&error, SkillTransferError::Rehome { rollback, .. } if rollback == "complete"),
            "{error:?}"
        );
        assert!(
            fixture
                .source_store
                .skill_dir(&fixture.slug)
                .join("SKILL.md")
                .exists(),
            "the moved canonical folder is restored"
        );
        assert!(
            !fixture.target_store.skill_dir(&fixture.slug).exists(),
            "the destination copy is removed"
        );
        let operations: Vec<(String, Option<String>, Option<String>)> = {
            let mut statement = fixture
                .database
                .connection()
                .prepare("SELECT id, status, error_kind FROM operations ORDER BY id")
                .expect("operations statement");
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("operations")
                .collect::<Result<Vec<_>, _>>()
                .expect("operations")
        };
        assert!(!operations.is_empty(), "the applied phases are recorded");
        for (id, status, error_kind) in &operations {
            assert_eq!(status.as_deref(), Some("failed"), "operation {id}");
            assert_eq!(error_kind.as_deref(), Some("rolled_back"), "operation {id}");
        }
        let rows: Vec<(Option<String>, Option<String>)> = {
            let mut statement = fixture
                .database
                .connection()
                .prepare("SELECT before_hash, after_hash FROM operation_files")
                .expect("file rows statement");
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("file rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("file rows")
        };
        assert!(!rows.is_empty(), "the phases recorded the files they wrote");
        for (before, after) in rows {
            assert_eq!(before, after, "each row describes the restored state");
        }
        for (id, _, _) in operations {
            let undo = at_migrate::undo::undo_operation(
                at_migrate::undo::UndoOperationRequest {
                    connection: fixture.database.connection(),
                    store_root: fixture.target_store.root(),
                    backup_root: &backup,
                    authorized_roots: &roots,
                    actor: at_db::ops::OperationActor::Ui,
                    force: false,
                },
                &id,
            )
            .expect("undo runs");
            assert_eq!(
                undo,
                at_migrate::undo::UndoOutcome::NotUndoable,
                "undo refuses rolled-back operation {id}"
            );
        }
    }
}
