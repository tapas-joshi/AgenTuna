//! Shared native-findings refresh for the app's screens and row actions (TC-025, TM-020).
//!
//! Every command that can change what an agent folder holds — the post-quarantine/first-run
//! rebuild, the Rescan action, adopt, `migrate apply`, migrate undo and the MCP save and row
//! actions — calls this wrapper so `native_items` never describes an older disk than the one the
//! user just changed. `at_engine::native_index` owns the scan and the writes; this module only
//! turns its report into commands that keep working.
//!
//! A refresh failure is logged and the calling command still succeeds: the table is derived
//! data, and the next refresh (or a manual Rescan) rebuilds it. It never fails the user's write
//! after that write landed.

use at_engine::native_index::{NativeRefreshReport, refresh_native};
use at_store::paths::AppPaths;
use rusqlite::Connection;

/// Rescans native folders and replaces the derived `native_items` rows.
///
/// Returns `None` when the scan could not run or its rows could not be written; every problem is
/// logged first, with the failing path when the scanner named one.
pub(crate) fn refresh_native_findings(
    paths: &AppPaths,
    connection: &Connection,
) -> Option<NativeRefreshReport> {
    match refresh_native(connection, paths) {
        Ok(report) => {
            for warning in &report.warnings {
                tracing::warn!(
                    path = %warning.path.display(),
                    agent = %warning.agent,
                    scope = %warning.scope,
                    kind = %warning.kind,
                    "native discovery: {}",
                    warning.message
                );
            }
            Some(report)
        }
        Err(error) => {
            tracing::warn!(%error, "native findings were not refreshed");
            None
        }
    }
}
