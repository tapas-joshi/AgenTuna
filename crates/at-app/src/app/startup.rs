//! Startup database recovery: quarantine handling, derived-data rebuild and warnings.
//!
//! [`at_db::Database::open_file`] never hands the shell a damaged index: a file that fails
//! `PRAGMA quick_check` is renamed to `agentuna.db.corrupt-<stamp>` and an empty database takes
//! its place (TF-020). Two things must follow, and this module owns both:
//!
//! 1. **Rebuild derived data.** Every table except history and the backup catalogue is a
//!    projection of the filesystem (TF-019), so a fresh index is filled by one `at_db::rebuild`.
//! 2. **Report, do not block.** The shell starts, and a [`StartupWarningView`] carries the
//!    quarantine path so the Overview banner can name the file the user may want to inspect.
//!
//! A database that cannot be opened at all is treated the same way: the file is left exactly as
//! it was, the app starts without an index, and the warning names the file and the reason. History
//! in an unreadable file is never discarded to "make startup work".

use std::path::Path;

use at_db::Database;
use at_db::rebuild::{RebuildPaths, RebuildReport, rebuild};
use at_store::paths::AppPaths;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A non-blocking problem the shell reports after startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct StartupWarningView {
    /// Stable machine-readable kind: `database_quarantined`, `derived_rebuild_failed` or
    /// `index_unavailable`.
    pub kind: String,
    /// One-line message shown in the banner.
    pub message: String,
    /// File the warning refers to, when one exists.
    pub path: Option<String>,
}

impl StartupWarningView {
    /// A corrupt index was quarantined and an empty one replaced it.
    #[must_use]
    pub fn quarantined(quarantine: &Path) -> Self {
        Self {
            kind: "database_quarantined".to_owned(),
            message: format!(
                "The index was damaged and moved to {}. AgenTuna rebuilt it from your stores; \
                 the damaged file is kept for inspection.",
                quarantine.display()
            ),
            path: Some(quarantine.display().to_string()),
        }
    }

    /// The index was rebuilt but the rebuild itself failed.
    #[must_use]
    pub fn rebuild_failed(quarantine: &Path, reason: &str) -> Self {
        Self {
            kind: "derived_rebuild_failed".to_owned(),
            message: format!(
                "The index was moved to {} and could not be rebuilt: {reason}. \
                 Fix the store and run a rebuild from the menu.",
                quarantine.display()
            ),
            path: Some(quarantine.display().to_string()),
        }
    }

    /// The index file could not be opened at all, so the app runs without one.
    #[must_use]
    pub fn index_unavailable(database_path: &Path, reason: &str) -> Self {
        Self {
            kind: "index_unavailable".to_owned(),
            message: format!(
                "The index at {} could not be opened: {reason}. \
                 Nothing was changed on disk; fix the file and restart.",
                database_path.display()
            ),
            path: Some(database_path.display().to_string()),
        }
    }
}

/// The opened database plus everything startup had to report about it.
#[derive(Debug)]
pub struct OpenedDatabase {
    /// The open handle, or `None` when the file could not be opened at all.
    pub database: Option<Database>,
    /// Warnings for the shell, in the order they happened.
    pub warnings: Vec<StartupWarningView>,
    /// Rebuild summary when derived tables had to be rebuilt.
    pub rebuild: Option<RebuildReport>,
}

/// Opens `paths.db_path()`, rebuilds after a quarantine and collects startup warnings.
///
/// Never panics and never leaves the shell without a decision: a fatal open failure becomes a
/// warning plus `database: None`, so the UI can still explain itself.
#[must_use]
pub fn open_database(paths: &AppPaths) -> OpenedDatabase {
    let mut database = match Database::open_file(paths.db_path()) {
        Ok(database) => database,
        Err(error) => {
            tracing::error!(%error, "application database unavailable");
            return OpenedDatabase {
                database: None,
                warnings: vec![StartupWarningView::index_unavailable(
                    paths.db_path(),
                    &error.to_string(),
                )],
                rebuild: None,
            };
        }
    };
    let quarantine = database.startup_warning().map(Path::to_path_buf);
    let is_fresh = is_database_empty(database.connection());
    if quarantine.is_none() && !is_fresh {
        return OpenedDatabase {
            database: Some(database),
            warnings: Vec::new(),
            rebuild: None,
        };
    }
    let mut warnings = Vec::new();
    if let Some(quarantine) = &quarantine {
        tracing::warn!(
            path = %quarantine.display(),
            "the index was quarantined; rebuilding derived data"
        );
        warnings.push(StartupWarningView::quarantined(quarantine));
    } else {
        tracing::info!("building derived data on first database creation");
    }
    let mut report = match rebuild(database.connection_mut(), &rebuild_paths(paths), None) {
        Ok(report) => Some(report),
        Err(error) => {
            tracing::error!(%error, "derived-data rebuild failed");
            let warning_path = quarantine.as_deref().unwrap_or_else(|| paths.db_path());
            warnings.push(StartupWarningView::rebuild_failed(
                warning_path,
                &error.to_string(),
            ));
            None
        }
    };
    if let Some(report) = report.as_mut() {
        mark_derived_built(database.connection());
        // The rebuild writes skill rows only; MCP servers and rule files are discovered by the
        // shared native refresh, so a fresh or quarantined index must run it once or the MCP
        // screen would open with no "not in AgenTuna" rows and no banner.
        super::native_scan::refresh_native_findings(paths, database.connection());
        // The rebuild counted its rows before that refresh wrote the native findings, so the
        // report would claim `native_items=0` while the index holds every discovered row.
        // Report what the index actually holds once startup has finished building it.
        if let Ok(native_items) = at_db::native::count_native_items(database.connection()) {
            report.counts.native_items = native_items;
        }
    }
    OpenedDatabase {
        database: Some(database),
        warnings,
        rebuild: report,
    }
}

/// Filesystem roots the post-quarantine rebuild scans.
///
/// The project store is included only when the shell was started inside a project: a quarantined
/// index holds no project registry, so nothing else is guessed.
#[must_use]
pub fn rebuild_paths(paths: &AppPaths) -> RebuildPaths {
    let home = paths
        .home_dir()
        .map_or_else(|| paths.support_dir().to_path_buf(), Path::to_path_buf);
    let mut rebuild_paths =
        RebuildPaths::new(home, paths.user_store()).with_global_store(paths.global_store());
    if let Some(store) = paths.project_store() {
        let root = store
            .parent()
            .map_or_else(|| store.to_path_buf(), Path::to_path_buf);
        rebuild_paths = rebuild_paths.with_project_and_store(root, store.to_path_buf());
    }
    rebuild_paths
}

fn is_database_empty(connection: &rusqlite::Connection) -> bool {
    let marker_exists: bool = connection
        .query_row(
            "SELECT 1 FROM settings WHERE key = 'startup.derived_built'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if marker_exists {
        return false;
    }
    let item_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap_or(0);
    let native_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM native_items", [], |r| r.get(0))
        .unwrap_or(0);
    item_count == 0 && native_count == 0
}

fn mark_derived_built(connection: &rusqlite::Connection) {
    let _ = connection.execute(
        "INSERT INTO settings (key, value_json, updated_at) \
         VALUES ('startup.derived_built', 'true', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(key) DO UPDATE SET value_json = 'true'",
        [],
    );
}
