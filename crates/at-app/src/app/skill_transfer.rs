//! Reviewed skill promotion, movement and disable-in-repository command handlers.
//!
//! This module adapts the engine's content-bound plans to Tauri requests and views. All
//! production filesystem changes are applied as at-fs ChangeSets.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{LazyLock, Mutex},
};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::deployments::DeploymentSpec;
use at_engine::{
    bulk_skill::{BulkIntent, BulkItemPlan, BulkKind, BulkPlan, plan_bulk},
    change_for_repo::{ChangeForRepoIntent, ChangeForRepoRequest, plan_change_for_repo},
    local_scope::{LocalLayer, LocalScope},
    skill_transfer::{
        ClashResolution, NativeTransferTarget, SkillTransfer, SkillTransferPlan,
        SkillTransferRequest, apply_skill_transfer_with_registry, plan_skill_transfer,
        update_deployment_rows,
    },
};
use at_model::{ChangeSet, ExpectedState, FileOp, ItemId, ItemKind, ItemState, LinkMode, Scope};
use at_store::{
    SkillPackage,
    layout::{Slug, StoreLayout},
    local_store::LocalStorePaths,
    manifest::{StoreManifest, hash_skill_folder},
    paths::AppPaths,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use ts_rs::TS;

use super::{
    AppState,
    bulk_skill::{BulkSkillBatchView, BulkSkillProgressView, EVENT_BULK_PROGRESS},
    commands::{CommandError, CommandErrorCode},
    mcp_servers::project_of,
    paths::resolve_command_paths,
};

/// Parameters used to list legal destination scopes for a skill transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferOptionsRequest {
    /// Indexed skill identity.
    pub item_id: String,
    /// Requested transfer kind: `promote`, `move`, or `duplicate`.
    pub transfer: String,
    /// Registered project context for Project and Local scopes.
    pub project_id: Option<String>,
}

/// A destination scope and its availability for a reviewed transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferTargetView {
    /// Destination scope wire name.
    pub scope: String,
    /// Canonical destination store root, when the scope is resolvable.
    pub path: String,
    /// Whether a transfer may be planned for this destination.
    pub available: bool,
    /// Explanation when the destination is unavailable.
    pub reason: Option<String>,
}

/// Available destinations for one indexed skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferOptionsView {
    /// Indexed skill identity.
    pub item_id: String,
    /// Display name of the skill.
    pub name: String,
    /// Source scope wire name.
    pub source_scope: String,
    /// Canonical source package path.
    pub source_path: String,
    /// Scope choices in broadest-to-narrowest order.
    pub targets: Vec<SkillTransferTargetView>,
}

/// Parameters used to preview a Promote, Move or Duplicate operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferPreviewRequest {
    /// Indexed skill identity.
    pub item_id: String,
    /// Destination scope wire name.
    pub target_scope: String,
    /// Requested transfer kind: `promote`, `move`, or `duplicate`.
    pub transfer: String,
    /// Explicit target collision decision: `replace`, `rename`, or `null`.
    pub resolution: Option<String>,
    /// New lowercase slug required by `rename` or `duplicate`.
    pub new_slug: Option<String>,
    /// Registered project context for Project and Local scopes.
    pub project_id: Option<String>,
}

/// One package file affected by replacing an existing target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferDiffFileView {
    /// Relative package path.
    pub path: String,
    /// Whether this file is `added`, `changed`, or `removed` at the target.
    pub change: String,
}

/// Existing target details that require an explicit Replace or Rename decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferOverwriteView {
    /// SHA-256 of the current target package.
    pub target_hash: String,
    /// SHA-256 of the source package.
    pub source_hash: String,
    /// Per-file differences between source and target.
    pub files: Vec<SkillTransferDiffFileView>,
}

/// Content-bound review of a Promote, Move or Duplicate operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferPreviewView {
    /// Cached plan identity; absent while an existing target awaits a decision.
    pub preview_id: Option<String>,
    /// Skill display name.
    pub name: String,
    /// Source scope wire name.
    pub source_scope: String,
    /// Destination scope wire name.
    pub target_scope: String,
    /// Source package path.
    pub source_path: String,
    /// Destination package path.
    pub target_path: String,
    /// Source package SHA-256.
    pub source_hash: String,
    /// Destination package SHA-256 when it already exists.
    pub target_hash: Option<String>,
    /// True when Move removes the source only after its backup is created.
    pub removes_source_after_backup: bool,
    /// Backup root used by the safe-write pipeline.
    pub backup_location: String,
    /// Explicit collision comparison, when an existing target blocks planning.
    pub overwrite: Option<SkillTransferOverwriteView>,
    /// Paths changed by a non-blocked reviewed plan.
    pub files: Vec<String>,
}

/// Parameters used to apply a reviewed skill transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferApplyRequest {
    /// Preview identity returned by [`skill_transfer_preview`].
    pub preview_id: String,
}

/// Result of applying a reviewed Promote, Move or Duplicate operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferApplyView {
    /// Durable operation identity for undo/history.
    pub operation_id: String,
    /// Backup root containing the operation's backup folder.
    pub backup_location: String,
    /// Final destination package path.
    pub target_path: String,
}

/// Parameters for an atomic multi-skill Promote or Move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferBulkPreviewRequest {
    /// Selected managed skill identities.
    pub item_ids: Vec<String>,
    /// One destination scope shared by every selected skill.
    pub target_scope: String,
    /// Requested transfer: `promote` or `move`.
    pub transfer: String,
    /// Registered project used to resolve Project and Local stores.
    pub project_id: Option<String>,
}

/// One item in the atomic transfer preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferBulkItemView {
    /// Stable indexed skill identity.
    pub item_id: String,
    /// Display name, or the identity when the skill could not be loaded.
    pub name: String,
    /// Preflight refusal for this skill, when any.
    pub reason: Option<String>,
}

/// Reviewed all-or-nothing multi-skill transfer preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferBulkPreviewView {
    /// Cached ChangeSet identity; absent if any item is blocked.
    pub preview_id: Option<String>,
    /// Selected destination scope.
    pub target_scope: String,
    /// Backup root used for this ChangeSet.
    pub backup_location: String,
    /// Per-item preflight results.
    pub items: Vec<SkillTransferBulkItemView>,
}

/// Parameters to apply a reviewed bulk transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferBulkApplyRequest {
    /// Preview identity returned by [`skill_transfer_bulk_preview`].
    pub preview_id: String,
}

/// Result of applying an atomic multi-skill transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillTransferBulkApplyView {
    /// Durable operation identity for undo/history.
    pub operation_id: String,
    /// Backup root containing the operation's backup folder.
    pub backup_location: String,
    /// Number of skills applied.
    pub applied_count: u64,
}

/// Parameters used to preview disabling a skill in one project repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDisableInRepoPreviewRequest {
    /// Indexed skill identity.
    pub item_id: String,
    /// Registered project identity.
    pub project_id: String,
}

/// Reviewed project override and native link removals for a repo disable action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDisableInRepoPreviewView {
    /// Cached reviewed ChangeSet identity.
    pub preview_id: String,
    /// Skill display name.
    pub name: String,
    /// Source scope wire name.
    pub scope: String,
    /// Project store manifest path that receives the override tombstone.
    pub override_path: String,
    /// Native paths that will be removed after backup.
    pub links: Vec<String>,
    /// Agent names whose project deployment is being removed.
    pub agents: Vec<String>,
    /// Backup root used by the safe-write pipeline.
    pub backup_location: String,
}

/// Parameters used to apply a reviewed disable-in-repository action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDisableInRepoApplyRequest {
    /// Preview identity returned by [`skill_disable_in_repo_preview`].
    pub preview_id: String,
    /// Explicit user confirmation that the project override and link removals are wanted.
    pub confirmed: bool,
}

