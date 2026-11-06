//! Typed Tauri commands for the project picker registry.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use ts_rs::TS;

use super::bridge::{JobError, JobId, JobSpec, ProgressSender};
use super::commands::CommandError;
use super::state::AppState;

/// One registered project row displayed by the project picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ProjectView {
    /// Stable opaque project ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Canonical project folder path.
    pub path: String,
    /// Last selected or added time in UTC RFC 3339 format.
    pub last_used_at: Option<String>,
    /// Number of skills owned by or deployed into this project.
    pub skill_count: i64,
    /// Number of MCP servers owned by or deployed into this project.
    pub server_count: i64,
}

impl From<at_db::projects::ProjectSummary> for ProjectView {
    fn from(project: at_db::projects::ProjectSummary) -> Self {
        Self {
            id: project.id,
            name: project.name,
            path: project.path,
            last_used_at: project.last_used_at,
            skill_count: project.skill_count,
            server_count: project.server_count,
        }
    }
}

/// Lists visible registered projects, with item counts from the database summary query.
#[tauri::command]
pub fn projects_list(state: State<'_, AppState>) -> Result<Vec<ProjectView>, CommandError> {
    let database = state
        .database()
        .map_err(|_| CommandError::unavailable("project database is unavailable"))?;
    let projects = at_db::projects::list_projects(database.connection())
        .map_err(|_| CommandError::internal("could not read the project registry"))?;
    Ok(projects
        .into_iter()
        .filter(|project| !project.hidden)
        .map(ProjectView::from)
        .collect())
}

/// Registers the selected project folder, returning its canonical registered row.
#[tauri::command]
pub fn projects_add(state: State<'_, AppState>, path: String) -> Result<ProjectView, CommandError> {
    if path.trim().is_empty() || path.len() > 4096 {
        return Err(CommandError::invalid_argument(
            "path",
            "expected a non-empty path up to 4096 bytes",
        ));
    }
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err(CommandError::invalid_argument(
            "path",
            "expected an absolute folder path",
        ));
    }
    let database = state
        .database()
        .map_err(|_| CommandError::unavailable("project database is unavailable"))?;
    let added =
        at_db::projects::add_project(database.connection(), &path, None, None).map_err(|_| {
            CommandError::invalid_argument("path", "folder is unreadable or does not exist")
        })?;
    let project = at_db::projects::find_project_by_id(database.connection(), &added.id)
        .map_err(|_| CommandError::internal("could not read the registered project"))?
        .ok_or_else(|| CommandError::internal("registered project was not found"))?;
    Ok(ProjectView::from(project))
}

/// Opens the native folder picker and registers the selected folder.
///
/// # Errors
/// Returns a typed error when the dialog, database, or selected folder is unavailable.
#[tauri::command]
pub async fn projects_choose_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ProjectView>, CommandError> {
    let dialog_app = app.clone();
    let selected = tokio::task::spawn_blocking(move || {
        super::dialogs::choose_folder(&dialog_app, "Add Project Folder")
    })
    .await
    .map_err(|_| CommandError::internal("native folder picker failed"))?
    .map_err(|_| CommandError::internal("native folder picker returned an invalid path"))?;
    let Some(path) = selected else {
        return Ok(None);
    };
    add_path(&state, &path).map(Some)
}

