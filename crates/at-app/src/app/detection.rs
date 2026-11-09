//! Agent detection and filesystem-change loop.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Background retry schedule for a detection watcher that failed to start. Each entry is the
/// delay before the next attempt; the last entry repeats.
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
];

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_adapters::detect::{VersionProbe, detect_one, find_executable};
use at_db::Database;
use at_fs::{
    RecentWrites, WatchConfig, WatchError, WatchEvent, WatchRoot, WatchRootKind, WatchService,
};
use at_model::{AgentId, ItemKind, Scope};
use at_store::paths::AppPaths;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use ts_rs::TS;

/// Event emitted after a complete detection pass.
pub const EVENT_AGENTS_DETECTED: &str = "agentuna:agents-detected";

/// Event emitted after a self-write-suppressed filesystem watcher batch is indexed.
pub const EVENT_EXTERNAL_REFRESH: &str = "agentuna:external-refresh";

/// Event emitted for each non-fatal diagnostic the detection loop records.
pub const EVENT_DETECTION_DIAGNOSTIC: &str = "agentuna:detection-diagnostic";

/// Quiet period for combining watcher invalidations into one detection pass.
const DETECTION_DEBOUNCE: Duration = Duration::from_millis(500);

/// A non-fatal problem the detection loop recorded and worked around (TA-081).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Diagnostic {
    /// The platform refused to watch a root.
    ///
    /// That root is dropped from the watch set so every remaining root keeps delivering
    /// change events instead of one unwatchable path stopping detection entirely.
    WatchFailed {
        /// Root the platform refused.
        path: String,
        /// Platform error text.
        os_error: String,
    },
}

/// What one detection pass recorded for one agent (TA-081 AC1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DetectionResult {
    /// The agent's index rows and candidate paths were refreshed.
    Refreshed {
        /// Agent whose rows this pass wrote.
        agent: AgentId,
    },
    /// The agent is detected and stays visible, but the user marked it unmanaged: the pass
    /// records no deployment work for it and never rewrites its native configuration.
    SkippedUnmanaged {
        /// Agent the user opted out of managing.
        agent: AgentId,
    },
}

/// Aggregate result of one detection pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectionPass {
    /// Agents whose detection rows were written, including skipped unmanaged agents, because
    /// the Agents screen still shows them.
    pub detected: Vec<AgentId>,
    /// One result per detected agent, in registry order.
    pub results: Vec<DetectionResult>,
}

impl DetectionPass {
    /// Agents the pass detected but did not treat as managed.
    #[must_use]
    pub fn skipped_unmanaged(&self) -> Vec<String> {
        self.results
            .iter()
            .filter_map(|result| match result {
                DetectionResult::SkippedUnmanaged { agent } => Some(agent.to_string()),
                DetectionResult::Refreshed { .. } => None,
            })
            .collect()
    }
}

/// Pending invalidations are bounded and collected even while the scan worker is busy.
#[derive(Debug, Default)]
struct PendingDetection {
    deadline: Option<Instant>,
    paths: BTreeSet<String>,
    full_rescan: bool,
}

impl PendingDetection {
    fn enqueue(&mut self, now: Instant, event: ExternalRefreshEvent) {
        self.deadline = Some(now + DETECTION_DEBOUNCE);
        if event.full_rescan {
            self.paths.clear();
            self.full_rescan = true;
        } else if !self.full_rescan {
            for path in event.paths {
                if self.paths.len() >= at_fs::DEFAULT_MAX_BATCH_PATHS && !self.paths.contains(&path)
                {
                    self.paths.clear();
                    self.full_rescan = true;
                    break;
                }
                self.paths.insert(path);
            }
        }
    }

    fn take_ready(&mut self, now: Instant) -> Option<ExternalRefreshEvent> {
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        self.deadline = None;
        Some(ExternalRefreshEvent {
            paths: std::mem::take(&mut self.paths).into_iter().collect(),
            full_rescan: std::mem::take(&mut self.full_rescan),
        })
    }
}

type DetectionSchedule = (Mutex<PendingDetection>, Condvar);