/// Result of applying a reviewed disable-in-repository action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDisableInRepoApplyView {
    /// Durable operation identity for undo/history.
    pub operation_id: String,
    /// Backup root containing the operation's backup folder.
    pub backup_location: String,
    /// Number of agent deployment rows switched off.
    pub agents_disabled: u64,
}
struct CachedBulkTransfer {
    plan: BulkPlan,
    transfers: Vec<SkillTransferPlan>,
    target_root: PathBuf,
    backup_root: PathBuf,
    authorized_roots: Vec<PathBuf>,
    target_scope: Scope,
    target_project_id: Option<String>,
}

#[derive(Clone)]
struct ManifestSnapshot {
    manifest: StoreManifest,
    bytes: Vec<u8>,
}

struct CachedTransfer {
    plan: SkillTransferPlan,
    target_root: PathBuf,
    backup_root: PathBuf,
    authorized_roots: Vec<PathBuf>,
    target_path: PathBuf,
}

struct CachedRepoDisable {
    set: ChangeSet,
    item_id: ItemId,
    linked_cells: Vec<at_db::shared::DeploymentCell>,
    project_store_root: PathBuf,
    backup_root: PathBuf,
    authorized_roots: Vec<PathBuf>,
}

static TRANSFER_PLANS: LazyLock<Mutex<HashMap<String, CachedTransfer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static REPO_DISABLE_PLANS: LazyLock<Mutex<HashMap<String, CachedRepoDisable>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static BULK_TRANSFER_PLANS: LazyLock<Mutex<HashMap<String, CachedBulkTransfer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Lists valid transfer destinations for a skill without writing files.
#[tauri::command]
pub async fn skill_transfer_options(
    app: AppHandle,
    request: SkillTransferOptionsRequest,
) -> Result<SkillTransferOptionsView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_transfer_options_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn skill_transfer_options_impl(
    state: &AppState,
    request: SkillTransferOptionsRequest,
) -> Result<SkillTransferOptionsView, CommandError> {
    let transfer = parse_transfer(&request.transfer)?;
    let item_id = parse_item_id(&request.item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = indexed_skill(database.connection(), item_id)?;
    ensure_unlocked(database.connection(), item_id)?;
    let project = project_for_item(
        database.connection(),
        request.project_id.as_deref(),
        item.scope,
        item.project_id.as_deref(),
    )?;
    let paths = effective_paths(state, database.connection(), project.as_ref())?;
    let source_layout = StoreLayout::for_scope(&paths, item.scope)
        .map_err(|_| CommandError::unavailable("source skill store is unavailable"))?;
    let source_path = skill_path(&source_layout, &item.slug)?;
    let targets = transfer_targets(item.scope, transfer, &paths, project.is_some());
    Ok(SkillTransferOptionsView {
        item_id: item_id.to_string(),
        name: item.name,
        source_scope: item.scope.wire_name().to_owned(),
        source_path: source_path.display().to_string(),
        targets,
    })
}

/// Plans a Promote or Move and caches its exact filesystem and database operations.
#[tauri::command]
pub async fn skill_transfer_preview(
    app: AppHandle,
    request: SkillTransferPreviewRequest,
) -> Result<SkillTransferPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_transfer_preview_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn skill_transfer_preview_impl(
    state: &AppState,
    request: SkillTransferPreviewRequest,
) -> Result<SkillTransferPreviewView, CommandError> {
    let mut transfer = parse_transfer(&request.transfer)?;
    let is_move = matches!(&transfer, SkillTransfer::Move);
    let item_id = parse_item_id(&request.item_id)?;
    let target_scope = Scope::from_str(&request.target_scope).map_err(|_| {
        CommandError::invalid_argument("target_scope", "unsupported destination scope")
    })?;
    let resolution = if matches!(&transfer, SkillTransfer::Duplicate { .. }) {
        if request.resolution.is_some() {
            return Err(CommandError::invalid_argument(
                "resolution",
                "Duplicate uses a new slug, not Replace or Rename",
            ));
        }
        let slug = request.new_slug.as_deref().ok_or_else(|| {
            CommandError::invalid_argument("new_slug", "enter a lowercase slug for the duplicate")
        })?;
        transfer = SkillTransfer::Duplicate {
            slug: Slug::parse(slug).map_err(|_| {
                CommandError::invalid_argument("new_slug", "enter a valid lowercase skill slug")
            })?,
        };
        None
    } else {
        parse_resolution(request.resolution.as_deref(), request.new_slug.as_deref())?
    };
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    let item = indexed_skill(connection, item_id)?;
    ensure_unlocked(connection, item_id)?;
    let project = project_for_item(
        connection,
        request.project_id.as_deref(),
        item.scope,
        item.project_id.as_deref(),
    )?;
    let paths = effective_paths(state, connection, project.as_ref())?;
    let targets = transfer_targets(item.scope, transfer.clone(), &paths, project.is_some());
    let target_option = targets
        .iter()
        .find(|target| target.scope == target_scope.wire_name())
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "target_scope",
                "destination is not valid for this transfer",
            )
        })?;
    if !target_option.available {
        return Err(CommandError::unavailable(
            target_option
                .reason
                .as_deref()
                .map_or("destination scope is unavailable", |reason| reason),
        ));
    }

    let source_store = StoreLayout::for_scope(&paths, item.scope)
        .map_err(|_| CommandError::unavailable("source skill store is unavailable"))?;
    let target_store = StoreLayout::for_scope(&paths, target_scope)
        .map_err(|_| CommandError::unavailable("destination skill store is unavailable"))?;
    let (source_manifest, source_bytes) = read_manifest(&source_store)?;
    let (target_manifest, target_bytes) = read_manifest(&target_store)?;
    let target_project_id = if matches!(target_scope, Scope::Project | Scope::Local) {
        project.as_ref().map(|row| row.id.clone())
    } else {
        None
    };
    let global_permission =
        at_fs::privilege::GlobalStoreStatus::probe(paths.global_store()).is_available();
    let native_targets = if matches!(&transfer, SkillTransfer::Duplicate { .. }) {
        Vec::new()
    } else {
        native_transfer_targets(
            connection,
            &paths,
            project.as_ref(),
            item_id,
            item.scope,
            target_scope,
        )?
    };
    let plan_result = plan_skill_transfer(SkillTransferRequest {
        transfer,
        source_store: &source_store,
        target_store: &target_store,
        item_id,
        source_manifest: &source_manifest,
        target_manifest: &target_manifest,
        source_manifest_bytes: &source_bytes,
        target_manifest_bytes: &target_bytes,
        global_permission,
        resolution,
        connection,
        target_project_id,
        native_targets: &native_targets,
    });

    match plan_result {
        Ok(plan) => {
            let preview_id = plan.change_set.preview_id.clone();
            let source_path = skill_path(&source_store, &item.slug)?;
            let target_path = target_store.skill_dir(&plan.target_slug);
            let source_hash = SkillPackage::read(&source_path)
                .map_err(|_| {
                    CommandError::unavailable("source skill package could not be inspected")
                })?
                .hash
                .to_string();
            let target_hash = if target_path.exists() {
                Some(
                    hash_skill_folder(&target_path)
                        .map_err(|_| {
                            CommandError::unavailable(
                                "destination skill package could not be inspected",
                            )
                        })?
                        .to_string(),
                )
            } else {
                None
            };
            // Undo reads the central backup root, not the destination store's folder.
            let backup_root = paths.backup_dir().to_path_buf();
            let authorized_roots = authorized_skill_roots(
                &[&source_store, &target_store],
                &[backup_root.as_path(), paths.backup_dir()],
                paths.home_dir(),
                project.as_ref().map(|row| Path::new(&row.path)),
            );
            let files = plan
                .change_set
                .ops
                .iter()
                .filter_map(FileOp::destination)
                .map(|path| path.display().to_string())
                .collect();
            TRANSFER_PLANS
                .lock()
                .map_err(|_| CommandError::internal("skill transfer preview cache is unavailable"))?
                .insert(
                    preview_id.clone(),
                    CachedTransfer {
                        plan,
                        target_root: target_store.root().to_path_buf(),
                        backup_root: backup_root.clone(),
                        authorized_roots,
                        target_path: target_path.clone(),
                    },
                );
            Ok(SkillTransferPreviewView {
                preview_id: Some(preview_id),
                name: item.name,
                source_scope: item.scope.wire_name().to_owned(),
                target_scope: target_scope.wire_name().to_owned(),
                source_path: source_path.display().to_string(),
                target_path: target_path.display().to_string(),
                source_hash,
                target_hash,
                removes_source_after_backup: is_move,
                backup_location: backup_root.display().to_string(),
                overwrite: None,
                files,
            })
        }
        Err(at_engine::skill_transfer::SkillTransferError::Clash(prompt)) => {
            let source_path = skill_path(&source_store, &item.slug)?;
            let source_hash = prompt.source_hash.to_string();
            let target_hash = prompt.target_hash.to_string();
            let files = compare_skill_packages(&source_path, &prompt.target_path)?;
            Ok(SkillTransferPreviewView {
                preview_id: None,
                name: item.name,
                source_scope: item.scope.wire_name().to_owned(),
                target_scope: target_scope.wire_name().to_owned(),
                source_path: source_path.display().to_string(),
                target_path: prompt.target_path.display().to_string(),
                source_hash: source_hash.clone(),
                target_hash: Some(target_hash.clone()),
                removes_source_after_backup: is_move,
                backup_location: target_store.backups_dir().display().to_string(),
                overwrite: Some(SkillTransferOverwriteView {
                    target_hash,
                    source_hash,
                    files,
                }),
                files: Vec::new(),
            })
        }
        Err(error) => Err(transfer_error(error)),
    }
}

