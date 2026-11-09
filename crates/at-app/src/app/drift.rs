//! Drift detection from watcher batches (TC-012).
//!
//! The watcher is the same debounced, self-write-suppressed service the detection loop uses; this
//! module subscribes the engine's [`DriftService`] to its batches. Baselines come from the index
//! (`deployments`, falling back to the newest `item_versions` row), and every detected change is
//! persisted by the engine and emitted to the UI with its classification.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use at_db::Database;
use at_engine::drift::{
    DriftKind, DriftNotice, DriftService, drift_watch_config, targets_from_index,
};
use at_fs::{RecentWrites, WatchEvent, WatchService};
use at_store::paths::AppPaths;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use ts_rs::TS;

use super::detection::{allowed_link_roots, watch_roots_for};
fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Event emitted with every drift notice produced by one batch.
pub const EVENT_DRIFT_DETECTED: &str = "agentuna:drift-detected";

/// Event emitted whenever the watcher condition changes, so the Sync & drift screen can show
/// `Watching` / `Not watching: <reason>` without polling.
pub const EVENT_DRIFT_WATCHER_STATUS: &str = "agentuna:drift-watcher-status";

/// Background retry schedule for a watcher that failed to start. Each entry is the delay
/// before the next attempt; the last entry repeats.
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
];

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
    app: AppHandle,
    paused: Arc<AtomicBool>,
    retry: Arc<Mutex<RetryState>>,
}

#[derive(Debug, Default)]
struct RetryState {
    attempt: usize,
    last_attempt: Option<Instant>,
}

impl DriftWatcherControl {
    fn new(watcher: Arc<WatchService>, app: AppHandle) -> Self {
        Self {
            watcher,
            status: Arc::new(Mutex::new(DriftWatcherStatusView {
                status: "paused".to_owned(),
                path: None,
                error: None,
            })),
            app,
            paused: Arc::new(AtomicBool::new(false)),
            retry: Arc::new(Mutex::new(RetryState::default())),
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
        self.paused.store(paused, Ordering::SeqCst);
        if paused {
            self.watcher.stop().map_err(|error| error.to_string())?;
            self.set_status("paused", None, None);
            return Ok(self.status());
        }
        match self.watcher.start() {
            Ok(()) => {
                if let Ok(mut retry) = self.retry.lock() {
                    *retry = RetryState::default();
                }
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
        let next = DriftWatcherStatusView {
            status: status.to_owned(),
            path,
            error,
        };
        if let Ok(mut current) = self.status.lock() {
            if *current == next {
                return;
            }
            *current = next.clone();
        }
        if let Err(error) = self.app.emit(EVENT_DRIFT_WATCHER_STATUS, &next) {
            tracing::warn!(%error, "drift watcher status event emission failed");
        }
    }

    /// Syncs the visible condition with the watch service and retries a dead service in the
    /// background with backoff.
    ///
    /// The watch service installs its watches asynchronously and self-heals refused roots on
    /// its own recheck, so this only mirrors its state — and, when the service could not be
    /// started at all, brings it back.
    fn sync_status(&self) {
        if self.paused.load(Ordering::SeqCst) {
            return;
        }
        let service_status = self.watcher.status();
        let (name, error) = match &service_status {
            at_fs::WatchStatus::Starting => ("starting", None),
            at_fs::WatchStatus::Watching => ("active", None),
            at_fs::WatchStatus::Failed(message) => ("error", Some(message.clone())),
            at_fs::WatchStatus::Degraded(roots) => {
                let reason = roots
                    .first()
                    .map(|root| root.reason.to_string())
                    .unwrap_or_else(|| "Some roots are degraded.".to_owned());
                ("degraded", Some(reason))
            }
            other => {
                let message = other.to_string();
                ("error", Some(message))
            }
        };
        self.set_status(name, None, error);
        if matches!(service_status, at_fs::WatchStatus::Failed(_)) && !self.watcher.is_running() {
            self.retry_start();
        }
    }

    /// Attempts one background start, honouring [`RETRY_DELAYS`] between attempts.
    fn retry_start(&self) {
        let allowed = self
            .retry
            .lock()
            .map(|mut retry| {
                let index = retry.attempt.min(RETRY_DELAYS.len() - 1);
                let cooling_down = retry.last_attempt.is_some_and(|last| {
                    Instant::now().saturating_duration_since(last) < RETRY_DELAYS[index]
                });
                if cooling_down {
                    return false;
                }
                retry.last_attempt = Some(Instant::now());
                retry.attempt += 1;
                true
            })
            .unwrap_or(false);
        if !allowed {
            return;
        }
        match self.watcher.start() {
            Ok(()) => {
                if let Ok(mut retry) = self.retry.lock() {
                    *retry = RetryState::default();
                }
            }
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(%error, "drift watcher retry failed");
                self.set_status(
                    "error",
                    watcher_error_path(&error, self.watcher.roots()),
                    Some(message),
                );
            }
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
    let watcher = Arc::new(WatchService::new(drift_watch_config(
        roots,
        registry,
        allowed_link_roots(&paths),
    )));
    let control = DriftWatcherControl::new(Arc::clone(&watcher), app.clone());
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
    let cancellation = jobs.cancellation_token();
    tauri::async_runtime::spawn_blocking(move || {
        if jobs.window_is_paused(now_ms()) {
            jobs.wait_until_window_active();
        }
        reconcile_once(&app, &database);
        let mut deferred_while_hidden = false;
        loop {
            // App shutdown cancels the token; the watcher is dropped with the process, so the
            // worker must stop instead of idling forever on the event queue.
            if cancellation.is_cancelled() {
                return;
            }
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
                // Idle tick: mirror the watch service's own background progress (async watch
                // installation, self-healing, retries) into the visible condition.
                worker_control.sync_status();
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
