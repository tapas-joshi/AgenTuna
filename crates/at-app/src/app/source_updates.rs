//! Explicit, policy-gated skill source checks and reviewed updates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use at_db::Database;
use at_engine::skill_source::{
    AutoUpdateScheduler, SkillSourceCheck, SkillUpdateDecision, SkillUpdatePlan,
    SkillUpdateRequest, SystemUpdateClock, UpdateClock, UpdateScheduleTrigger,
};
use at_model::network::{NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger};
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::warn;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::network::NetworkState;
use super::state::AppState;

/// Result of an explicit source check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillSourceCheckView {
    /// Stable result: update_available, current, source_gone, or unknown.
    pub status: String,
    /// Latest checked remote commit, when known.
    pub remote_sha: Option<String>,
    /// Whether the installed package differs from its imported snapshot.
    pub local_modified: Option<bool>,
}

/// A diff and identity for a reviewed update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillUpdatePreviewView {
    /// Preview identity required by apply.
    pub preview_id: String,
    /// File changes shown for review.
    pub files: Vec<SkillUpdateFileView>,
}

/// One file update in the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillUpdateFileView {
    /// Store-relative path.
    pub path: String,
    /// Unified diff or binary/size summary.
    pub diff: String,
    /// Whether apply creates a backup.
    pub backup_required: bool,
}

/// Result of applying a reviewed skill update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillUpdateApplyView {
    /// Operation journal identity.
    pub operation_id: String,
    /// Number of changed files.
    pub files_changed: usize,
}

#[derive(Clone)]
struct CachedUpdate {
    plan: SkillUpdatePlan,
    preview_id: String,
    store_root: PathBuf,
    backup_folder: PathBuf,
}

static UPDATE_PLANS: LazyLock<Mutex<HashMap<String, CachedUpdate>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Checks a Git-backed skill after the user explicitly confirms the network request.
#[tauri::command]
pub fn skill_source_check(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    item_id: String,
    confirmed: bool,
) -> Result<SkillSourceCheckView, CommandError> {
    if !confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let id = parse_skill_id(&item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = skill_item(database.connection(), &id)?;
    let source = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?
        .filter(|source| source.kind == at_db::sources::SourceKind::Git)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill has no live Git source.",
            )
        })?;
    let host = at_model::network::NetworkHost::from_url(source.url.as_deref().unwrap_or_default())
        .ok_or_else(|| {
            CommandError::invalid_argument("source", "the source URL has no valid network host")
        })?;
    network
        .authorize(NetworkAction::GitFetch, host.as_str(), NetworkTrigger::User)
        .map_err(|_| CommandError::unavailable("The network policy refused this source check."))?;
    let path = item
        .source_path
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The installed skill package is unavailable.",
            )
        })?;
    let mut events = NetworkEventLog::new();
    let checked = at_engine::skill_source::check_skill_source(
        database.connection(),
        &item_id,
        &path,
        &network.policy(),
        &mut events,
    )
    .map_err(|error| CommandError::unavailable(error.to_string()))?;
    let (status, remote_sha, local_modified) = match checked {
        SkillSourceCheck::UpdateAvailable {
            remote_sha,
            local_modified,
        } => ("update_available", Some(remote_sha), Some(local_modified)),
        SkillSourceCheck::Current {
            remote_sha,
            local_modified,
        } => ("current", Some(remote_sha), Some(local_modified)),
        SkillSourceCheck::SourceGone => ("source_gone", None, None),
        SkillSourceCheck::Unknown | SkillSourceCheck::NotRequested => ("unknown", None, None),
        _ => ("unknown", None, None),
    };
    Ok(SkillSourceCheckView {
        status: status.to_owned(),
        remote_sha,
        local_modified,
    })
}