fn wait_for_detection(schedule: &DetectionSchedule) -> ExternalRefreshEvent {
    let (pending, changed) = schedule;
    let mut pending = pending.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let now = Instant::now();
        if let Some(refresh) = pending.take_ready(now) {
            return refresh;
        }
        pending = match pending.deadline {
            Some(deadline) => {
                changed
                    .wait_timeout(pending, deadline.saturating_duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => changed
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
}

/// Watch startup seam so the tolerant-root policy is exercised without a platform failure.
pub trait WatchStarter {
    /// Builds and starts a service over exactly these roots.
    ///
    /// # Errors
    /// Returns the platform startup error, including [`WatchError::WatchFailed`] for a root the
    /// platform refuses.
    fn start(&mut self, roots: Vec<WatchRoot>) -> Result<WatchService, WatchError>;
}

/// The real platform watcher, sharing the app's self-write registry.
pub struct PlatformWatcher {
    registry: Arc<RecentWrites>,
    link_roots: Vec<std::path::PathBuf>,
}

impl PlatformWatcher {
    /// Creates a starter that shares one write-suppression registry with the write path.
    ///
    /// No link target is followed until [`Self::with_allowed_link_roots`] names the roots a
    /// linked skill may resolve inside; production always sets them from the injected paths.
    #[must_use]
    pub fn new(registry: Arc<RecentWrites>) -> Self {
        Self {
            registry,
            link_roots: Vec::new(),
        }
    }

    /// Restricts link-target following to these roots.
    #[must_use]
    pub fn with_allowed_link_roots(
        mut self,
        link_roots: impl IntoIterator<Item = std::path::PathBuf>,
    ) -> Self {
        self.link_roots = link_roots.into_iter().collect();
        self
    }
}

impl WatchStarter for PlatformWatcher {
    fn start(&mut self, roots: Vec<WatchRoot>) -> Result<WatchService, WatchError> {
        let service = WatchService::new(
            WatchConfig::new(roots)
                .with_allowed_link_roots(self.link_roots.iter().cloned())
                .with_registry(Arc::clone(&self.registry)),
        );
        service.start()?;
        Ok(service)
    }
}

/// Roots a linked skill target may resolve inside for this application layout.
///
/// The injected home and the project root cover agent and library folders. The stores are
/// included as well, so a link into the User or Global store (which an override can place
/// outside the home) keeps being watched instead of degrading.
pub(crate) fn allowed_link_roots(paths: &AppPaths) -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(|store| store.parent()) {
        roots.push(project.to_path_buf());
    }
    roots.push(paths.user_store().to_path_buf());
    roots.push(paths.global_store().to_path_buf());
    roots.sort();
    roots.dedup();
    roots
}

/// Starts a watch service over every root the platform accepts (TA-081 AC4).
///
/// A root the platform refuses is dropped and reported as [`Diagnostic::WatchFailed`] so one
/// unwatchable path cannot stop detection for every other agent. The failure is returned
/// unchanged when nothing watchable remains.
///
/// # Errors
/// Returns the startup error when the failure is not per-root, or when dropping refused roots
/// leaves nothing to watch.
pub fn start_retaining_watchable_roots(
    mut roots: Vec<WatchRoot>,
    starter: &mut dyn WatchStarter,
) -> Result<(WatchService, Vec<Diagnostic>), WatchError> {
    debug_assert!(
        !roots.is_empty(),
        "the detection loop needs at least one watch root"
    );
    let mut diagnostics = Vec::new();
    loop {
        match starter.start(roots.clone()) {
            Ok(service) => return Ok((service, diagnostics)),
            Err(WatchError::WatchFailed { path, source }) => {
                diagnostics.push(Diagnostic::WatchFailed {
                    path: path.to_string_lossy().into_owned(),
                    os_error: source.to_string(),
                });
                let before = roots.len();
                // The refused path is a root or one of its ancestors: file roots are watched
                // through their parent, and a missing directory through its nearest existing
                // ancestor, so match the refused path against each root's prefix too.
                roots.retain(|root| !root.path.starts_with(&path));
                if roots.len() == before || roots.is_empty() {
                    return Err(WatchError::WatchFailed { path, source });
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Typed filesystem invalidation delivered to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct ExternalRefreshEvent {
    /// Changed paths in the observed batch. Empty when a complete rescan is required.
    pub paths: Vec<String>,
    /// Whether the event requires reloading the complete current screen snapshot.
    pub full_rescan: bool,
}

/// Maps one watcher event to the typed invalidation the frontend reloads on.
///
/// Returns `None` for events that need no view refresh; `full_rescan` is set when the batch is
/// too coarse to name paths (`RescanRequired`) or a watched path disappeared.
#[must_use]
pub fn external_refresh_event(event: &WatchEvent) -> Option<ExternalRefreshEvent> {
    let (paths, full_rescan) = match event {
        WatchEvent::Batch(batch) => (
            batch
                .paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            false,
        ),
        WatchEvent::RescanRequired { .. } => (Vec::new(), true),
        WatchEvent::PathGone { path, .. } => (vec![path.to_string_lossy().into_owned()], true),
        _ => return None,
    };
    Some(ExternalRefreshEvent { paths, full_rescan })
}

#[derive(Debug, Clone, Serialize)]
struct DetectionEvent {
    agents: usize,
    skipped_unmanaged: Vec<String>,
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Starts an initial agent scan and keeps the index current as native files change.
///
/// When the platform refuses the initial watch, the start is retried in the background with
/// backoff; the returned error reports the first failure so the caller can log it once.
///
/// # Errors
/// Returns the startup error of the first attempt.
pub fn start(
    app: AppHandle,
    paths: AppPaths,
    database: Arc<Mutex<Database>>,
    registry: Arc<RecentWrites>,
    jobs: super::jobs::JobsControl,
) -> Result<(), WatchError> {
    let adapters = Arc::new(AgentAdapterRegistry::register());
    let environment = at_engine::refresh::agent_environment(&paths);
    let roots = watch_roots(&adapters, &environment, &paths);
    let link_roots = allowed_link_roots(&paths);
    let mut starter =
        PlatformWatcher::new(Arc::clone(&registry)).with_allowed_link_roots(link_roots.clone());
    match start_retaining_watchable_roots(roots.clone(), &mut starter) {
        Ok((watcher, diagnostics)) => {
            emit_diagnostics(&app, &diagnostics);
            spawn_detection_consumers(&app, adapters, environment, database, jobs, watcher);
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "agent filesystem detection watcher unavailable; retrying in the background"
            );
            spawn_detection_retry(
                app,
                environment,
                database,
                adapters,
                roots,
                link_roots,
                registry,
                jobs,
            );
            Err(error)
        }
    }
}

/// Emits per-root watch refusals as typed diagnostics.
fn emit_diagnostics(app: &AppHandle, diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        tracing::warn!(
            ?diagnostic,
            "watch root refused; the remaining roots stay live"
        );
        if let Err(error) = app.emit(EVENT_DETECTION_DIAGNOSTIC, diagnostic) {
            tracing::warn!(%error, "detection diagnostic event emission failed");
        }
    }
}

/// Retries the detection watcher start in the background with backoff until it succeeds.
///
/// Each attempt rebuilds the platform watcher; a success hands the service to the same
/// consumers a normal start would have used, so detection becomes live without user action.
#[allow(clippy::too_many_arguments)]
fn spawn_detection_retry(
    app: AppHandle,
    environment: AgentEnvironment,
    database: Arc<Mutex<Database>>,
    adapters: Arc<AgentAdapterRegistry>,
    roots: Vec<WatchRoot>,
    link_roots: Vec<std::path::PathBuf>,
    registry: Arc<RecentWrites>,
    jobs: super::jobs::JobsControl,
) {
    // The app's shutdown cancels this token; the loop must stop retrying even while the
    // platform watcher keeps refusing, instead of spinning past the process lifetime.
    let cancellation = jobs.cancellation_token();
    tauri::async_runtime::spawn_blocking(move || {
        retry_with_backoff(
            &RETRY_DELAYS,
            || cancellation.is_cancelled(),
            |index| {
                let mut starter = PlatformWatcher::new(Arc::clone(&registry))
                    .with_allowed_link_roots(link_roots.iter().cloned());
                match start_retaining_watchable_roots(roots.clone(), &mut starter) {
                    Ok((watcher, diagnostics)) => {
                        tracing::info!("agent filesystem detection watcher started on retry");
                        emit_diagnostics(&app, &diagnostics);
                        spawn_detection_consumers(
                            &app,
                            Arc::clone(&adapters),
                            environment.clone(),
                            Arc::clone(&database),
                            jobs.clone(),
                            watcher,
                        );
                        true
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            "agent filesystem detection watcher retry failed (attempt {})",
                            index + 1
                        );
                        false
                    }
                }
            },
        );
    });
}

/// How often a cancellable backoff wait re-checks the stop flag.
const RETRY_STOP_POLL: Duration = Duration::from_millis(25);

/// Sleeps for `delay` unless `stopped` turns true first; returns whether it stopped.
fn wait_unless_stopped(delay: Duration, stopped: &impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if stopped() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep((deadline - now).min(RETRY_STOP_POLL));
    }
}

/// Calls `attempt` until it returns `true` or `stopped` turns true, waiting `delays` between
/// attempts; the last delay repeats. The first attempt runs after `delays[0]`, because the
/// caller already tried once. A stop is observed within [`RETRY_STOP_POLL`] of the signal, so a
/// dropped service or a quitting app never leaves the thread sleeping through a backoff step.
fn retry_with_backoff(
    delays: &[Duration],
    stopped: impl Fn() -> bool,
    mut attempt: impl FnMut(usize) -> bool,
) {
    let mut index = 0usize;
    loop {
        if wait_unless_stopped(delays[index.min(delays.len() - 1)], &stopped) {
            return;
        }
        if stopped() {
            return;
        }
        if attempt(index) {
            return;
        }
        index += 1;
    }
}

/// The scan worker and the watcher event consumer, shared by the initial start and retries.
fn spawn_detection_consumers(
    app: &AppHandle,
    adapters: Arc<AgentAdapterRegistry>,
    environment: AgentEnvironment,
    database: Arc<Mutex<Database>>,
    jobs: super::jobs::JobsControl,
    watcher: WatchService,
) {
    let schedule: Arc<DetectionSchedule> =
        Arc::new((Mutex::new(PendingDetection::default()), Condvar::new()));
    let scan_schedule = Arc::clone(&schedule);
    let scan_jobs = jobs.clone();
    let scan_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Keep the initial pass on the same worker so watcher requests cannot overlap it.
        if scan_jobs.window_is_paused(now_ms()) {
            scan_jobs.wait_until_window_active();
        }
        detect_and_persist(&scan_app, &adapters, &environment, &database);
        let mut deferred_while_hidden = false;
        loop {
            let mut refresh = wait_for_detection(&scan_schedule);
            if scan_jobs.window_is_paused(now_ms()) {
                deferred_while_hidden = true;
                scan_jobs.wait_until_window_active();
            }
            if deferred_while_hidden {
                refresh = ExternalRefreshEvent {
                    paths: Vec::new(),
                    full_rescan: true,
                };
                deferred_while_hidden = false;
            }
            detect_and_persist(&scan_app, &adapters, &environment, &database);
            if let Err(error) = scan_app.emit(EVENT_EXTERNAL_REFRESH, refresh) {
                tracing::warn!(%error, "external refresh event emission failed");
            }
        }
    });

    tauri::async_runtime::spawn_blocking(move || {
        loop {
            let Some(event) = watcher.next_event(Duration::from_secs(1)) else {
                continue;
            };
            if let Some(refresh) = external_refresh_event(&event) {
                let (pending, changed) = &*schedule;
                let mut pending = pending.lock().unwrap_or_else(PoisonError::into_inner);
                pending.enqueue(Instant::now(), refresh);
                changed.notify_one();
            }
        }
    });
}

/// Watch roots for the detected agents and AgenTuna stores.
///
/// Shared with the drift watcher (TC-012) so both consumers watch exactly the same paths.
pub(crate) fn watch_roots_for(paths: &AppPaths) -> Vec<WatchRoot> {
    let adapters = AgentAdapterRegistry::register();
    let environment = at_engine::refresh::agent_environment(paths);
    watch_roots(&adapters, &environment, paths)
}

fn watch_roots(
    adapters: &AgentAdapterRegistry,
    environment: &AgentEnvironment,
    paths: &AppPaths,
) -> Vec<WatchRoot> {
    let mut roots = vec![
        WatchRoot::dir(paths.user_store(), WatchRootKind::UserStore),
        WatchRoot::dir(paths.global_store(), WatchRootKind::GlobalStore),
    ];
    if let Some(project) = paths.project_store() {
        roots.push(WatchRoot::dir(project, WatchRootKind::ProjectStore));
    }
    if let Ok(adapters) = adapters.adapters() {
        for adapter in adapters {
            for kind in [ItemKind::Skill, ItemKind::McpServer, ItemKind::Rule] {
                for scope in [Scope::User, Scope::Global, Scope::Project, Scope::Local] {
                    for candidate in adapter.paths(kind, scope, environment) {
                        let root_kind = match candidate.kind.as_str() {
                            "skills_dir" => WatchRootKind::SkillsDir,
                            "mcp_file" => WatchRootKind::McpFile,
                            "rules_file" => WatchRootKind::RulesFile,
                            "settings_file" => WatchRootKind::Other,
                            _ => continue,
                        };
                        let root = if root_kind.is_directory() {
                            WatchRoot::dir(candidate.path, root_kind)
                        } else {
                            WatchRoot::file(candidate.path, root_kind)
                        };
                        roots.push(root);
                    }
                }
            }
        }
    }
    roots.sort_by(|left, right| left.path.cmp(&right.path));
    roots.dedup_by(|left, right| left.path == right.path && left.kind == right.kind);
    roots
}

/// Runs one detection pass over every registered adapter and records the results.
///
/// The caller owns the environment and database handle, so an on-demand rescan from a command
/// uses the same persistence path as the background watcher.
///
/// # Errors
/// Returns an error when the adapter registry or the database lock is unavailable.
pub fn rescan(
    adapters: &Arc<AgentAdapterRegistry>,
    environment: &AgentEnvironment,
    database: &Arc<Mutex<Database>>,
) -> Result<DetectionPass, &'static str> {
    let adapter_list = adapters
        .adapters()
        .map_err(|_| "agent adapter registry unavailable")?;
    let results = tauri::async_runtime::block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        for adapter in adapter_list {
            let id = adapter.id();
            let registry = Arc::clone(adapters);
            let environment = environment.clone();
            tasks.spawn(async move {
                let probe = version_probe_for(&id, &environment);
                let result = detect_one(&registry, &id, &environment, probe.as_ref()).await;
                (id, result)
            });
        }
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(result) => results.push(result),
                Err(error) => tracing::warn!(%error, "agent detection task failed"),
            }
        }
        results
    });
    let database = database
        .lock()
        .map_err(|_| "database lock poisoned during agent detection")?;
    Ok(persist_results(database.connection(), results))
}

