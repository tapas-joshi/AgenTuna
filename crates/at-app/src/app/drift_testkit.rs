//! Drift assertions shared by at-app command tests.
//!
//! A command that writes an agent-native path must refresh the deployment baseline of every
//! row that targets it; otherwise the next reconcile reports AgenTuna's own write as an
//! external edit. These helpers drive the same reconcile the app watcher uses, over a
//! throwaway sandbox database.

use std::path::Path;

use at_engine::drift::{DriftNotice, DriftService, targets_from_index};

use super::state::AppState;

/// Reconciles every indexed deployment and returns the notices it recorded.
#[cfg(test)]
pub(crate) fn reconcile(state: &AppState) -> Vec<DriftNotice> {
    let database = state.database().expect("database");
    let connection = database.connection();
    let targets = targets_from_index(connection).expect("deployment targets");
    DriftService {
        database: &database,
        targets: &targets,
    }
    .reconcile()
    .expect("drift reconcile")
}

/// Asserts that a command's own write left no open drift event.
pub(crate) fn assert_no_drift(state: &AppState, context: &str) {
    {
        let database = state.database().expect("database");
        let open =
            at_db::drift::open_drift_events(database.connection(), None).expect("open events");
        assert!(
            open.is_empty(),
            "{context}: open drift events before reconcile: {open:?}"
        );
    }
    let notices = reconcile(state);
    assert!(
        notices.is_empty(),
        "{context}: the command's own write was reported as drift: {notices:?}"
    );
}

/// Writes bytes outside AgenTuna and asserts the next reconcile detects the edit at `path`.
pub(crate) fn assert_external_edit_detected(state: &AppState, path: &Path, bytes: &[u8]) {
    assert_external_edit_detected_at(state, path, path, bytes);
}

/// Writes `bytes` at `write_path` and asserts a drift notice for `watched_path`.
///
/// Skill deployments name the skill folder as their target while the edited file lives inside
/// it, so the written path and the watched path can differ.
pub(crate) fn assert_external_edit_detected_at(
    state: &AppState,
    write_path: &Path,
    watched_path: &Path,
    bytes: &[u8],
) {
    std::fs::write(write_path, bytes).expect("external edit");
    let notices = reconcile(state);
    assert!(
        notices.iter().any(|notice| notice.path == watched_path),
        "the external edit at {} was not detected for {}: {notices:?}",
        write_path.display(),
        watched_path.display()
    );
}