/// Downloads a checked Git source into the private cache and prepares a reviewed package diff.
#[tauri::command]
pub fn skill_update_preview(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    item_id: String,
    confirmed: bool,
) -> Result<SkillUpdatePreviewView, CommandError> {
    if !confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let id = parse_skill_id(&item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = skill_item(database.connection(), &id)?;
    let source = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?
        .filter(|source| source.kind == at_db::sources::SourceKind::Git)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill has no live Git source.",
            )
        })?;
    let remote_sha = source.remote_sha.as_deref().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "Check the source before reviewing an update.",
        )
    })?;
    let url = source
        .url
        .as_deref()
        .ok_or_else(|| CommandError::invalid_argument("source", "the Git source URL is missing"))?;
    let host = at_model::network::NetworkHost::from_url(url).ok_or_else(|| {
        CommandError::invalid_argument("source", "the source URL has no valid network host")
    })?;
    network
        .authorize(NetworkAction::GitClone, host.as_str(), NetworkTrigger::User)
        .map_err(|_| {
            CommandError::unavailable("The network policy refused the update download.")
        })?;
    let project_path = if item.scope == at_model::Scope::Project {
        let project_id = item.project_id.as_deref().ok_or_else(|| {
            CommandError::unavailable("The project registry entry is unavailable.")
        })?;
        Some(
            at_db::projects::find_project_by_id(database.connection(), project_id)
                .map_err(|_| CommandError::internal("could not query project registry"))?
                .ok_or_else(|| {
                    CommandError::new(
                        CommandErrorCode::NotFound,
                        "The owning project is no longer registered.",
                    )
                })?
                .path,
        )
    } else {
        None
    };
    let mut paths = super::paths::resolve_process_paths(project_path.as_deref().map(Path::new))
        .map_err(|_| CommandError::unavailable("skill store paths are unavailable"))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load store path settings"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    let store = at_store::layout::StoreLayout::for_scope(&paths, item.scope)
        .map_err(|_| CommandError::unavailable("skill store is unavailable"))?;
    let slug = at_store::layout::Slug::parse(&item.slug)
        .map_err(|_| CommandError::internal("indexed skill slug is invalid"))?;
    let package_path = PathBuf::from("skills").join(slug.as_str());
    let local_package = store.skill_dir(&slug);
    let cache = paths.cache_dir().join("skill-updates");
    let mut events = NetworkEventLog::new();
    let repository = at_git::import::clone_for_import_with_process(
        url,
        source.git_ref.as_deref(),
        &cache,
        &network.policy(),
        &mut events,
    )
    .map_err(|error| {
        CommandError::unavailable(format!("could not fetch the skill source: {error}"))
    })?;
    if repository.commit_sha != remote_sha {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The source changed after its last check; check again before reviewing the update.",
        ));
    }
    let source_subpath = source
        .subpath
        .as_deref()
        .unwrap_or_else(|| package_path.to_str().unwrap_or(""));
    let source_subpath = at_store::manifest::StorePath::parse(source_subpath).map_err(|_| {
        CommandError::invalid_argument("source", "the source package path is invalid")
    })?;
    let remote_package = at_store::skill_package::SkillPackage::read(
        &repository.repository.join(source_subpath.as_str()),
    )
    .map_err(|_| CommandError::unavailable("The remote skill package is unavailable."))?;
    let request = SkillUpdateRequest {
        item_id: &item_id,
        local_package: &local_package,
        package_path: &package_path,
        store_root: store.root(),
        remote_package: &remote_package.package,
        remote_sha,
        scope: item.scope,
        project_id: item.project_id.as_deref(),
        override_local_modified: false,
        editor_has_unsaved_changes: false,
    };
    let decision = at_engine::skill_source::plan_skill_update(database.connection(), request)
        .map_err(|error| CommandError::unavailable(error.to_string()))?;
    let plan = match decision {
        SkillUpdateDecision::Review(plan) => *plan,
        SkillUpdateDecision::SkippedLocalModified => {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "Local edits were detected; the update will not overwrite them.",
            ));
        }
        SkillUpdateDecision::UpToDate => {
            return Err(CommandError::invalid_argument(
                "item_id",
                "the skill is already up to date",
            ));
        }
        SkillUpdateDecision::PausedEditorUnsavedChanges => {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "Save or discard the open skill edit before updating.",
            ));
        }
        _ => {
            return Err(CommandError::new(
                CommandErrorCode::Conflict,
                "The update plan is not supported.",
            ));
        }
    };
    let preview = SkillUpdatePreviewView {
        preview_id: plan.preview.id.clone(),
        files: plan
            .preview
            .files
            .iter()
            .map(|file| SkillUpdateFileView {
                path: file.path.display().to_string(),
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
            .collect(),
    };
    UPDATE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("skill update review cache is unavailable"))?
        .insert(
            preview.preview_id.clone(),
            CachedUpdate {
                plan,
                preview_id: preview.preview_id.clone(),
                store_root: store.root().to_path_buf(),
                backup_folder: store.backups_dir(),
            },
        );
    Ok(preview)
}