/// Applies the exact cached transfer plan returned by [`skill_transfer_preview`].
#[tauri::command]
pub async fn skill_transfer_apply(
    app: AppHandle,
    request: SkillTransferApplyRequest,
) -> Result<SkillTransferApplyView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_transfer_apply_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn skill_transfer_apply_impl(
    state: &AppState,
    request: SkillTransferApplyRequest,
) -> Result<SkillTransferApplyView, CommandError> {
    let cached = TRANSFER_PLANS
        .lock()
        .map_err(|_| CommandError::internal("skill transfer preview cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "skill transfer preview expired; preview again",
            )
        })?;
    let roots = existing_apply_roots(cached.authorized_roots)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: &cached.target_root,
        backup_folder: &cached.backup_root,
        recorder: &mut recorder,
    };
    let outcome = apply_skill_transfer_with_registry(
        database.connection(),
        &cached.plan,
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map_err(transfer_error)?;
    Ok(SkillTransferApplyView {
        operation_id: outcome.canonical_operation_id,
        backup_location: cached.backup_root.display().to_string(),
        target_path: cached.target_path.display().to_string(),
    })
}

/// Preflights every selected skill and creates one atomic Promote or Move ChangeSet.
#[tauri::command]
pub async fn skill_transfer_bulk_preview(
    app: AppHandle,
    request: SkillTransferBulkPreviewRequest,
) -> Result<SkillTransferBulkPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_transfer_bulk_preview_impl(&state, request)
    })
    .await
}

