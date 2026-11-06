//! Path resolution for the Tauri shell.
//!
//! The shell resolves [`AppPaths`] once from the process environment and hands the values to
//! commands. This module adds no policy of its own: the ordering rules live in
//! [`at_store::paths`], so the CLI and the desktop app resolve identical roots.

use std::path::Path;

use at_store::paths::{AppPaths, PathEnvironment, PathError};
use serde::Serialize;
use ts_rs::TS;

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