/// Applies exactly the update diff explicitly confirmed by the user.
#[tauri::command]
pub fn skill_update_apply(
    state: State<'_, AppState>,
    preview_id: String,
    confirmed: bool,
) -> Result<SkillUpdateApplyView, CommandError> {
    if !confirmed {
        return Err(CommandError::invalid_argument(
            "confirmed",
            "explicit confirmation is required",
        ));
    }
    let cached = UPDATE_PLANS
        .lock()
        .map_err(|_| CommandError::internal("skill update review cache is unavailable"))?
        .remove(&preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "the reviewed update expired; review it again",
            )
        })?;
    if cached.preview_id != preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "the reviewed update identity does not match",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &cached.store_root,
        backup_folder: &cached.backup_folder,
        recorder: &mut recorder,
    };
    let result = at_engine::skill_source::apply_skill_update(
        database.connection(),
        &cached.plan,
        &mut context,
    )
    .map_err(|error| match error {
        at_engine::skill_source::SkillUpdateError::Preview(
            at_engine::preview::PreviewError::ConcurrentModification,
        ) => CommandError::new(
            CommandErrorCode::Conflict,
            "Files changed since review; preview the update again.",
        ),
        other => CommandError::unavailable(other.to_string()),
    })?;
    state.record_writes(&cached.plan.change_set, &cached.store_root);
    Ok(SkillUpdateApplyView {
        operation_id: result.operation_id,
        files_changed: cached.plan.change_set.ops.len(),
    })
}

fn parse_skill_id(item_id: &str) -> Result<at_model::ItemId, CommandError> {
    item_id
        .parse()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill identity"))
}

fn skill_item(
    connection: &rusqlite::Connection,
    id: &at_model::ItemId,
) -> Result<at_db::shared::ItemRow, CommandError> {
    at_db::shared::items_by_ids(connection, std::slice::from_ref(id))
        .map_err(|_| CommandError::internal("could not query skill index"))?
        .into_iter()
        .next()
        .filter(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill is no longer in the index.",
            )
        })
}

/// Runs the first launch pass for skills whose persisted source preference allows auto-update.
///
/// The injected clock and engine scheduler keep launch eligibility deterministic in tests.
///
/// # Errors
/// Returns a redacted database or scheduler failure.
pub fn run_launch_auto_checks(
    database: &Mutex<Database>,
    network: &NetworkState,
    clock: &impl UpdateClock,
) -> Result<usize, String> {
    let database = database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?;
    let mut scheduler = AutoUpdateScheduler::default();
    let due = scheduler
        .due_items(
            database.connection(),
            UpdateScheduleTrigger::Launch,
            Duration::from_secs(24 * 60 * 60),
            false,
            clock,
        )
        .map_err(|_| "could not load scheduled skill sources".to_owned())?;
    if due.is_empty() {
        return Ok(0);
    }
    let policy: NetworkPolicy = network.policy().with_updates_opt_in(true);
    let mut completed = 0;
    for item_id in due {
        let item = match skill_item(database.connection(), &item_id) {
            Ok(item) => item,
            Err(error) => {
                warn!(%error, item_id = %item_id, "scheduled skill update skipped");
                continue;
            }
        };
        let Some(local_package) = item.source_path.as_deref() else {
            continue;
        };
        let mut events = NetworkEventLog::new();
        match at_engine::skill_source::check_skill_source_if_requested(
            database.connection(),
            &item_id.to_string(),
            Path::new(local_package),
            false,
            &policy,
            &mut events,
        ) {
            Ok(SkillSourceCheck::NotRequested) => {}
            Ok(_) => completed += 1,
            Err(error) => warn!(%error, item_id = %item_id, "scheduled skill update check failed"),
        }
    }
    Ok(completed)
}

/// Production clock for the launch auto-update pass.
#[must_use]
pub fn system_update_clock() -> SystemUpdateClock {
    SystemUpdateClock
}
