//! Rebuild integration using [`AppPaths`] (TF-019).
//!
//! Provides [`rebuild_from_app_paths`] to rebuild all derived tables
//! across Global, User, and Project stores resolved by [`AppPaths`].
use std::path::Path;

use at_db::Database;
pub use at_db::rebuild::{
    HistoryCounts, NormalizedDump, RebuildCounts, RebuildError, RebuildObserver, RebuildPaths,
    RebuildProgress, RebuildReport, RebuildState, RebuildStatus, normalized_derived_dump,
    read_rebuild_state, rebuild, snapshot_history_counts,
};

use crate::paths::AppPaths;

/// Rebuilds all derived database tables using paths from [`AppPaths`].
///
/// # Errors
/// Returns [`RebuildError`] if rebuild or scanning fails.
pub fn rebuild_from_app_paths(
    db: &mut Database,
    paths: &AppPaths,
    observer: Option<&dyn RebuildObserver>,
) -> Result<RebuildReport, RebuildError> {
    let home = paths
        .home_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.user_store().to_path_buf());

    let mut rebuild_paths =
        RebuildPaths::new(home, paths.user_store()).with_global_store(paths.global_store());

    if let Some(proj_store) = paths.project_store() {
        let proj_root = proj_store.parent().unwrap_or(proj_store);
        rebuild_paths = rebuild_paths.with_project_and_store(proj_root, proj_store);
    }

    rebuild(db.connection_mut(), &rebuild_paths, observer)
}
