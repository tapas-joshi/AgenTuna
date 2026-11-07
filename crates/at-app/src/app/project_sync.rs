//! Typed commands for reviewing and applying project/library skill syncs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use at_engine::project_sync::{
    DatabaseProjectSyncProvider, ProjectSyncProvider, SyncAction, SyncDirection, SyncPlan,
    SyncTreeRequest, package_files, package_hashes,
};
use at_model::{ItemId, Scope, SyncLabel};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;

/// One project skill's library-sync status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ProjectSyncItemView {
    /// Stable skill identity.
    pub item_id: String,
    /// Scope of the indexed copy.
    pub scope: String,
    /// Display name.
    pub name: String,
    /// Existing TA-005 synchronization label, when a baseline exists.
    pub label: Option<String>,
    /// Permitted action: `push`, `pull`, `compare`, `none`, or `untracked`.
    pub action: String,
}

/// One file effect in a reviewed project-sync preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ProjectSyncFileView {
    /// Destination path relative to the target store.
    pub path: String,
    /// `write` for a new or replaced file.
    pub action: String,
    /// Unified diff or a concise binary/size summary.
    pub diff: String,
    /// Whether a backup is taken before replacement.
    pub backup_required: bool,
}

/// Exact diff and identity the user reviews before a project/library sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ProjectSyncPreviewView {
    /// Content-bound preview identity required by apply.
    pub preview_id: String,
    /// Skill being synchronized.
    pub item_id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Direction reviewed.
    pub direction: String,
    /// Files changed by the plan.
    pub files: Vec<ProjectSyncFileView>,
    /// Destination-only paths retained rather than deleted.
    pub missing_source_paths: Vec<String>,
}

/// Result of applying a reviewed project-sync plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ProjectSyncApplyView {
    /// Operation journal identity.
    pub operation_id: String,
    /// Number of files written.
    pub files_changed: usize,
}

#[derive(Clone)]
struct CachedPlan {
    change_set: at_model::ChangeSet,
    preview_root: PathBuf,
    preview_id: String,
    project_id: String,
    item_id: ItemId,
    next_baseline: String,
    target_root: PathBuf,
    backup_folder: PathBuf,
    apply_roots: Vec<PathBuf>,
}

static PLAN_CACHE: LazyLock<Mutex<HashMap<String, CachedPlan>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn plan_cache() -> &'static Mutex<HashMap<String, CachedPlan>> {
    &PLAN_CACHE
}

/// Lists existing TA-005 sync labels for the selected project's skills.
#[tauri::command]
pub fn project_sync_status(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<ProjectSyncItemView>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let _project = at_db::projects::find_project_by_id(database.connection(), &project_id)
        .map_err(|_| CommandError::internal("could not query project registry"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The selected project is no longer registered.",
            )
        })?;
    let skills = at_db::skills_index::list_skills(
        database.connection(),
        &at_db::skills_index::SkillFilter::default(),
    )
    .map_err(|_| CommandError::internal("could not query User and Project skills"))?;
    let item_ids = skills
        .iter()
        .filter_map(|row| row.item_id)
        .collect::<Vec<_>>();
    let items = at_db::shared::items_by_ids(database.connection(), &item_ids)
        .map_err(|_| CommandError::internal("could not query project skill records"))?;
    let item_map = items
        .into_iter()
        .map(|item| (item.id, (item.scope, item.project_id)))
        .collect::<HashMap<_, _>>();
    let provider = DatabaseProjectSyncProvider {
        database: &database,
        project_id: &project_id,
        version: 0,
    };
    skills
        .into_iter()
        .filter_map(|skill| {
            let item_id = skill.item_id?;
            let (scope, owning_project) = item_map.get(&item_id)?;
            if *scope != Scope::User
                && (*scope != Scope::Project
                    || owning_project.as_deref() != Some(project_id.as_str()))
            {
                return None;
            }
            Some((item_id, skill.name, scope.to_string()))
        })
        .map(|(item_id, name, scope)| {
            let record = provider
                .sync_record(item_id)
                .map_err(|_| CommandError::internal("could not read project sync state"))?;
            let (label, action) = record.map_or((None, "untracked".to_owned()), |record| {
                let label = match record.label {
                    SyncLabel::InSync => "in_sync",
                    SyncLabel::ProjectOnly => "project_only",
                    SyncLabel::LibraryNewer => "library_newer",
                    SyncLabel::ProjectNewer => "project_newer",
                    SyncLabel::LibraryOnly => "library_only",
                    SyncLabel::Conflict => "conflict",
                    SyncLabel::Drifted => "drifted",
                    _ => "unknown",
                };
                let action = match at_engine::project_sync::action_for(record.label) {
                    SyncAction::None => "none",
                    SyncAction::Push => "push",
                    SyncAction::Pull => "pull",
                    SyncAction::Compare => "compare",
                    _ => "compare",
                };
                (Some(label.to_owned()), action.to_owned())
            });
            Ok(ProjectSyncItemView {
                item_id: item_id.to_string(),
                scope,
                name,
                label,
                action,
            })
        })
        .collect()
}