fn detect_and_persist(
    app: &AppHandle,
    adapters: &Arc<AgentAdapterRegistry>,
    environment: &AgentEnvironment,
    database: &Arc<Mutex<Database>>,
) {
    let Ok(adapter_list) = adapters.adapters() else {
        tracing::warn!("agent adapter registry unavailable during detection");
        return;
    };
    let results = tauri::async_runtime::block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        for adapter in adapter_list {
            let id = adapter.id();
            let registry = Arc::clone(adapters);
            let environment = environment.clone();
            tasks.spawn(async move {
                let probe = version_probe_for(&id, &environment);
                let result = detect_one(&registry, &id, &environment, probe.as_ref()).await;
                (id, result)
            });
        }
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(result) => results.push(result),
                Err(error) => tracing::warn!(%error, "agent detection task failed"),
            }
        }
        results
    });
    let Ok(database) = database.lock() else {
        tracing::warn!("database lock poisoned during agent detection");
        return;
    };
    let detected = persist_results(database.connection(), results);
    if let Some(state) = app.try_state::<super::state::AppState>() {
        state.invalidate_scope_cache();
    }
    if let Err(error) = app.emit(
        EVENT_AGENTS_DETECTED,
        DetectionEvent {
            agents: detected.detected.len(),
            skipped_unmanaged: detected.skipped_unmanaged(),
        },
    ) {
        tracing::warn!(%error, "agent detection event emission failed");
    }
}