/// Internal bulk-transfer planner over injected application state.
pub(crate) fn skill_transfer_bulk_preview_impl(
    state: &AppState,
    request: SkillTransferBulkPreviewRequest,
) -> Result<SkillTransferBulkPreviewView, CommandError> {
    if request.item_ids.is_empty() {
        return Err(CommandError::invalid_argument(
            "item_ids",
            "select at least one skill",
        ));
    }
    let transfer = parse_transfer(&request.transfer)?;
    let kind = match &transfer {
        SkillTransfer::Promote => BulkKind::Promote,
        SkillTransfer::Move => BulkKind::Move,
        _ => {
            return Err(CommandError::invalid_argument(
                "transfer",
                "bulk transfer supports Promote or Move",
            ));
        }
    };
    let target_scope = Scope::from_str(&request.target_scope).map_err(|_| {
        CommandError::invalid_argument("target_scope", "unsupported destination scope")
    })?;
    let items = request
        .item_ids
        .iter()
        .map(|value| parse_item_id(value))
        .collect::<Result<Vec<_>, _>>()?;
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(CommandError::invalid_argument(
            "item_ids",
            "duplicate skill identities are not allowed",
        ));
    }

    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    let mut snapshots = BTreeMap::<PathBuf, ManifestSnapshot>::new();
    let mut planned = Vec::<(SkillTransferPlan, StoreLayout, StoreLayout, String)>::new();
    let mut item_views = Vec::with_capacity(items.len());
    let mut authorized_roots = Vec::new();
    let mut target_root = None;
    let mut backup_root = None;
    let mut target_project_id = None;

    for (item_id, input_id) in items.iter().zip(&request.item_ids) {
        let item = match indexed_skill(connection, *item_id) {
            Ok(item) => item,
            Err(error) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: input_id.clone(),
                    name: input_id.clone(),
                    reason: Some(error.message),
                });
                continue;
            }
        };
        let mut reason = ensure_unlocked(connection, *item_id)
            .err()
            .map(|error| error.message);
        let project = if reason.is_none() {
            match project_for_item(
                connection,
                request.project_id.as_deref(),
                item.scope,
                item.project_id.as_deref(),
            ) {
                Ok(project) => project,
                Err(error) => {
                    reason = Some(error.message);
                    None
                }
            }
        } else {
            None
        };
        let paths = if reason.is_none() {
            match effective_paths(state, connection, project.as_ref()) {
                Ok(paths) => Some(paths),
                Err(error) => {
                    reason = Some(error.message);
                    None
                }
            }
        } else {
            None
        };
        let Some(paths) = paths else {
            item_views.push(SkillTransferBulkItemView {
                item_id: item_id.to_string(),
                name: item.name,
                reason,
            });
            continue;
        };
        let targets = transfer_targets(item.scope, transfer.clone(), &paths, project.is_some());
        let target_option = targets
            .iter()
            .find(|target| target.scope == target_scope.wire_name());
        if target_option.is_none_or(|target| !target.available) {
            item_views.push(SkillTransferBulkItemView {
                item_id: item_id.to_string(),
                name: item.name,
                reason: Some(
                    target_option
                        .and_then(|target| target.reason.as_deref())
                        .unwrap_or("destination scope is unavailable")
                        .to_owned(),
                ),
            });
            continue;
        }
        let source_store = match StoreLayout::for_scope(&paths, item.scope) {
            Ok(layout) => layout,
            Err(_) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name,
                    reason: Some("source skill store is unavailable".to_owned()),
                });
                continue;
            }
        };
        let target_store = match StoreLayout::for_scope(&paths, target_scope) {
            Ok(layout) => layout,
            Err(_) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name,
                    reason: Some("destination skill store is unavailable".to_owned()),
                });
                continue;
            }
        };
        let source_snapshot = match load_manifest_snapshot(&source_store, &mut snapshots) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name,
                    reason: Some(error.message),
                });
                continue;
            }
        };
        let target_snapshot = match load_manifest_snapshot(&target_store, &mut snapshots) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name,
                    reason: Some(error.message),
                });
                continue;
            }
        };
        let scoped_project_id = if matches!(target_scope, Scope::Project | Scope::Local) {
            project.as_ref().map(|row| row.id.clone())
        } else {
            None
        };
        if target_project_id.is_some() && target_project_id != scoped_project_id {
            item_views.push(SkillTransferBulkItemView {
                item_id: item_id.to_string(),
                name: item.name,
                reason: Some("selected skills do not share one destination project".to_owned()),
            });
            continue;
        }
        if scoped_project_id.is_some() {
            target_project_id.clone_from(&scoped_project_id);
        }
        let global_permission =
            at_fs::privilege::GlobalStoreStatus::probe(paths.global_store()).is_available();
        let native_targets = match native_transfer_targets(
            connection,
            &paths,
            project.as_ref(),
            *item_id,
            item.scope,
            target_scope,
        ) {
            Ok(targets) => targets,
            Err(error) => {
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name,
                    reason: Some(error.message),
                });
                continue;
            }
        };
        let plan = plan_skill_transfer(SkillTransferRequest {
            transfer: transfer.clone(),
            source_store: &source_store,
            target_store: &target_store,
            item_id: *item_id,
            source_manifest: &source_snapshot.manifest,
            target_manifest: &target_snapshot.manifest,
            source_manifest_bytes: &source_snapshot.bytes,
            target_manifest_bytes: &target_snapshot.bytes,
            global_permission,
            resolution: None,
            connection,
            target_project_id: scoped_project_id,
            native_targets: &native_targets,
        });
        match plan {
            Ok(plan) => {
                target_root.get_or_insert_with(|| target_store.root().to_path_buf());
                backup_root.get_or_insert_with(|| paths.backup_dir().to_path_buf());
                authorized_roots.extend(authorized_skill_roots(
                    &[&source_store, &target_store],
                    &[target_store.backups_dir().as_path(), paths.backup_dir()],
                    paths.home_dir(),
                    project.as_ref().map(|row| Path::new(&row.path)),
                ));
                item_views.push(SkillTransferBulkItemView {
                    item_id: item_id.to_string(),
                    name: item.name.clone(),
                    reason: None,
                });
                planned.push((plan, source_store, target_store, item.name));
            }
            Err(error) => item_views.push(SkillTransferBulkItemView {
                item_id: item_id.to_string(),
                name: item.name,
                reason: Some(error.to_string()),
            }),
        }
    }

    let backup_location = backup_root
        .as_ref()
        .map_or_else(String::new, |path| path.display().to_string());
    if item_views.iter().any(|item| item.reason.is_some()) {
        return Ok(SkillTransferBulkPreviewView {
            preview_id: None,
            target_scope: target_scope.wire_name().to_owned(),
            backup_location,
            items: item_views,
        });
    }

    for (plan, source_store, target_store, _) in &planned {
        let manifest_path = target_store.manifest_path();
        let target_bytes = plan
            .change_set
            .ops
            .iter()
            .find_map(|op| match op {
                FileOp::Write { path, bytes, .. } if path == &manifest_path => {
                    Some(bytes.as_slice())
                }
                _ => None,
            })
            .ok_or_else(|| {
                CommandError::internal("transfer plan omitted its destination manifest")
            })?;
        let planned_manifest = StoreManifest::parse(target_bytes).map_err(|error| {
            CommandError::internal(format!("planned manifest is invalid: {error}"))
        })?;
        let target_item = planned_manifest
            .item(plan.target_item_id)
            .cloned()
            .ok_or_else(|| CommandError::internal("transfer plan omitted its destination item"))?;
        let target_snapshot = snapshots
            .get_mut(&manifest_path)
            .ok_or_else(|| CommandError::internal("destination manifest snapshot is missing"))?;
        if let Some(replaced) = plan.replaced_item_id {
            target_snapshot.manifest.remove_item(replaced);
        }
        target_snapshot
            .manifest
            .insert_item(target_item)
            .map_err(|error| {
                CommandError::internal(format!("combined manifest is invalid: {error}"))
            })?;
        if !plan.keeps_source {
            let source_snapshot = snapshots
                .get_mut(&source_store.manifest_path())
                .ok_or_else(|| CommandError::internal("source manifest snapshot is missing"))?;
            source_snapshot.manifest.remove_item(plan.source_item_id);
        }
    }

    let manifest_paths = snapshots.keys().cloned().collect::<BTreeSet<_>>();
    let mut item_plans = planned
        .iter()
        .map(|(plan, _, _, _)| {
            let mut ops = plan
                .change_set
                .ops
                .iter()
                .filter(|op| !is_manifest_write(op, &manifest_paths))
                .cloned()
                .collect::<Vec<_>>();
            if let Some(deployment_set) = &plan.deployment_change_set {
                ops.extend(deployment_set.ops.iter().cloned());
            }
            BulkItemPlan {
                item_id: plan.source_item_id,
                ops,
                refusal: None,
            }
        })
        .collect::<Vec<_>>();
    let Some(first_plan) = item_plans.first_mut() else {
        return Err(CommandError::invalid_argument(
            "item_ids",
            "no valid skill transfers",
        ));
    };
    for (manifest_path, snapshot) in &snapshots {
        let bytes = snapshot.manifest.to_bytes().map_err(|error| {
            CommandError::internal(format!(
                "combined manifest could not be serialized: {error}"
            ))
        })?;
        let expected = ExpectedState::hash(at_fs::sha256_hex(&snapshot.bytes))
            .map_err(|_| CommandError::internal("manifest precondition could not be created"))?;
        first_plan.ops.push(FileOp::Write {
            path: manifest_path.clone(),
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
    }
    let intent = BulkIntent {
        kind,
        items: items.clone(),
        targets: Vec::new(),
        scope: target_scope,
        project_id: target_project_id.clone(),
    };
    let bulk_plan = match plan_bulk(&intent, item_plans) {
        Ok(plan) => plan,
        Err(error) => {
            let reason = format!("The atomic batch could not be planned: {error}");
            return Ok(SkillTransferBulkPreviewView {
                preview_id: None,
                target_scope: target_scope.wire_name().to_owned(),
                backup_location,
                items: item_views
                    .into_iter()
                    .map(|mut item| {
                        item.reason = Some(reason.clone());
                        item
                    })
                    .collect(),
            });
        }
    };
    let preview_id = bulk_plan.change_set.preview_id.clone();
    BULK_TRANSFER_PLANS
        .lock()
        .map_err(|_| CommandError::internal("bulk transfer preview cache is unavailable"))?
        .insert(
            preview_id.clone(),
            CachedBulkTransfer {
                plan: bulk_plan,
                transfers: planned.into_iter().map(|(plan, _, _, _)| plan).collect(),
                target_root: target_root
                    .ok_or_else(|| CommandError::internal("destination store is missing"))?,
                backup_root: backup_root
                    .ok_or_else(|| CommandError::internal("backup location is missing"))?,
                authorized_roots,
                target_scope,
                target_project_id,
            },
        );
    Ok(SkillTransferBulkPreviewView {
        preview_id: Some(preview_id),
        target_scope: target_scope.wire_name().to_owned(),
        backup_location,
        items: item_views,
    })
}

/// Applies one atomic bulk transfer after a successful preflight.
#[tauri::command]
pub async fn skill_transfer_bulk_apply(
    app: AppHandle,
    request: SkillTransferBulkApplyRequest,
) -> Result<SkillTransferBulkApplyView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_transfer_bulk_apply_impl(&app, &state, request)
    })
    .await
}

