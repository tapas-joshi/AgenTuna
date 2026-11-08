//! Drift detection from watcher batches (TC-012).
//!
//! The watcher is the same debounced, self-write-suppressed service the detection loop uses; this
//! module subscribes the engine's [`DriftService`] to its batches. Baselines come from the index
//! (`deployments`, falling back to the newest `item_versions` row), and every detected change is
//! persisted by the engine and emitted to the UI with its classification.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use at_db::Database;
use at_engine::drift::{
    DriftKind, DriftNotice, DriftService, drift_watch_config, targets_from_index,
};
use at_fs::{RecentWrites, WatchEvent, WatchService};
use at_store::paths::AppPaths;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use super::detection::watch_roots_for;
fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Event emitted with every drift notice produced by one batch.
pub const EVENT_DRIFT_DETECTED: &str = "agentuna:drift-detected";

/// Drift event payload emitted to the UI after watcher processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct DriftNoticeView {
    /// Persisted drift-event row id.
    pub event_id: i64,
    /// Classification of the detected change.
    pub kind: String,
    /// Affected canonical item, when known.
    pub item_id: Option<String>,
    /// Affected agent.
    pub agent: String,
    /// Scope of the deployment.
    pub scope: String,
    /// Path that changed.
    pub path: String,
    /// Hash the deployment expected.
    pub expected_hash: Option<String>,
    /// Hash currently observed.
    pub actual_hash: Option<String>,
    /// Link target recorded for a replaced link.
    pub link_target: Option<String>,
    /// Number of changed files represented by the coalesced notice.
    pub changed_file_count: usize,
    /// Whether this watcher batch is eligible for a drift toast.
    pub toast_eligible: bool,
}

/// Current watcher condition shown in the Sync & drift header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct DriftWatcherStatusView {
    /// `active`, `paused` or `error`.
    pub status: String,
    /// Root path that failed, when the watcher is in an error state.
    pub path: Option<String>,
    /// Safe, user-facing watcher failure reason.
    pub error: Option<String>,
}

/// Managed watcher lifecycle for Sync & drift.
#[derive(Clone)]
pub struct DriftWatcherControl {
    watcher: Arc<WatchService>,
    status: Arc<Mutex<DriftWatcherStatusView>>,
}

impl DriftWatcherControl {
    fn new(watcher: Arc<WatchService>) -> Self {
        Self {
            watcher,
            status: Arc::new(Mutex::new(DriftWatcherStatusView {
                status: "paused".to_owned(),
                path: None,
                error: None,
            })),
        }
    }

    /// Returns the current watcher condition.
    #[must_use]
    pub fn status(&self) -> DriftWatcherStatusView {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| DriftWatcherStatusView {
                status: "error".to_owned(),
                path: None,
                error: Some("The watcher status is unavailable.".to_owned()),
            })
    }

    /// Pauses or resumes watching and returns the resulting condition.
    ///
    /// # Errors
    /// Returns the watcher failure when it cannot stop or install its native watches.
    pub fn set_paused(&self, paused: bool) -> Result<DriftWatcherStatusView, String> {
        if paused {
            self.watcher.stop().map_err(|error| error.to_string())?;
            self.set_status("paused", None, None);
            return Ok(self.status());
        }
        match self.watcher.start() {
            Ok(()) => {
                self.set_status("active", None, None);
                Ok(self.status())
            }
            Err(error) => {
                let path = watcher_error_path(&error, self.watcher.roots());
                let message = error.to_string();
                self.set_status("error", path, Some(message.clone()));
                Err(message)
            }
        }
    }

    fn set_status(&self, status: &str, path: Option<String>, error: Option<String>) {
        if let Ok(mut current) = self.status.lock() {
            *current = DriftWatcherStatusView {
                status: status.to_owned(),
                path,
                error,
            };
        }
    }
}