/// Persists one completed detection pass and records what it did per agent.
///
/// Every detected agent keeps a visible row so the Agents screen stays complete, but an agent
/// the user marked unmanaged is recorded as [`DetectionResult::SkippedUnmanaged`] instead of
/// [`DetectionResult::Refreshed`]: nothing is rescanned into deployments for it and its native
/// configuration is never rewritten.
fn persist_results(
    connection: &rusqlite::Connection,
    results: Vec<(
        at_model::AgentId,
        Result<at_adapters::detect::AgentDetectionResult, at_adapters::adapter::AgentError>,
    )>,
) -> DetectionPass {
    let unmanaged: std::collections::BTreeSet<AgentId> =
        at_db::agents::list_agents_on_mac(connection)
            .map(|rows| {
                rows.into_iter()
                    .filter(|row| !row.managed)
                    .map(|row| row.id)
                    .collect()
            })
            .unwrap_or_default();
    let mut agents = Vec::with_capacity(results.len());
    for (id, result) in results {
        match result {
            Ok(result) => agents.push(at_engine::refresh::DetectedAgent::from_detection(
                &id,
                &result.detection,
            )),
            Err(error) => tracing::warn!(%error, agent = %id, "agent detection failed"),
        }
    }
    let mut pass = DetectionPass {
        detected: Vec::with_capacity(agents.len()),
        results: Vec::with_capacity(agents.len()),
    };
    for agent in &agents {
        pass.detected.push(agent.id.clone());
        pass.results.push(if unmanaged.contains(&agent.id) {
            DetectionResult::SkippedUnmanaged {
                agent: agent.id.clone(),
            }
        } else {
            DetectionResult::Refreshed {
                agent: agent.id.clone(),
            }
        });
    }
    if let Err(error) = at_engine::refresh::persist_detected_agents(connection, &agents) {
        tracing::warn!(%error, "agent detection persistence failed");
        pass.detected.clear();
        pass.results.clear();
    }
    pass
}