/// Internal bulk-transfer applier over injected state.
pub(crate) fn skill_transfer_bulk_apply_impl(
    app: &AppHandle,
    state: &AppState,
    request: SkillTransferBulkApplyRequest,
) -> Result<SkillTransferBulkApplyView, CommandError> {
    let cached = BULK_TRANSFER_PLANS
        .lock()
        .map_err(|_| CommandError::internal("bulk transfer preview cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "bulk transfer preview expired; preview again",
            )
        })?;
    let roots = existing_apply_roots(cached.authorized_roots)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    connection
        .execute_batch("SAVEPOINT skill_transfer_bulk")
        .map_err(|_| CommandError::internal("could not start the bulk transfer transaction"))?;
    let mut recorder = at_fs::DbOperationRecorder::new(connection, 30);
    let outcome = {
        let mut context = at_fs::ApplyContext {
            store_root: &cached.target_root,
            backup_folder: &cached.backup_root,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots_reversible_and_registry(
            &cached.plan.change_set,
            &mut context,
            &roots,
            &state.recent_writes(),
        )
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = connection
                .execute_batch("ROLLBACK TO skill_transfer_bulk; RELEASE skill_transfer_bulk");
            emit_bulk_transfer_progress(app, &cached.plan, false, "", 0, Some(error.to_string()));
            return Err(CommandError::unavailable(error.to_string()));
        }
    };
    let rollback_handle = match outcome.rollback_handle {
        Some(handle) => handle,
        None => {
            let _ = connection
                .execute_batch("ROLLBACK TO skill_transfer_bulk; RELEASE skill_transfer_bulk");
            return Err(CommandError::internal(
                "bulk transfer apply did not return a rollback token",
            ));
        }
    };
    let database_update = cached.transfers.iter().try_for_each(|plan| {
        if let Some(replaced) = plan.replaced_item_id {
            at_db::items::delete_item(connection, &replaced)?;
        }
        at_db::items::rehome_item(
            connection,
            &plan.source_item_id,
            cached.target_scope,
            plan.target_slug.as_str(),
            &format!("skills/{}", plan.target_slug),
            cached.target_project_id.as_deref(),
        )?;
        let canonical_package = plan
            .target_store_root
            .join("skills")
            .join(plan.target_slug.as_str());
        let store_hash = SkillPackage::read(&canonical_package)
            .map_err(|error| at_db::DbError::InvalidValue {
                field: "skills",
                reason: error.to_string(),
            })?
            .hash
            .to_string();
        // The batch applied every phase as one operation, so one id owns the detach tombstones
        // and the new target rows alike.
        update_deployment_rows(
            connection,
            plan,
            cached.target_scope,
            &store_hash,
            Some(&outcome.operation_id),
            &outcome.operation_id,
        )?;
        Ok::<(), at_db::DbError>(())
    });
    if let Err(error) = database_update {
        let _ = connection
            .execute_batch("ROLLBACK TO skill_transfer_bulk; RELEASE skill_transfer_bulk");
        let rollback = at_fs::rollback_committed(rollback_handle, &mut recorder);
        return Err(CommandError::internal(match rollback {
            Ok(()) => format!("skill index could not be updated: {error}"),
            Err(rollback_error) => format!(
                "skill index could not be updated: {error}; filesystem rollback failed: {rollback_error}"
            ),
        }));
    }
    connection
        .execute_batch("RELEASE skill_transfer_bulk")
        .map_err(|error| {
            CommandError::internal(format!(
                "bulk transfer transaction could not be committed: {error}"
            ))
        })?;
    at_fs::commit_reversible(rollback_handle).map_err(|error| {
        CommandError::internal(format!("bulk transfer could not be finalized: {error}"))
    })?;
    emit_bulk_transfer_progress(
        app,
        &cached.plan,
        true,
        &outcome.operation_id,
        outcome.completed,
        None,
    );
    Ok(SkillTransferBulkApplyView {
        operation_id: outcome.operation_id,
        backup_location: cached.backup_root.display().to_string(),
        applied_count: u64::try_from(cached.transfers.len()).unwrap_or(u64::MAX),
    })
}

fn emit_bulk_transfer_progress(
    app: &AppHandle,
    plan: &BulkPlan,
    applied: bool,
    operation_id: &str,
    files_changed: usize,
    refusal: Option<String>,
) {
    let events = plan
        .items
        .iter()
        .map(|item_id| BulkSkillProgressView {
            item_id: item_id.to_string(),
            stage: if applied { "item_done" } else { "item_failed" }.to_owned(),
        })
        .collect();
    let batch = BulkSkillBatchView {
        batch_id: plan.change_set.id.clone(),
        events,
        operation_id: operation_id.to_owned(),
        files_changed,
        applied,
        refusal,
    };
    let _ = app.emit(EVENT_BULK_PROGRESS, &batch);
}

fn load_manifest_snapshot(
    layout: &StoreLayout,
    snapshots: &mut BTreeMap<PathBuf, ManifestSnapshot>,
) -> Result<ManifestSnapshot, CommandError> {
    let path = layout.manifest_path();
    if let Some(snapshot) = snapshots.get(&path) {
        return Ok(snapshot.clone());
    }
    let (manifest, bytes) = read_manifest(layout)?;
    let snapshot = ManifestSnapshot { manifest, bytes };
    snapshots.insert(path, snapshot.clone());
    Ok(snapshot)
}

fn is_manifest_write(op: &FileOp, manifest_paths: &BTreeSet<PathBuf>) -> bool {
    matches!(op, FileOp::Write { path, .. } if manifest_paths.contains(path))
}

