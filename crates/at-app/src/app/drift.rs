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

use super::detection::watch_roots_for;

/// Event emitted with every drift notice produced by one batch.
pub const EVENT_DRIFT_DETECTED: &str = "agentuna:drift-detected";

/// One drift notice as the UI receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

/// Starts drift processing over the agent and store roots.
///
/// # Errors
/// Returns the watcher failure when the platform refuses the roots; drift processing is skipped
/// rather than blocking startup.
pub fn start(
    app: AppHandle,
    paths: AppPaths,
    database: Arc<Mutex<Database>>,
    registry: Arc<RecentWrites>,
) -> Result<(), at_fs::WatchError> {
    let roots = watch_roots_for(&paths);
    let watcher = Arc::new(WatchService::new(drift_watch_config(roots, registry)));
    watcher.start()?;
    tauri::async_runtime::spawn_blocking(move || {
        // Startup reconciliation uses the same baseline comparison as ordinary batches, so a
        // change made while AgenTuna was closed is reported once.
        reconcile_once(&app, &database);
        loop {
            let Some(event) = watcher.next_event(Duration::from_secs(1)) else {
                continue;
            };
            match event {
                WatchEvent::Batch(_) | WatchEvent::RescanRequired { .. } => {
                    process_event(&app, &database, &event);
                }
                // WatchEvent can grow; an event this module does not know needs no drift pass.
                _ => {}
            }
        }
    });
    Ok(())
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
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    match service.reconcile() {
        Ok(notices) => emit(app, &notices),
        Err(error) => tracing::warn!(%error, "drift reconciliation failed"),
    }
}

fn process_event(app: &AppHandle, database: &Arc<Mutex<Database>>, event: &WatchEvent) {
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
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    match service.process(event) {
        Ok(notices) => emit(app, &notices),
        Err(error) => tracing::warn!(%error, "drift processing failed"),
    }
}

fn emit(app: &AppHandle, notices: &[DriftNotice]) {
    if notices.is_empty() {
        return;
    }
    let views = notices.iter().map(notice_view).collect::<Vec<_>>();
    if let Err(error) = app.emit(EVENT_DRIFT_DETECTED, &views) {
        tracing::warn!(%error, "drift event emission failed");
    }
}

fn notice_view(notice: &DriftNotice) -> DriftNoticeView {
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