fn version_probe_for(
    agent: &at_model::AgentId,
    environment: &AgentEnvironment,
) -> Option<VersionProbe> {
    let executable = match agent {
        at_model::AgentId::ClaudeCode => "claude",
        at_model::AgentId::Codex => "codex",
        at_model::AgentId::Omp => "omp",
        at_model::AgentId::Cursor => "cursor",
        at_model::AgentId::Custom(_) => return None,
    };
    let executable = find_executable(
        environment.variables.get("PATH").map(String::as_str),
        executable,
    )?;
    Some(VersionProbe {
        executable,
        args: vec![String::from("--version")],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh(paths: impl IntoIterator<Item = String>) -> ExternalRefreshEvent {
        ExternalRefreshEvent {
            paths: paths.into_iter().collect(),
            full_rescan: false,
        }
    }

    #[test]
    fn retry_with_backoff_repeats_until_the_attempt_succeeds() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&attempts);
        retry_with_backoff(
            &[Duration::from_millis(1), Duration::from_millis(1)],
            || false,
            |index| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                index >= 2
            },
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "two failures then one success"
        );
    }

    /// A starter whose watch is always refused, the condition the retry loop exists for.
    struct AlwaysRefusing;

    impl WatchStarter for AlwaysRefusing {
        fn start(&mut self, roots: Vec<WatchRoot>) -> Result<WatchService, WatchError> {
            let path = roots
                .first()
                .map(|root| root.path.clone())
                .unwrap_or_default();
            Err(WatchError::WatchFailed {
                path,
                source: std::io::Error::other("refused by the test starter").into(),
            })
        }
    }

    #[test]
    fn retry_with_backoff_stops_within_one_backoff_step_when_cancelled() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&attempts);
        // The stop arrives while the thread is inside the long second backoff: the old loop
        // slept through the whole delay and every later attempt.
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            signal.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let began = Instant::now();
        retry_with_backoff(
            &[Duration::from_millis(10), Duration::from_secs(30)],
            || stop.load(std::sync::atomic::Ordering::SeqCst),
            |_index| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut starter = AlwaysRefusing;
                start_retaining_watchable_roots(
                    vec![WatchRoot::dir(
                        std::path::PathBuf::from("/nonexistent/agentuna-test-root"),
                        WatchRootKind::SkillsDir,
                    )],
                    &mut starter,
                )
                .is_ok()
            },
        );
        let elapsed = began.elapsed();
        stopper.join().expect("join the stop signal");
        assert!(
            elapsed < Duration::from_secs(5),
            "the loop must exit within one poll step, took {elapsed:?}"
        );
        let attempted = attempts.load(std::sync::atomic::Ordering::SeqCst);
        assert!(attempted >= 1, "the failing starter ran before the stop");
        assert_eq!(attempted, 1, "no attempt runs once the stop flag is set");
    }

    #[test]
    fn split_watcher_batches_within_the_debounce_produce_one_complete_refresh() {
        let start = Instant::now();
        let mut pending = PendingDetection::default();
        let paths: Vec<String> = (0..20).map(|index| format!("rule-{index:02}.md")).collect();

        pending.enqueue(start, refresh(paths[..18].iter().cloned()));
        assert!(
            pending
                .take_ready(start + Duration::from_millis(299))
                .is_none()
        );
        pending.enqueue(
            start + Duration::from_millis(300),
            refresh(paths[18..].iter().cloned()),
        );
        assert!(
            pending
                .take_ready(start + Duration::from_millis(799))
                .is_none()
        );

        let event = pending
            .take_ready(start + Duration::from_millis(800))
            .expect("one scan is ready after the trailing quiet window");
        assert_eq!(event.paths, paths);
        assert!(!event.full_rescan);
        assert!(pending.take_ready(start + Duration::from_secs(2)).is_none());
    }

    #[test]
    fn events_received_during_a_scan_need_only_one_follow_up_scan() {
        let start = Instant::now();
        let mut pending = PendingDetection::default();
        pending.enqueue(start, refresh([String::from("first.md")]));
        assert!(
            pending
                .take_ready(start + Duration::from_millis(500))
                .is_some()
        );

        // The scan worker has taken the first request and has not finished it yet.
        pending.enqueue(
            start + Duration::from_millis(600),
            refresh([String::from("second.md")]),
        );
        pending.enqueue(
            start + Duration::from_millis(700),
            refresh([String::from("third.md"), String::from("second.md")]),
        );
        let follow_up = pending
            .take_ready(start + Duration::from_secs(2))
            .expect("one follow-up covers every event received during the scan");
        assert_eq!(follow_up.paths, ["second.md", "third.md"]);
        assert!(!follow_up.full_rescan);
        assert!(pending.take_ready(start + Duration::from_secs(3)).is_none());
    }

    #[test]
    fn a_running_scan_does_not_block_watcher_requests() {
        let schedule: Arc<DetectionSchedule> =
            Arc::new((Mutex::new(PendingDetection::default()), Condvar::new()));
        let worker_schedule = Arc::clone(&schedule);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (finish_tx, finish_rx) = std::sync::mpsc::sync_channel(1);
        let (follow_tx, follow_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let first = wait_for_detection(&worker_schedule);
            started_tx.send(first).expect("report the started scan");
            finish_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("finish the controlled scan");
            let follow_up = wait_for_detection(&worker_schedule);
            follow_tx.send(follow_up).expect("report the follow-up");
        });

        let (pending, changed) = &*schedule;
        {
            let mut pending = pending.lock().expect("schedule lock");
            pending.enqueue(
                Instant::now() - Duration::from_secs(2),
                refresh([String::from("first.md")]),
            );
            changed.notify_one();
        }
        let first = started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the scan starts");
        assert_eq!(first.paths, ["first.md"]);

        let paths: Vec<String> = (0..20).map(|index| format!("rule-{index:02}.md")).collect();
        {
            let mut pending = pending.lock().expect("watcher can enqueue while scan runs");
            let first_batch = Instant::now() - Duration::from_secs(2);
            pending.enqueue(first_batch, refresh(paths[..18].iter().cloned()));
            pending.enqueue(
                first_batch + Duration::from_millis(300),
                refresh(paths[18..].iter().cloned()),
            );
            changed.notify_one();
        }
        finish_tx.send(()).expect("release the controlled scan");
        let follow_up = follow_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("one follow-up scan starts");
        worker.join().expect("join the scan worker");
        assert_eq!(follow_up.paths, paths);
        assert!(!follow_up.full_rescan);
        assert!(
            pending
                .lock()
                .expect("schedule lock")
                .take_ready(Instant::now())
                .is_none(),
            "the burst must not leave another scan queued"
        );
    }

    #[test]
    fn an_oversized_pending_refresh_becomes_a_bounded_full_rescan() {
        let start = Instant::now();
        let mut pending = PendingDetection::default();
        pending.enqueue(
            start,
            refresh((0..=at_fs::DEFAULT_MAX_BATCH_PATHS).map(|index| format!("rule-{index}.md"))),
        );
        let event = pending
            .take_ready(start + Duration::from_millis(500))
            .expect("the full rescan is ready");
        assert!(event.full_rescan);
        assert!(event.paths.is_empty());
    }
}