/// Plans a Project-scope tombstone and removal of this project's materialized skill links.
#[tauri::command]
pub async fn skill_disable_in_repo_preview(
    app: AppHandle,
    request: SkillDisableInRepoPreviewRequest,
) -> Result<SkillDisableInRepoPreviewView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_disable_in_repo_preview_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn skill_disable_in_repo_preview_impl(
    state: &AppState,
    request: SkillDisableInRepoPreviewRequest,
) -> Result<SkillDisableInRepoPreviewView, CommandError> {
    let item_id = parse_item_id(&request.item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    let item = indexed_skill(connection, item_id)?;
    ensure_unlocked(connection, item_id)?;
    let project = project_of(&database, Some(&request.project_id))?.ok_or_else(|| {
        CommandError::invalid_argument("project_id", "a registered project is required")
    })?;
    if matches!(item.scope, Scope::Project | Scope::Local)
        && item.project_id.as_deref() != Some(project.id.as_str())
    {
        return Err(CommandError::invalid_argument(
            "item_id",
            "skill does not belong to the selected project",
        ));
    }
    let project_root = PathBuf::from(&project.path);
    let paths = effective_paths(state, connection, Some(&project))?;
    let project_store = StoreLayout::for_scope(&paths, Scope::Project)
        .map_err(|_| CommandError::unavailable("project skill store is unavailable"))?;
    let local_store = StoreLayout::for_scope(&paths, Scope::Local)
        .map_err(|_| CommandError::unavailable("local skill store is unavailable"))?;
    let (project_manifest, _) = read_manifest(&project_store)?;
    let local_manifest = read_manifest_or_empty(&local_store, Scope::Local)?;
    let loaded = state
        .load_scope_snapshots(Some(&project_root))
        .map_err(|_| CommandError::unavailable("project scope definitions are unavailable"))?;
    let mut base_layers = loaded
        .layers
        .iter()
        .filter(|layer| layer.scope != Scope::Local)
        .cloned()
        .collect::<Vec<_>>();
    base_layers.sort_by_key(|layer| layer.scope.precedence());
    let inherited_manifests = load_inherited_manifests(&paths)?;
    let local_paths = LocalStorePaths::from_project(&project_store)
        .ok_or_else(|| CommandError::unavailable("project Local store is unavailable"))?;
    let local_scope = LocalScope::detect(&project_root, local_paths);
    let local_definitions = loaded
        .layers
        .iter()
        .find(|layer| layer.scope == Scope::Local)
        .map_or_else(Vec::new, |layer| layer.definitions.clone());
    let local_layer = LocalLayer::new(local_scope, local_definitions)
        .map_err(|_| CommandError::unavailable("Local skill definitions are invalid"))?;
    let definition = loaded
        .layers
        .iter()
        .flat_map(|layer| layer.definitions.iter())
        .find(|definition| {
            definition.item_id == Some(item_id) && definition.kind == ItemKind::Skill
        })
        .cloned()
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "skill definition is unavailable",
            )
        })?;
    let plan = plan_change_for_repo(
        ChangeForRepoIntent::DisableInRepo {
            scope: Scope::Project,
        },
        &ChangeForRepoRequest {
            item_id,
            kind: ItemKind::Skill,
            name: item.name.clone(),
            definition_bytes: Vec::new(),
            project_definition: definition,
            base_layers: &base_layers,
            local_layer: &local_layer,
            inherited_manifests: &inherited_manifests,
            project_manifest: &project_manifest,
            local_manifest: &local_manifest,
            project_store_path: project_store.manifest_path(),
            local_store_path: local_store.manifest_path(),
            project_definitions: project_store.skills_dir(),
            locks: None,
            agent: None,
            project_id: project.id.clone(),
        },
    )
    .map_err(|error| CommandError::new(CommandErrorCode::Conflict, error.to_string()))?;
    let mut set = plan
        .change_set
        .ok_or_else(|| CommandError::internal("disable plan did not include a ChangeSet"))?;

    let deployments = at_db::deployments::list_deployments_for_item(connection, &item_id)
        .map_err(|_| CommandError::internal("could not read skill project deployments"))?;
    let linked_cells = deployments
        .into_iter()
        .filter(|cell| {
            matches!(cell.scope, Scope::Project | Scope::Local)
                && cell.project_id.as_deref() == Some(project.id.as_str())
                && cell.state == ItemState::Linked
        })
        .collect::<Vec<_>>();
    let mut links = BTreeMap::<PathBuf, ExpectedState>::new();
    for cell in &linked_cells {
        let Some(target) = cell.target_path.as_deref() else {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                format!(
                    "{} has a linked skill without a recorded target path",
                    cell.agent_id
                ),
            ));
        };
        let path = PathBuf::from(target);
        if !path.is_absolute() || !path.starts_with(&project_root) {
            return Err(CommandError::new(
                CommandErrorCode::RefusedOverwrite,
                format!(
                    "refusing to remove a skill deployment outside the selected project: {target}"
                ),
            ));
        }
        let hash = at_fs::hash_node(&path)
            .map_err(|_| {
                CommandError::new(
                    CommandErrorCode::Conflict,
                    "project skill link could not be inspected",
                )
            })?
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::Conflict,
                    format!("recorded project skill link is missing: {target}"),
                )
            })?;
        let expected = ExpectedState::hash(hash)
            .map_err(|_| CommandError::internal("project skill link hash is invalid"))?;
        links.entry(path).or_insert(expected);
    }
    for (path, expected) in &links {
        set.ops.push(FileOp::Delete {
            path: path.clone(),
            expected: expected.clone(),
        });
    }
    set.summary = format!(
        "Disable skill {} in {} and remove {} project link(s)",
        item.name,
        project.name,
        links.len()
    );
    set.validate().map_err(|error| {
        CommandError::internal(format!("disable ChangeSet is invalid: {error}"))
    })?;
    let preview_id = uuid::Uuid::now_v7().to_string();
    let authorized_roots = authorized_skill_roots(
        &[&project_store, &local_store],
        &[paths.backup_dir()],
        paths.home_dir(),
        Some(project_root.as_path()),
    );
    let backup_root = paths.backup_dir().to_path_buf();
    let agents = linked_cells
        .iter()
        .map(|cell| cell.agent_id.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let link_paths = links
        .keys()
        .map(|path| path.display().to_string())
        .collect();
    REPO_DISABLE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("disable preview cache is unavailable"))?
        .insert(
            preview_id.clone(),
            CachedRepoDisable {
                set,
                item_id,
                linked_cells,
                project_store_root: project_store.root().to_path_buf(),
                backup_root: backup_root.clone(),
                authorized_roots,
            },
        );
    Ok(SkillDisableInRepoPreviewView {
        preview_id,
        name: item.name,
        scope: item.scope.wire_name().to_owned(),
        override_path: project_store.manifest_path().display().to_string(),
        links: link_paths,
        agents,
        backup_location: backup_root.display().to_string(),
    })
}

/// Applies the reviewed project tombstone and removes recorded project links after backup.
#[tauri::command]
pub async fn skill_disable_in_repo_apply(
    app: AppHandle,
    request: SkillDisableInRepoApplyRequest,
) -> Result<SkillDisableInRepoApplyView, CommandError> {
    super::commands::run_blocking(move || {
        let state = app.state::<AppState>();
        skill_disable_in_repo_apply_impl(&state, request)
    })
    .await
}

/// Internal implementation over injected state for tests.
pub(crate) fn skill_disable_in_repo_apply_impl(
    state: &AppState,
    request: SkillDisableInRepoApplyRequest,
) -> Result<SkillDisableInRepoApplyView, CommandError> {
    if !request.confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let cached = REPO_DISABLE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("disable preview cache is unavailable"))?
        .remove(&request.preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "disable preview expired; preview again")
        })?;
    let roots = existing_apply_roots(cached.authorized_roots)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let connection = database.connection();
    connection
        .execute_batch("SAVEPOINT skill_disable_in_repo")
        .map_err(|_| CommandError::internal("could not start the disable transaction"))?;
    let mut recorder = at_fs::DbOperationRecorder::new(connection, 30);
    let outcome = {
        let mut context = at_fs::ApplyContext {
            store_root: &cached.project_store_root,
            backup_folder: &cached.backup_root,
            recorder: &mut recorder,
        };
        at_fs::apply_with_roots_reversible_and_registry(
            &cached.set,
            &mut context,
            &roots,
            &state.recent_writes(),
        )
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = connection
                .execute_batch("ROLLBACK TO skill_disable_in_repo; RELEASE skill_disable_in_repo");
            return Err(CommandError::from(error));
        }
    };
    let rollback_handle = outcome
        .rollback_handle
        .ok_or_else(|| CommandError::internal("disable apply did not return a rollback token"))?;
    for cell in &cached.linked_cells {
        let target_path = cell.target_path.as_deref().ok_or_else(|| {
            CommandError::internal("reviewed skill deployment lost its target path")
        })?;
        let hashes = at_db::deployments::DeploymentHashes::new(
            cell.store_hash.as_deref(),
            None,
            cell.baseline_hash.as_deref(),
        );
        let mut spec = DeploymentSpec::applied(
            &cached.item_id,
            &cell.agent_id,
            cell.scope,
            ItemState::Off,
            Some("none"),
            Some(target_path),
            hashes,
        )
        .with_operation(&outcome.operation_id);
        if let Some(project_id) = cell.project_id.as_deref() {
            spec = spec.with_project(project_id);
        }
        if let Err(error) = at_db::deployments::upsert_deployment(connection, &spec) {
            let _ = connection
                .execute_batch("ROLLBACK TO skill_disable_in_repo; RELEASE skill_disable_in_repo");
            let rollback = at_fs::rollback_committed(rollback_handle, &mut recorder);
            return Err(CommandError::internal(match rollback {
                Ok(()) => format!("deployment state could not be updated: {error}"),
                Err(rollback_error) => format!(
                    "deployment state could not be updated: {error}; filesystem rollback failed: {rollback_error}"
                ),
            }));
        }
    }
    connection
        .execute_batch("RELEASE skill_disable_in_repo")
        .map_err(|error| {
            CommandError::internal(format!(
                "disable transaction could not be committed: {error}"
            ))
        })?;
    at_fs::commit_reversible(rollback_handle).map_err(|error| {
        CommandError::internal(format!("disable operation could not be finalized: {error}"))
    })?;
    let agents_disabled = u64::try_from(cached.linked_cells.len()).unwrap_or(u64::MAX);
    Ok(SkillDisableInRepoApplyView {
        operation_id: outcome.operation_id,
        backup_location: cached.backup_root.display().to_string(),
        agents_disabled,
    })
}