/// Builds and caches a reviewed ChangeSet for copying one skill between Project and User.
#[tauri::command]
pub fn project_sync_preview(
    state: State<'_, AppState>,
    project_id: String,
    item_id: String,
    direction: String,
) -> Result<ProjectSyncPreviewView, CommandError> {
    let direction = parse_direction(&direction)?;
    let item_id = item_id
        .parse::<ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill identity"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = at_db::projects::find_project_by_id(database.connection(), &project_id)
        .map_err(|_| CommandError::internal("could not query project registry"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The selected project is no longer registered.",
            )
        })?;
    let item = at_db::shared::items_by_ids(database.connection(), std::slice::from_ref(&item_id))
        .map_err(|_| CommandError::internal("could not query skill index"))?
        .into_iter()
        .next()
        .filter(|item| {
            item.kind == at_model::ItemKind::Skill
                && (item.scope == Scope::User
                    || (item.scope == Scope::Project
                        && item.project_id.as_deref() == Some(project_id.as_str())))
        })
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This project skill is no longer available.",
            )
        })?;
    let mut paths = super::paths::resolve_process_paths(Some(Path::new(&project.path)))
        .map_err(|_| CommandError::unavailable("project store paths are unavailable"))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load store path settings"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    let project_store = at_store::layout::StoreLayout::for_scope(&paths, Scope::Project)
        .map_err(|_| CommandError::unavailable("project store is unavailable"))?;
    let user_store = at_store::layout::StoreLayout::for_scope(&paths, Scope::User)
        .map_err(|_| CommandError::unavailable("User store is unavailable"))?;
    let slug = at_store::layout::Slug::parse(&item.slug)
        .map_err(|_| CommandError::internal("indexed skill slug is invalid"))?;
    let (source_store, target_store) = match direction {
        SyncDirection::Push => (&project_store, &user_store),
        SyncDirection::Pull => (&user_store, &project_store),
        _ => {
            return Err(CommandError::invalid_argument(
                "direction",
                "unsupported project sync direction",
            ));
        }
    };
    let source_path = source_store.skill_dir(&slug);
    let target_path = target_store.skill_dir(&slug);
    let source = at_store::skill_package::SkillPackage::read(&source_path).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The source skill package is unavailable.",
        )
    })?;
    let destination = match at_store::skill_package::SkillPackage::read(&target_path) {
        Ok(loaded) => Some(loaded),
        Err(at_store::skill_package::SkillPackageError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => {
            return Err(CommandError::unavailable(format!(
                "The destination package cannot be safely read: {error}"
            )));
        }
    };
    let provider = DatabaseProjectSyncProvider {
        database: &database,
        project_id: &project_id,
        version: 0,
    };
    let record = provider
        .sync_record(item_id)
        .map_err(|_| CommandError::internal("could not read project sync state"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "This item has no project sync baseline.",
            )
        })?;
    let source_files = package_files(&source.package);
    let source_file_hashes = source_files
        .iter()
        .map(|(path, bytes)| (path.clone(), at_fs::sha256_hex(bytes)))
        .collect();
    let destination_files = destination
        .as_ref()
        .map(|loaded| package_hashes(&loaded.package))
        .unwrap_or_default();
    let direction_name = match direction {
        SyncDirection::Push => "push",
        SyncDirection::Pull => "pull",
        _ => {
            return Err(CommandError::invalid_argument(
                "direction",
                "unsupported project sync direction",
            ));
        }
    };
    let plan = at_engine::project_sync::plan_sync_tree(SyncTreeRequest {
        item_id,
        project_id: project_id.clone(),
        direction,
        label: record.label,
        base_hash: record.evidence.base_hash,
        destination_hash: destination.as_ref().map(|loaded| loaded.hash.to_string()),
        source_hash: source.hash.to_string(),
        force_reviewed: true,
        destination_root_exists: target_store.root().is_dir(),
        source_files,
        source_file_hashes,
        destination_files,
    })
    .map_err(sync_error)?;
    let (change_set, missing_source_paths) = match plan {
        SyncPlan::Change(change_set) => (change_set, Vec::new()),
        SyncPlan::ChangeWithMissing {
            change_set,
            missing_source_paths,
        } => (change_set, missing_source_paths),
        SyncPlan::AlreadyInSync => {
            return Err(CommandError::new(
                CommandErrorCode::InvalidArgument,
                "The two skill packages already match.",
            ));
        }
        SyncPlan::MissingSide(paths) => {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                format!(
                    "The destination contains files absent from the source: {}",
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        _ => {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "The sync plan is not supported.",
            ));
        }
    };
    let preview = at_engine::preview::preview(&change_set, target_store.root()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Conflict,
            "The sync preview could not be safely read; reload and compare the copies.",
        )
    })?;
    let preview_id = preview.id.clone();
    let files = preview
        .files
        .iter()
        .map(|file| ProjectSyncFileView {
            path: file.path.display().to_string(),
            action: file.action.clone(),
            diff: match &file.diff {
                at_engine::preview::DiffKind::Unified { text, .. } => text.clone(),
                at_engine::preview::DiffKind::Binary { size } => {
                    format!("Binary file ({size} bytes)")
                }
                at_engine::preview::DiffKind::TooLarge { size } => {
                    format!("File too large to display ({size} bytes)")
                }
                at_engine::preview::DiffKind::Unchanged => String::new(),
                _ => "This file type cannot be previewed.".to_owned(),
            },
            backup_required: file.backup_required,
        })
        .collect();
    let cache = CachedPlan {
        change_set,
        preview_root: target_store.root().to_path_buf(),
        preview_id: preview_id.clone(),
        project_id,
        item_id,
        next_baseline: source.hash.to_string(),
        target_root: target_store.root().to_path_buf(),
        backup_folder: target_store.backups_dir(),
        apply_roots: vec![target_store.root().to_path_buf()],
    };
    plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("sync plan cache is unavailable"))?
        .insert(preview_id.clone(), cache);
    Ok(ProjectSyncPreviewView {
        preview_id,
        item_id: item_id.to_string(),
        name: item.name,
        direction: direction_name.to_owned(),
        files,
        missing_source_paths: missing_source_paths
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

/// Applies a user-confirmed project sync only if its exact reviewed destination is still current.
#[tauri::command]
pub fn project_sync_apply(
    state: State<'_, AppState>,
    preview_id: String,
    confirmed: bool,
) -> Result<ProjectSyncApplyView, CommandError> {
    if !confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let cached = plan_cache()
        .lock()
        .map_err(|_| CommandError::internal("sync plan cache is unavailable"))?
        .remove(&preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument("preview_id", "the reviewed sync expired; preview again")
        })?;
    if cached.preview_id != preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "the reviewed sync identity does not match",
        ));
    }
    let current =
        at_engine::preview::preview(&cached.change_set, &cached.preview_root).map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "Files changed since review; preview the sync again.",
            )
        })?;
    if current.id != cached.preview_id {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "Files changed since review; preview the sync again.",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.apply_roots.clone())
        .map_err(|_| CommandError::unavailable("authorized project sync roots are unavailable"))?;
    let outcome = {
        let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
        let mut context = at_fs::changeset::ApplyContext {
            store_root: &cached.target_root,
            backup_folder: &cached.backup_folder,
            recorder: &mut recorder,
        };
        at_fs::changeset::apply_with_roots(&cached.change_set, &mut context, &roots).map_err(
            |error| match error {
                at_fs::changeset::ChangeSetApplyError::Stale(_) => CommandError::new(
                    CommandErrorCode::Conflict,
                    "Files changed since review; preview the sync again.",
                ),
                other => CommandError::from(other),
            },
        )?
    };
    state.record_writes(&cached.change_set, &cached.target_root);
    at_db::deployments::update_project_sync_baseline(
        database.connection(),
        &cached.item_id,
        &cached.project_id,
        &cached.next_baseline,
    )
    .map_err(|_| {
        CommandError::internal("sync succeeded but the project baseline could not be updated")
    })?;
    Ok(ProjectSyncApplyView {
        operation_id: outcome.operation_id,
        files_changed: cached.change_set.ops.len(),
    })
}

fn parse_direction(value: &str) -> Result<SyncDirection, CommandError> {
    match value {
        "push" => Ok(SyncDirection::Push),
        "pull" => Ok(SyncDirection::Pull),
        _ => Err(CommandError::invalid_argument(
            "direction",
            "choose push or pull",
        )),
    }
}

fn sync_error(error: at_engine::project_sync::SyncPlanError) -> CommandError {
    use at_engine::project_sync::SyncPlanError;
    match error {
        SyncPlanError::Conflict | SyncPlanError::TargetChanged => {
            CommandError::new(CommandErrorCode::Conflict, error.to_string())
        }
        SyncPlanError::DirectionNotAllowed { .. } => {
            CommandError::invalid_argument("direction", &error.to_string())
        }
        _ => CommandError::unavailable(error.to_string()),
    }
}