/// Starts drift processing over the agent and store roots.
///
/// Startup reconciliation is intentionally not toast-eligible: the Drift tab owns the persisted
/// list, while the toast stream is reserved for external changes observed during this session.
pub fn start(
    app: AppHandle,
    paths: AppPaths,
    database: Arc<Mutex<Database>>,
    registry: Arc<RecentWrites>,
    jobs: super::jobs::JobsControl,
) -> DriftWatcherControl {
    let roots = watch_roots_for(&paths);
    let watcher = Arc::new(WatchService::new(drift_watch_config(roots, registry)));
    let control = DriftWatcherControl::new(Arc::clone(&watcher));
    match watcher.start() {
        Ok(()) => control.set_status("active", None, None),
        Err(error) => {
            control.set_status(
                "error",
                watcher_error_path(&error, watcher.roots()),
                Some(error.to_string()),
            );
            tracing::warn!(%error, "drift watcher unavailable");
        }
    }
    let worker_control = control.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if jobs.window_is_paused(now_ms()) {
            jobs.wait_until_window_active();
        }
        reconcile_once(&app, &database);
        let mut deferred_while_hidden = false;
        loop {
            if jobs.window_is_paused(now_ms()) {
                deferred_while_hidden = true;
                jobs.wait_until_window_active();
                continue;
            }
            if deferred_while_hidden {
                // Changes received while hidden are reconciled as a non-toast startup pass.
                reconcile_once(&app, &database);
                deferred_while_hidden = false;
            }
            let Some(event) = watcher.next_event(Duration::from_secs(1)) else {
                continue;
            };
            if jobs.window_is_paused(now_ms()) {
                deferred_while_hidden = true;
                continue;
            }
            match &event {
                WatchEvent::Batch(_) => process_event(&app, &database, &event, true),
                WatchEvent::RescanRequired { .. } => {
                    process_event(&app, &database, &event, false);
                }
                WatchEvent::PathGone { path, .. } => {
                    process_event(&app, &database, &event, false);
                    worker_control.set_status(
                        "error",
                        Some(path.display().to_string()),
                        Some("A watched path is unavailable.".to_owned()),
                    );
                }
                _ => {}
            }
        }
    });
    control
}

fn watcher_error_path(error: &at_fs::WatchError, roots: &[at_fs::WatchRoot]) -> Option<String> {
    match error {
        at_fs::WatchError::WatchFailed { path, .. } => Some(path.display().to_string()),
        _ => roots.first().map(|root| root.path.display().to_string()),
    }
}

fn reconcile_once(app: &AppHandle, database: &Arc<Mutex<Database>>) {
    let Ok(database) = database.lock() else {
        tracing::warn!("database lock poisoned during drift reconciliation");
        return;
    };
    let targets = match targets_from_index(database.connection()) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%error, "drift baselines unavailable");
            return;
        }
    };
    let targets = match at_engine::drift::open_targets(database.connection(), targets) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%error, "open drift events unavailable");
            return;
        }
    };
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    match service.reconcile() {
        Ok(notices) => emit(app, &notices, false),
        Err(error) => tracing::warn!(%error, "drift reconciliation failed"),
    }
}

fn process_event(
    app: &AppHandle,
    database: &Arc<Mutex<Database>>,
    event: &WatchEvent,
    toast_eligible: bool,
) {
    let Ok(database) = database.lock() else {
        tracing::warn!("database lock poisoned during drift processing");
        return;
    };
    let targets = match targets_from_index(database.connection()) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%error, "drift baselines unavailable");
            return;
        }
    };
    let targets = match at_engine::drift::open_targets(database.connection(), targets) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%error, "open drift events unavailable");
            return;
        }
    };
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    match service.process(event) {
        Ok(notices) => emit(app, &notices, toast_eligible),
        Err(error) => tracing::warn!(%error, "drift processing failed"),
    }
}

fn emit(app: &AppHandle, notices: &[DriftNotice], toast_eligible: bool) {
    if notices.is_empty() {
        return;
    }
    let views = notices
        .iter()
        .map(|notice| notice_view(notice, toast_eligible))
        .collect::<Vec<_>>();
    if let Err(error) = app.emit(EVENT_DRIFT_DETECTED, &views) {
        tracing::warn!(%error, "drift event emission failed");
    }
}

fn notice_view(notice: &DriftNotice, toast_eligible: bool) -> DriftNoticeView {
    DriftNoticeView {
        event_id: notice.event_id,
        kind: kind_name(notice.kind).to_owned(),
        item_id: notice.item_id.map(|id| id.to_string()),
        agent: notice.agent_id.to_string(),
        scope: notice.scope.wire_name().to_owned(),
        path: notice.path.display().to_string(),
        expected_hash: notice.expected_hash.clone(),
        actual_hash: notice.actual_hash.clone(),
        link_target: notice
            .link_target
            .as_ref()
            .map(|path| path.display().to_string()),
        changed_file_count: notice.changed_file_count,
        toast_eligible,
    }
}

fn kind_name(kind: DriftKind) -> &'static str {
    match kind {
        DriftKind::Drifted => "drifted",
        DriftKind::LinkReplacedByFile => "link_replaced_by_file",
        DriftKind::LinkRemoved => "link_removed",
        DriftKind::FileDeleted => "file_deleted",
        DriftKind::StoreFileEditedWhileLinked => "store_file_edited_while_linked",
        DriftKind::Unreadable => "unreadable",
        _ => "unknown",
    }
}
