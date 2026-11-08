//! Path resolution for the Tauri shell.
//!
//! The shell resolves [`AppPaths`] once from the process environment and hands the values to
//! commands. This module adds no policy of its own: the ordering rules live in
//! [`at_store::paths`], so the CLI and the desktop app resolve identical roots.

use std::path::{Path, PathBuf};

use at_store::paths::{AppPaths, PathEnvironment, PathError};
use serde::Serialize;
use ts_rs::TS;

use super::commands::CommandError;

/// Resolves every root from the process environment, optionally for a project.
///
/// # Errors
/// [`PathError`] when the home directory is unavailable or the project root is invalid.
pub fn resolve_process_paths(project_root: Option<&Path>) -> Result<AppPaths, PathError> {
    let mut environment = PathEnvironment::from_process();
    if let Some(root) = project_root {
        environment = environment.with_project_root(root);
    }
    AppPaths::resolve(&environment)
}

/// Resolves the paths one command operates on.
///
/// An [`AppPaths`] injected into the [`super::state::AppState`] wins over process resolution, so a test or
/// recovery session never reads a developer's real home; the selected project root is
/// re-resolved through the same central path policy either way. Production resolves from the
/// process environment exactly like the CLI.
///
/// # Errors
/// Returns [`CommandError`] when no home can be resolved or the project root is invalid.
pub fn resolve_command_paths(
    state: &super::state::AppState,
    project_root: Option<&Path>,
) -> Result<AppPaths, CommandError> {
    match state.paths() {
        Some(paths) if project_root.is_none() => Ok(paths.clone()),
        Some(paths) => paths
            .for_project(project_root.expect("project root checked above"))
            .map_err(|_| CommandError::unavailable("application paths are unavailable")),
        None => resolve_process_paths(project_root)
            .map_err(|_| CommandError::unavailable("application paths are unavailable")),
    }
}

/// The database-selected project root, when the selection still resolves to a directory.
///
/// The desktop shell resolves its process paths without a project (`resolve_process_paths(None)`),
/// so commands that operate on the selected project look the selection up here, the same way the
/// CLI takes `--project`.
///
/// # Errors
/// Returns [`CommandError`] when the database or the settings table cannot be read.
pub(crate) fn active_project_root(
    state: &super::state::AppState,
) -> Result<Option<PathBuf>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not read application settings"))?;
    let Some(id) = settings.last_project_id else {
        return Ok(None);
    };
    let project = at_db::projects::find_project_by_id(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not read the selected project"))?;
    Ok(project
        .map(|project| PathBuf::from(project.path))
        .filter(|path| path.is_dir()))
}

/// Resolves the paths one command operates on for the database-selected project.
///
/// # Errors
/// See [`resolve_command_paths`].
pub(crate) fn resolve_active_paths(
    state: &super::state::AppState,
) -> Result<AppPaths, CommandError> {
    let project = active_project_root(state)?;
    resolve_command_paths(state, project.as_deref())
}

/// Serializable snapshot of [`AppPaths`] for the webview, which never touches the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AppPathsView {
    /// Support directory (database, logs, backups, cache).
    pub support_dir: String,
    /// SQLite index path.
    pub db_path: String,
    /// Log directory.
    pub log_dir: String,
    /// Backup root.
    pub backup_dir: String,
    /// Cache directory.
    pub cache_dir: String,
    /// Per-user store.
    pub user_store: String,
    /// Per-project store, when a project is selected.
    pub project_store: Option<String>,
    /// Machine-wide Global store.
    pub global_store: String,
    /// Where the Global store came from (`environment`, `settings` or `default`).
    pub global_store_source: String,
}

impl From<&AppPaths> for AppPathsView {
    fn from(paths: &AppPaths) -> Self {
        Self {
            support_dir: paths.support_dir().display().to_string(),
            db_path: paths.db_path().display().to_string(),
            log_dir: paths.log_dir().display().to_string(),
            backup_dir: paths.backup_dir().display().to_string(),
            cache_dir: paths.cache_dir().display().to_string(),
            user_store: paths.user_store().display().to_string(),
            project_store: paths.project_store().map(|path| path.display().to_string()),
            global_store: paths.global_store().display().to_string(),
            global_store_source: paths.global_store_source().as_str().to_owned(),
        }
    }
}