/// Starts a cancellable project scan and returns its bridge job identifier.
///
/// The UI listens for ordinary job progress/finished events and reloads the registry
/// after completion. The scan never executes project contents.
#[tauri::command]
pub fn projects_scan(state: State<'_, AppState>) -> Result<JobId, CommandError> {
    let database = state
        .database_handle()
        .map_err(|_| CommandError::unavailable("project database is unavailable"))?;
    let (known, roots) = {
        let database_guard = database
            .lock()
            .map_err(|_| CommandError::internal("project database lock is unavailable"))?;
        let summaries = at_db::projects::list_projects(database_guard.connection())
            .map_err(|_| CommandError::internal("could not read the project registry"))?;
        let known = summaries
            .iter()
            .map(|project| PathBuf::from(&project.path))
            .collect::<Vec<_>>();
        let paths = super::paths::resolve_process_paths(None)
            .map_err(|_| CommandError::unavailable("project scan roots are unavailable"))?;
        let mut config = paths
            .home_dir()
            .map(at_store::project_scan::ScanConfig::defaults_for)
            .unwrap_or_default();
        for root in summaries
            .into_iter()
            .filter_map(|project| project.source_root)
        {
            config = config.with_root(root);
        }
        (known, config)
    };
    let cancel = at_store::project_scan::CancelFlag::new();
    let job_cancel = cancel.clone();
    let database_for_job = Arc::clone(&database);
    let job = JobSpec::new("Scan projects", move |progress: ProgressSender| {
        let observer = Arc::new(ScanProgressObserver {
            cancel: job_cancel.clone(),
            progress: Some(progress),
        });
        let config = roots.clone();
        let known = known.clone();
        let database = Arc::clone(&database_for_job);
        async move {
            let _cancel_guard = CancelOnDrop(job_cancel);
            let outcome = at_store::project_scan::spawn_scan(config, observer)
                .await
                .map_err(|_| JobError::Failed {
                    message: "project scan worker failed".to_owned(),
                })?;
            if outcome.cancelled {
                return Ok(());
            }
            let diff = at_store::project_scan::ProjectDiff::between(&outcome.projects, &known);
            let mut database = database.lock().map_err(|_| JobError::Failed {
                message: "project database lock is unavailable".to_owned(),
            })?;
            let mut registry = DbProjectRegistry {
                database: &mut database,
            };
            at_store::project_scan::apply_diff(&diff, &mut registry).map_err(|_| {
                JobError::Failed {
                    message: "project registry could not be updated".to_owned(),
                }
            })?;
            Ok(())
        }
    });
    state
        .bridge()
        .spawn_job(job)
        .map(|handle| handle.id().clone())
        .map_err(CommandError::from)
}

fn add_path(state: &AppState, path: &Path) -> Result<ProjectView, CommandError> {
    let database = state
        .database()
        .map_err(|_| CommandError::unavailable("project database is unavailable"))?;
    let added =
        at_db::projects::add_project(database.connection(), path, None, None).map_err(|_| {
            CommandError::invalid_argument("path", "folder is unreadable or does not exist")
        })?;
    let project = at_db::projects::find_project_by_id(database.connection(), &added.id)
        .map_err(|_| CommandError::internal("could not read the registered project"))?
        .ok_or_else(|| CommandError::internal("registered project was not found"))?;
    Ok(ProjectView::from(project))
}

struct ScanProgressObserver {
    cancel: at_store::project_scan::CancelFlag,
    progress: Option<ProgressSender>,
}

impl at_store::project_scan::ScanObserver for ScanProgressObserver {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn progress(&self, progress: &at_store::project_scan::ScanProgress) {
        if let Some(sender) = &self.progress {
            sender.message(format!(
                "{} projects · {} entries · {}",
                progress.projects_found,
                progress.visited,
                progress.current.display()
            ));
        }
    }
}

struct CancelOnDrop(at_store::project_scan::CancelFlag);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct DbProjectRegistry<'a> {
    database: &'a mut at_db::Database,
}

impl at_store::project_scan::ProjectRegistry for DbProjectRegistry<'_> {
    fn insert_discovered(
        &mut self,
        project: &at_store::project_scan::DiscoveredProject,
    ) -> Result<(), at_store::project_scan::ScanApplyError> {
        at_db::projects::add_project(
            self.database.connection(),
            &project.path,
            Some(&project.name),
            Some(&project.source_root),
        )
        .map(|_| ())
        .map_err(|error| at_store::project_scan::ScanApplyError {
            path: project.path.clone(),
            message: error.to_string(),
        })
    }

    fn mark_missing(&mut self, path: &Path) -> Result<(), at_store::project_scan::ScanApplyError> {
        let project = at_db::projects::find_project_by_path(self.database.connection(), path)
            .map_err(|error| at_store::project_scan::ScanApplyError {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        if let Some(project) = project {
            at_db::projects::hide_project(self.database.connection(), &project.id).map_err(
                |error| at_store::project_scan::ScanApplyError {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                },
            )?;
        }
        Ok(())
    }
}