fn indexed_skill(
    connection: &rusqlite::Connection,
    item_id: ItemId,
) -> Result<at_db::shared::ItemRow, CommandError> {
    let items = at_db::shared::items_by_ids(connection, &[item_id])
        .map_err(|_| CommandError::internal("could not read the skill index"))?;
    let item = items
        .into_iter()
        .find(|item| item.id == item_id)
        .ok_or_else(|| CommandError::new(CommandErrorCode::NotFound, "skill not found in index"))?;
    if item.kind != ItemKind::Skill {
        return Err(CommandError::invalid_argument(
            "item_id",
            "item is not a skill",
        ));
    }
    if !item.managed {
        return Err(CommandError::new(
            CommandErrorCode::RefusedOverwrite,
            "native or unmanaged skills cannot be promoted, moved, or disabled by this action",
        ));
    }
    Ok(item)
}

fn parse_item_id(value: &str) -> Result<ItemId, CommandError> {
    value
        .parse()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item UUID"))
}
fn skill_path(layout: &StoreLayout, slug: &str) -> Result<PathBuf, CommandError> {
    let slug = Slug::parse(slug).map_err(|_| {
        CommandError::invalid_argument("item_id", "skill index contains an invalid slug")
    })?;
    Ok(layout.skill_dir(&slug))
}

fn parse_transfer(value: &str) -> Result<SkillTransfer, CommandError> {
    match value {
        "promote" => Ok(SkillTransfer::Promote),
        "move" => Ok(SkillTransfer::Move),
        "duplicate" => Ok(SkillTransfer::Duplicate {
            slug: Slug::parse("duplicate")
                .map_err(|_| CommandError::internal("duplicate slug seed is invalid"))?,
        }),
        _ => Err(CommandError::invalid_argument(
            "transfer",
            "choose promote, move or duplicate",
        )),
    }
}

fn parse_resolution(
    resolution: Option<&str>,
    new_slug: Option<&str>,
) -> Result<Option<ClashResolution>, CommandError> {
    match resolution {
        None => Ok(None),
        Some("replace") => Ok(Some(ClashResolution::Replace)),
        Some("rename") => {
            let slug = new_slug.ok_or_else(|| {
                CommandError::invalid_argument(
                    "new_slug",
                    "enter a lowercase slug to rename the destination",
                )
            })?;
            let slug = Slug::parse(slug).map_err(|_| {
                CommandError::invalid_argument("new_slug", "enter a valid lowercase skill slug")
            })?;
            Ok(Some(ClashResolution::Rename(slug)))
        }
        Some(_) => Err(CommandError::invalid_argument(
            "resolution",
            "choose replace or rename",
        )),
    }
}

fn transfer_targets(
    source_scope: Scope,
    transfer: SkillTransfer,
    paths: &AppPaths,
    has_project: bool,
) -> Vec<SkillTransferTargetView> {
    let mut scopes = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
    scopes.sort_by_key(|scope| scope.precedence());
    scopes
        .into_iter()
        .filter(|scope| match &transfer {
            SkillTransfer::Promote => {
                scope.precedence() < source_scope.precedence() && *scope != source_scope
            }
            SkillTransfer::Move => *scope != source_scope,
            SkillTransfer::Duplicate { .. } => *scope == source_scope,
            _ => false,
        })
        .map(|scope| {
            let path = match StoreLayout::for_scope(paths, scope) {
                Ok(layout) => layout.root().display().to_string(),
                Err(_) => String::new(),
            };
            let mut reason = None;
            if matches!(scope, Scope::Project | Scope::Local) && !has_project {
                reason = Some("select a registered project first".to_owned());
            } else if scope == Scope::Local {
                let available = match paths.project_store() {
                    Some(store_root) => {
                        let project_store = StoreLayout::new(Scope::Project, store_root);
                        match (
                            store_root.parent(),
                            LocalStorePaths::from_project(&project_store),
                        ) {
                            (Some(project_root), Some(local_paths)) => {
                                LocalScope::detect(project_root, local_paths)
                                    .status()
                                    .is_available()
                            }
                            _ => false,
                        }
                    }
                    None => false,
                };
                if !available {
                    reason =
                        Some("Local scope requires a project inside a Git work tree".to_owned());
                }
            } else if scope == Scope::Global {
                let available =
                    at_fs::privilege::GlobalStoreStatus::probe(paths.global_store()).is_available();
                if !available {
                    reason =
                        Some("Global store is not writable; enable Global writes first".to_owned());
                }
            }
            SkillTransferTargetView {
                scope: scope.wire_name().to_owned(),
                path,
                available: reason.is_none(),
                reason,
            }
        })
        .collect()
}

fn project_for_item(
    connection: &rusqlite::Connection,
    requested_id: Option<&str>,
    source_scope: Scope,
    indexed_project_id: Option<&str>,
) -> Result<Option<at_db::projects::ProjectSummary>, CommandError> {
    let project_id = requested_id.or(indexed_project_id);
    let project = match project_id {
        Some(id) => at_db::projects::find_project_by_id(connection, id)
            .map_err(|_| CommandError::internal("could not read the selected project"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("project_id", "project is not registered")
            })
            .map(Some)?,
        None => None,
    };
    if matches!(source_scope, Scope::Project | Scope::Local) && project.is_none() {
        return Err(CommandError::invalid_argument(
            "project_id",
            "this skill requires its registered project",
        ));
    }
    if let Some(indexed_id) = indexed_project_id
        && project.as_ref().is_some_and(|row| row.id != indexed_id)
    {
        return Err(CommandError::invalid_argument(
            "project_id",
            "skill belongs to a different project",
        ));
    }
    Ok(project)
}

fn effective_paths(
    state: &AppState,
    connection: &rusqlite::Connection,
    project: Option<&at_db::projects::ProjectSummary>,
) -> Result<AppPaths, CommandError> {
    let root = project.map(|project| Path::new(&project.path));
    let mut paths = resolve_command_paths(state, root)?;
    let settings = at_db::settings::load(connection)
        .map_err(|_| CommandError::internal("could not read store path settings"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    Ok(paths)
}

fn ensure_unlocked(connection: &rusqlite::Connection, item_id: ItemId) -> Result<(), CommandError> {
    let locks = at_db::locks::locks_for_item(connection, &item_id)
        .map_err(|_| CommandError::internal("could not read skill locks"))?;
    if let Some(lock) = locks.first() {
        return Err(CommandError::new(
            CommandErrorCode::Locked,
            format!(
                "skill is locked and cannot be changed: {}",
                lock.reason
                    .as_deref()
                    .map_or_else(|| "locked by policy".to_owned(), |reason| reason.to_owned(),)
            ),
        ));
    }
    Ok(())
}

fn read_manifest(layout: &StoreLayout) -> Result<(StoreManifest, Vec<u8>), CommandError> {
    let bytes = std::fs::read(layout.manifest_path()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "skill store manifest is missing",
        )
    })?;
    let manifest = StoreManifest::parse(&bytes).map_err(|error| {
        CommandError::internal(format!("skill store manifest is invalid: {error}"))
    })?;
    Ok((manifest, bytes))
}

fn read_manifest_or_empty(
    layout: &StoreLayout,
    scope: Scope,
) -> Result<StoreManifest, CommandError> {
    match std::fs::read(layout.manifest_path()) {
        Ok(bytes) => StoreManifest::parse(&bytes).map_err(|error| {
            CommandError::internal(format!("skill store manifest is invalid: {error}"))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(StoreManifest::new(scope, at_store::layout::StoreId::new()))
        }
        Err(_) => Err(CommandError::unavailable(
            "skill store manifest could not be read",
        )),
    }
}

fn load_inherited_manifests(paths: &AppPaths) -> Result<Vec<(Scope, StoreManifest)>, CommandError> {
    [Scope::Global, Scope::User]
        .into_iter()
        .map(|scope| {
            let layout = StoreLayout::for_scope(paths, scope)
                .map_err(|_| CommandError::unavailable("inherited skill store is unavailable"))?;
            read_manifest_or_empty(&layout, scope).map(|manifest| (scope, manifest))
        })
        .collect()
}

fn compare_skill_packages(
    source_path: &Path,
    target_path: &Path,
) -> Result<Vec<SkillTransferDiffFileView>, CommandError> {
    let source = SkillPackage::read(source_path)
        .map_err(|_| CommandError::unavailable("source skill package could not be inspected"))?;
    let target = SkillPackage::read(target_path).map_err(|_| {
        CommandError::unavailable("destination skill package could not be inspected")
    })?;
    let mut differences = Vec::new();
    for path in source.package.paths() {
        let change = match target.package.get(path) {
            None => Some("added"),
            Some(existing)
                if source
                    .package
                    .get(path)
                    .is_some_and(|file| file.bytes != existing.bytes) =>
            {
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(change) = change {
            differences.push(SkillTransferDiffFileView {
                path: path.to_string(),
                change: change.to_owned(),
            });
        }
    }
    for path in target.package.paths() {
        if source.package.get(path).is_none() {
            differences.push(SkillTransferDiffFileView {
                path: path.to_string(),
                change: "removed".to_owned(),
            });
        }
    }
    differences.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(differences)
}

/// Native link destinations every agent enabled at the source scope needs in the target scope.
///
/// A move detaches the source-scope link, so each enabled agent must be re-deployed where it
/// actually reads skills in the destination scope. The caller resolves those directory roots
/// from the agent adapters here, because the engine owns no native path knowledge.
fn native_transfer_targets(
    connection: &rusqlite::Connection,
    paths: &AppPaths,
    project: Option<&at_db::projects::ProjectSummary>,
    item_id: ItemId,
    source_scope: Scope,
    target_scope: Scope,
) -> Result<Vec<NativeTransferTarget>, CommandError> {
    let deployments = at_db::deployments::list_deployments_for_item(connection, &item_id)
        .map_err(|_| CommandError::internal("skill deployments could not be read"))?;
    let enabled = deployments
        .iter()
        .filter(|cell| cell.scope == source_scope && cell.state == ItemState::Linked)
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        return Ok(Vec::new());
    }
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?;
    let env = AgentEnvironment {
        home: home.to_path_buf(),
        project: project.map(|row| PathBuf::from(&row.path)),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let mut targets = Vec::with_capacity(enabled.len());
    for cell in enabled {
        let agent = cell.agent_id.clone();
        let adapter = registry
            .get(&agent)
            .map_err(|_| CommandError::internal("agent registry is unavailable"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("agent_id", "agent adapter is unavailable")
            })?;
        let target_root = adapter
            .paths(ItemKind::Skill, target_scope, &env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .map(|candidate| candidate.path)
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::Conflict,
                    format!(
                        "{agent} has no skill folder in the destination scope; disable the skill \
                         for that agent before moving it"
                    ),
                )
            })?;
        let source_path = cell
            .target_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::Conflict,
                    format!(
                        "the recorded {agent} skill link is unusable; refresh the skill state \
                         before moving it"
                    ),
                )
            })?;
        targets.push(NativeTransferTarget {
            agent,
            source_path,
            target_root,
            mode: cell.link_mode.unwrap_or(LinkMode::Link),
            link_supported: adapter.capabilities(&env).supports_symlink,
        });
    }
    Ok(targets)
}

/// Roots a previewed skill operation may write to.
///
/// Authorization is derived from the involved store layouts, their backup folders and the
/// agent-native home/project directories only. A planned destination never grants itself
/// authorization: a ChangeSet op outside these roots is refused when the reviewed plan is
/// applied.
fn authorized_skill_roots(
    stores: &[&StoreLayout],
    backups: &[&Path],
    home: Option<&Path>,
    project: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = stores
        .iter()
        .map(|store| store.root().to_path_buf())
        .chain(backups.iter().map(|backup| (*backup).to_path_buf()))
        .collect();
    if let Some(home) = home {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = project {
        roots.push(project.to_path_buf());
    }
    roots
}

fn existing_apply_roots(authorized_roots: Vec<PathBuf>) -> Result<at_fs::ApplyRoots, CommandError> {
    let mut roots = Vec::new();
    for root in authorized_roots {
        if root.is_dir() {
            roots.push(root);
        } else if let Some(parent) = root.parent()
            && parent.is_dir()
        {
            roots.push(parent.to_path_buf());
        }
    }
    at_fs::ApplyRoots::new(roots)
        .map_err(|_| CommandError::unavailable("authorized skill operation roots are unavailable"))
}

fn transfer_error(error: at_engine::skill_transfer::SkillTransferError) -> CommandError {
    use at_engine::skill_transfer::SkillTransferError;
    match error {
        SkillTransferError::PermissionDenied => CommandError::unavailable(error.to_string()),
        SkillTransferError::NotBroader
        | SkillTransferError::DuplicateScopeMismatch
        | SkillTransferError::SameScope
        | SkillTransferError::NoTargetToReplace
        | SkillTransferError::MissingSource
        | SkillTransferError::Slug(_) => {
            CommandError::invalid_argument("transfer", &error.to_string())
        }
        SkillTransferError::UnmanagedTarget(path) => CommandError::new(
            CommandErrorCode::RefusedOverwrite,
            format!(
                "destination skill is unmanaged and will not be replaced: {}",
                path.display()
            ),
        ),
        SkillTransferError::Apply(at_fs::ChangeSetApplyError::Stale(path)) => CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "skill files changed since review at {}; preview again",
                path.display()
            ),
        ),
        _ => CommandError::new(CommandErrorCode::Conflict, error.to_string()),
    }
}

#[cfg(test)]
mod authorized_roots_tests {
    use super::{authorized_skill_roots, existing_apply_roots};
    use at_fs::{ApplyContext, OperationRecorder, apply_with_roots};
    use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};
    use std::path::Path;

    struct Recorder;

    impl OperationRecorder for Recorder {
        fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
            Ok("authorized-roots-test".to_owned())
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

    #[test]
    fn a_plan_destination_outside_the_layout_roots_is_refused() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let store_root = tmp.path().join("store");
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        for directory in [&store_root, &home, &outside] {
            std::fs::create_dir_all(directory).expect("fixture directory");
        }
        let store = at_store::layout::StoreLayout::new(Scope::User, store_root.clone());
        let roots = authorized_skill_roots(&[&store], &[], Some(&home), None);
        let apply_roots = existing_apply_roots(roots).expect("authorized roots");
        let change_set = ChangeSet {
            id: "cs-authorized-roots".to_owned(),
            preview_id: "pv-authorized-roots".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("known operation kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "plan destination outside the derived roots".to_owned(),
            ops: vec![FileOp::Write {
                path: outside.join("escaped.md"),
                bytes: b"outside".to_vec(),
                expected: ExpectedState::Absent,
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: 0,
        };
        let mut recorder = Recorder;
        let mut context = ApplyContext {
            store_root: &store_root,
            backup_folder: &home,
            recorder: &mut recorder,
        };
        let error = apply_with_roots(&change_set, &mut context, &apply_roots)
            .expect_err("a destination outside the derived roots is refused");
        assert!(
            matches!(error, at_fs::ChangeSetApplyError::OutsideAuthorizedRoots(_)),
            "the refusal is typed: {error:?}"
        );
    }
}
