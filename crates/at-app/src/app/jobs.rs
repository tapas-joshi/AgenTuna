//! Deterministic MCP health scheduling primitives used by app jobs.
//!
//! Time, jitter, and execution are supplied by the caller so scheduling policy can be exercised
//! without wall-clock sleeps, network access, or child processes.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::app::network::NetworkState;
use at_db::Database;
use at_db::health::{HealthStatus, HealthTransport as DbTransport, HealthTrigger, NewHealthCheck};
use at_db::settings::{BackgroundHealth, HealthInterval};
use at_model::ItemKind;
use at_model::Scope;
use at_model::network::{NetworkAction, NetworkTrigger};
use at_store::{
    layout::StoreLayout, manifest::StoreManifest, mcp_store::McpStore, paths::AppPaths,
};
use std::sync::Arc;
use std::time::SystemTime;

use at_model::{McpServer, McpServerId};
use at_secrets::{KeychainId, KeychainStore, SecretStore};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use ts_rs::TS;

/// Server descriptor supplied by the current resolved MCP configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct HealthTarget {
    /// Stable identity used to prevent overlapping work.
    pub id: String,
    /// Display name persisted to health history.
    pub name: String,
    /// AgenTuna item id, if this is a managed server.
    pub item_id: Option<String>,
    /// Scope where this server is configured.
    pub scope: Scope,
    /// Project id for project/local scope.
    pub project_id: Option<String>,
    /// Agent association, when applicable.
    pub agent_id: Option<at_model::AgentId>,
    /// Transport type.
    pub transport: HealthTransport,
    /// HTTP host passed to the policy gate; unused for stdio.
    pub host: Option<String>,
    /// Whether at least one manual test has succeeded or failed.
    pub manually_tested: bool,
    /// Exact managed server definition used by the real runner.
    pub server: McpServer,
}

/// Transports eligible for scheduled checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HealthTransport {
    /// Local stdio MCP server.
    Stdio,
    /// Remote HTTP MCP server.
    Http,
}

/// Injected monotonic and persistence time source.
pub trait Clock: Send + Sync {
    /// Current time in milliseconds since the scheduler epoch.
    fn now_ms(&self) -> i64;
}

/// Injected deterministic jitter provider.
pub trait Jitter: Send + Sync {
    /// Return a delay in [0, `max_ms`]. Implementations must honor the bound.
    fn delay_ms(&self, max_ms: u64) -> u64;
}
/// System wall clock used for production timestamps.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_millis().min(i64::MAX as u128) as i64
            })
    }
}

/// Process-local pseudorandom jitter source; deterministic sources belong in tests.
pub struct SystemJitter(std::sync::atomic::AtomicU64);

impl Default for SystemJitter {
    fn default() -> Self {
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(1, |duration| duration.as_nanos() as u64)
            .max(1);
        Self(std::sync::atomic::AtomicU64::new(seed))
    }
}

impl Jitter for SystemJitter {
    fn delay_ms(&self, max_ms: u64) -> u64 {
        if max_ms == 0 {
            return 0;
        }
        let value = self
            .0
            .fetch_add(0x9E37_79B9_7F4A_7C15, std::sync::atomic::Ordering::Relaxed);
        let mut mixed = value;
        mixed ^= mixed >> 30;
        mixed = mixed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed ^= mixed >> 27;
        mixed = mixed.wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        mixed % (max_ms + 1)
    }
}

/// Fakeable asynchronous MCP check executor.
#[allow(async_fn_in_trait)]
pub trait HealthRunner: Send + Sync {
    /// Run a single check; cancellation must stop child/network work promptly.
    async fn check(
        &self,
        target: &HealthTarget,
        network: &NetworkState,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> CheckResult;
}
/// Production runner for scheduled stdio and HTTP MCP health checks.
pub struct McpHealthRunner {
    secrets: Arc<dyn SecretStore>,
}

impl Default for McpHealthRunner {
    fn default() -> Self {
        let secrets: Arc<dyn SecretStore> = Arc::new(KeychainStore::new());
        Self { secrets }
    }
}
impl McpHealthRunner {
    fn resolve_http_headers(&self, server: &mut McpServer) -> Result<(), ()> {
        let at_model::McpTransport::Http { headers, .. } = &mut server.transport else {
            return Ok(());
        };
        for value in headers.values_mut() {
            let at_model::EnvValue::Secret(secret_ref) = value else {
                continue;
            };
            let key = KeychainId::new(secret_ref.keychain_account.clone()).map_err(|_| ())?;
            let secret = self.secrets.read(&key).map_err(|_| ())?;
            *value = at_model::EnvValue::Literal(secret.expose_secret().to_owned());
        }
        Ok(())
    }
}

impl HealthRunner for McpHealthRunner {
    async fn check(
        &self,
        target: &HealthTarget,
        network: &NetworkState,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> CheckResult {
        let started = tokio::time::Instant::now();
        match &target.server.transport {
            at_model::McpTransport::Stdio { .. } => {
                let background = network.policy().background_health();
                let allowed = match background {
                    at_model::BackgroundHealth::All => true,
                    at_model::BackgroundHealth::Local => target.manually_tested,
                    at_model::BackgroundHealth::Off => false,
                    _ => false,
                };
                if !allowed {
                    return failed_check("consent_required");
                }
                let Ok(launch) = at_mcp::StdioLaunch::from_server(&target.server) else {
                    return failed_check("invalid_launch");
                };
                // The persisted background-health choice is the user's explicit
                // "run without asking again" consent, and load_targets verified a manual test.
                let consent = at_mcp::LaunchConsent::user_approved(&launch);
                let client = at_mcp::StdioClient::new(
                    Arc::new(at_mcp::PathResolver::new()),
                    Arc::clone(&self.secrets),
                );
                let mut session = match client.open(&launch, &consent, cancellation).await {
                    Ok(session) => session,
                    Err(_) => return failed_check("stdio_check_failed"),
                };
                let listing = session.list_tools(cancellation).await;
                let _shutdown = session.shutdown(cancellation).await;
                match listing {
                    Ok(listing) => CheckResult::Ok {
                        latency_ms: started.elapsed().as_millis().max(1) as i64,
                        tools_count: listing.tools.len() as i64,
                    },
                    Err(_) => failed_check("stdio_check_failed"),
                }
            }
            at_model::McpTransport::Http { .. } => {
                let mut server = target.server.clone();
                if self.resolve_http_headers(&mut server).is_err() {
                    return failed_check("http_secret_unavailable");
                }
                let client =
                    at_mcp::HttpClient::new(Arc::new(network.policy()), Arc::clone(&self.secrets));
                let session = match client.open_scheduled_health(&server, cancellation).await {
                    Ok(session) => session,
                    Err(_) => return failed_check("http_check_failed"),
                };
                let listing = session.list_tools(cancellation).await;
                session.close().await;
                match listing {
                    Ok(listing) => CheckResult::Ok {
                        latency_ms: started.elapsed().as_millis().max(1) as i64,
                        tools_count: listing.tools.len() as i64,
                    },
                    Err(_) => failed_check("http_check_failed"),
                }
            }
        }
    }
}

fn failed_check(code: &str) -> CheckResult {
    CheckResult::Failed {
        code: code.to_owned(),
        redacted_error: "MCP health check failed".to_owned(),
    }
}

/// Inputs needed to execute and persist one scheduled check.
pub struct ExecutionContext<'a, R> {
    /// Injected time source.
    pub clock: &'a dyn Clock,
    /// Configured base interval.
    pub interval: HealthInterval,
    /// Fakeable MCP runner.
    pub runner: &'a R,
    /// Central network policy.
    pub network: &'a NetworkState,
    /// Durable health history.
    pub database: &'a Mutex<Database>,
    /// App-lifetime cancellation token.
    pub cancellation: &'a tokio_util::sync::CancellationToken,
    /// Per-check wall-clock budget. Production passes [`HEALTH_CHECK_DEADLINE`];
    /// process-level tests shorten it so a hung fixture is bounded in test time.
    pub deadline: Duration,
    /// Deterministic jitter provider.
    pub jitter: &'a dyn Jitter,
    /// UI event publisher; absent for jobs that only need persistence.
    pub event_sink: Option<&'a dyn HealthEventSink>,
}

/// Outcome from a runner.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckResult {
    /// A successful check.
    Ok {
        /// Response time in milliseconds.
        latency_ms: i64,
        /// Number of tools returned.
        tools_count: i64,
    },
    /// The server check failed; error content must already be redacted.
    Failed {
        /// Stable error kind identifier.
        code: String,
        /// Redacted failure explanation.
        redacted_error: String,
    },
}

/// Tauri event emitted when a managed MCP health result changes.
pub const EVENT_MCP_HEALTH_CHANGED: &str = "agentuna:mcp-health-changed";

/// Health state suitable for change notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub enum McpHealthEventStatus {
    /// The scheduled check succeeded.
    Ok,
    /// The scheduled check failed.
    Failed,
    /// The HTTP check was skipped because the app is offline.
    SkippedOffline,
}

/// One deduplicated health-state change delivered to the webview.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpHealthChangedEvent {
    /// Stable AgenTuna item id.
    pub item_id: String,
    /// Display name of the server.
    pub server_name: String,
    /// Current health state.
    pub status: McpHealthEventStatus,
    /// Stable failure category, when failed.
    pub error_code: Option<String>,
    /// Redacted failure explanation, when failed.
    pub redacted_error: Option<String>,
}

/// Publishes health changes after their rows are persisted.
pub trait HealthEventSink: Send + Sync {
    /// Sends one status transition or changed failure to the UI bridge.
    fn publish(&self, event: McpHealthChangedEvent);
}

/// Production Tauri event sink for the shared health scheduler.
pub struct TauriHealthEventSink {
    app: tauri::AppHandle,
}

impl TauriHealthEventSink {
    /// Creates a sink using the app's webview event channel.
    #[must_use]
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl HealthEventSink for TauriHealthEventSink {
    fn publish(&self, event: McpHealthChangedEvent) {
        if let Err(error) = self.app.emit(EVENT_MCP_HEALTH_CHANGED, event) {
            tracing::warn!(%error, "could not publish MCP health status change");
        }
    }
}

/// One planned scheduled invocation.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DueCheck {
    /// Run this target now.
    Run(HealthTarget),
    /// Persist a skipped_offline HTTP result; it is not a failure.
    SkippedOffline(HealthTarget),
}

/// Scheduling policy inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulePolicy {
    /// Configured base cadence.
    pub interval: HealthInterval,
    /// User's background health consent.
    pub background: BackgroundHealth,
    /// Whether the app is online.
    pub online: bool,
}

#[derive(Clone, Copy, Debug)]
struct TargetState {
    due_at_ms: i64,
    failures: u8,
    in_flight: bool,
}

/// Per-server schedule state; callers drive it from virtual or production time.
pub struct HealthScheduler {
    states: HashMap<String, TargetState>,
    last_published: HashMap<String, McpHealthChangedEvent>,
    paused: bool,
    pause_started_ms: Option<i64>,
    last_pruned_ms: Option<i64>,
    last_activity_ms: i64,
}
impl Default for HealthScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthScheduler {
    /// Creates an empty scheduler.
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
            last_published: HashMap::new(),
            paused: false,
            pause_started_ms: None,
            last_activity_ms: 0,
            last_pruned_ms: None,
        }
    }
    /// Records an outcome through the canonical health repository with scheduled trigger.
    ///
    /// HTTP checks pass through `NetworkState::authorize` before the runner is called.
    pub async fn execute<R: HealthRunner>(
        &mut self,
        due: DueCheck,
        context: &ExecutionContext<'_, R>,
    ) -> Result<(), JobError> {
        let now_ms = context.clock.now_ms();
        let mut http_host = None;
        let (target, result) = match due {
            DueCheck::SkippedOffline(target) => (target, None),
            DueCheck::Run(target) => {
                if target.transport == HealthTransport::Http {
                    let host = match target.host.as_deref() {
                        Some(host) => host,
                        None => {
                            self.cancel(&target.id);
                            return Err(JobError::MissingHttpHost);
                        }
                    };
                    let authorized = match context.network.authorize(
                        NetworkAction::ScheduledHttpHealthCheck,
                        host,
                        NetworkTrigger::Background,
                    ) {
                        Ok(host) => host,
                        Err(error) => {
                            self.cancel(&target.id);
                            return Err(JobError::NetworkDenied(error));
                        }
                    };
                    http_host = Some(authorized);
                }
                let result = tokio::select! {
                    result = tokio::time::timeout(context.deadline, context.runner.check(&target, context.network, context.cancellation)) => {
                        result.unwrap_or_else(|_| failed_check("check_timeout"))
                    },
                    _ = context.cancellation.cancelled() => {
                        self.cancel(&target.id);
                        return Ok(());
                    }
                };
                (target.clone(), Some(result))
            }
        };
        if result.is_some()
            && let Some(host) = http_host
        {
            context.network.record(at_model::NetworkEvent::new(
                time::OffsetDateTime::now_utc(),
                NetworkAction::ScheduledHttpHealthCheck,
                Some(host),
                0,
            ));
        }
        let (status, latency_ms, tools_count, error_code, error_text_redacted) =
            match result.as_ref() {
                Some(CheckResult::Ok {
                    latency_ms,
                    tools_count,
                }) => (
                    HealthStatus::Ok,
                    Some(*latency_ms),
                    Some(*tools_count),
                    None,
                    None,
                ),
                Some(CheckResult::Failed {
                    code,
                    redacted_error,
                }) => (
                    HealthStatus::Failed,
                    None,
                    None,
                    Some(code.as_str()),
                    Some(redacted_error.as_str()),
                ),
                None => (HealthStatus::SkippedOffline, None, None, None, None),
            };
        let check = NewHealthCheck {
            item_id: target.item_id.as_deref(),
            server_name: &target.name,
            scope: target.scope,
            project_id: target.project_id.as_deref(),
            agent_id: target.agent_id.as_ref(),
            transport: Some(match target.transport {
                HealthTransport::Stdio => DbTransport::Stdio,
                HealthTransport::Http => DbTransport::Http,
            }),
            checked_at_unix_ms: now_ms,
            status,
            trigger: HealthTrigger::Scheduled,
            latency_ms,
            tools_count,
            error_code,
            error_text_redacted,
        };
        let database = context
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Err(error) = at_db::health::record_check(database.connection(), &check, None) {
            if result.is_some() {
                self.cancel(&target.id);
            }
            return Err(JobError::Database(error));
        }
        if let Some(result) = &result {
            self.finish(&target.id, now_ms, result, context.interval, context.jitter);
        }
        if self.last_pruned_ms.is_none_or(|last| {
            now_ms.saturating_sub(last) >= at_db::health::HEALTH_PRUNE_INTERVAL.as_millis() as i64
        }) {
            at_db::health::prune_health_checks(database.connection())?;
            self.last_pruned_ms = Some(now_ms);
        }
        drop(database);
        let event = match result.as_ref() {
            Some(CheckResult::Ok { .. }) => McpHealthChangedEvent {
                item_id: target.item_id.clone().unwrap_or_else(|| target.id.clone()),
                server_name: target.name.clone(),
                status: McpHealthEventStatus::Ok,
                error_code: None,
                redacted_error: None,
            },
            Some(CheckResult::Failed {
                code,
                redacted_error,
            }) => McpHealthChangedEvent {
                item_id: target.item_id.clone().unwrap_or_else(|| target.id.clone()),
                server_name: target.name.clone(),
                status: McpHealthEventStatus::Failed,
                error_code: Some(code.clone()),
                redacted_error: Some(redacted_error.clone()),
            },
            None => McpHealthChangedEvent {
                item_id: target.item_id.clone().unwrap_or_else(|| target.id.clone()),
                server_name: target.name.clone(),
                status: McpHealthEventStatus::SkippedOffline,
                error_code: None,
                redacted_error: None,
            },
        };
        if self.last_published.get(&target.id) != Some(&event) {
            if let Some(sink) = context.event_sink {
                sink.publish(event.clone());
            }
            self.last_published.insert(target.id.clone(), event);
        }
        Ok(())
    }

    /// Updates visibility. Resume shifts deadlines by the hidden duration and adds a stable
    /// caller-provided stagger in the inclusive 0–2 second range.
    pub fn set_visible(&mut self, visible: bool, now_ms: i64, stagger_ms: u64) {
        if !visible && !self.paused {
            self.paused = true;
            self.pause_started_ms = Some(now_ms);
        } else if visible && self.paused {
            let hidden_for = self
                .pause_started_ms
                .take()
                .map_or(0, |start| now_ms.saturating_sub(start));
            for state in self.states.values_mut() {
                let stagger = stagger_ms.min(2_000) as i64;
                state.due_at_ms = if state.due_at_ms <= now_ms {
                    now_ms.saturating_add(stagger)
                } else {
                    state
                        .due_at_ms
                        .saturating_add(hidden_for)
                        .saturating_add(stagger)
                };
            }
            self.paused = false;
        }
    }
    /// Updates scheduler pause state from visibility, minimization and recent user activity.
    pub fn update_window_state(
        &mut self,
        visible: bool,
        minimized: bool,
        now_ms: i64,
        stagger_ms: u64,
    ) {
        let active = now_ms.saturating_sub(self.last_activity_ms) <= 60_000;
        self.set_visible(visible && !minimized && active, now_ms, stagger_ms);
    }

    /// Records user activity for the 60-second idle pause.
    pub fn record_activity(&mut self, now_ms: i64) {
        self.last_activity_ms = now_ms;
    }

    /// Returns eligible checks due at `now_ms`; in-flight servers are never returned twice.
    /// New targets are initially due after the base interval plus bounded jitter.
    pub fn due_checks(
        &mut self,
        now_ms: i64,
        policy: SchedulePolicy,
        targets: &[HealthTarget],
        jitter: &dyn Jitter,
    ) -> Vec<DueCheck> {
        if self.paused
            || policy.interval == HealthInterval::Off
            || policy.background == BackgroundHealth::Off
        {
            return Vec::new();
        }
        let base = interval_ms(policy.interval);
        let mut due = Vec::new();
        for target in targets {
            if !eligible(target, policy.background) {
                continue;
            }
            let state = self
                .states
                .entry(target.id.clone())
                .or_insert_with(|| TargetState {
                    due_at_ms: now_ms
                        .saturating_add(base)
                        .saturating_add(bounded_jitter(jitter)),
                    failures: 0,
                    in_flight: false,
                });
            if now_ms < state.due_at_ms || state.in_flight {
                continue;
            }
            state.in_flight = true;
            if target.transport == HealthTransport::Http && !policy.online {
                state.in_flight = false;
                state.due_at_ms = now_ms.saturating_add(base);
                due.push(DueCheck::SkippedOffline(target.clone()));
            } else {
                due.push(DueCheck::Run(target.clone()));
            }
        }
        due
    }

    /// Finishes a started check and schedules its next attempt with capped exponential retry.
    pub fn finish(
        &mut self,
        id: &str,
        now_ms: i64,
        result: &CheckResult,
        interval: HealthInterval,
        jitter: &dyn Jitter,
    ) {
        let Some(state) = self.states.get_mut(id) else {
            return;
        };
        state.in_flight = false;
        let delay = if matches!(result, CheckResult::Ok { .. }) {
            state.failures = 0;
            interval_ms(interval)
        } else {
            state.failures = state.failures.saturating_add(1);
            retry_ms(state.failures)
        };
        state.due_at_ms = now_ms
            .saturating_add(delay)
            .saturating_add(bounded_jitter(jitter));
    }

    /// Cancels the in-flight marker for an interrupted invocation without hiding its next due time.
    pub fn cancel(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.in_flight = false;
        }
    }

    /// Whether the HTTP check is the registered network action.
    #[must_use]
    pub const fn network_action(target: &HealthTarget) -> Option<NetworkAction> {
        match target.transport {
            HealthTransport::Stdio => None,
            HealthTransport::Http => Some(NetworkAction::ScheduledHttpHealthCheck),
        }
    }

    /// The trigger used when authorizing scheduled HTTP requests.
    #[must_use]
    pub const fn network_trigger() -> NetworkTrigger {
        NetworkTrigger::Background
    }
}
/// App-lifecycle signals shared with the scheduler worker.
#[derive(Clone)]
pub struct JobsControl {
    cancellation: tokio_util::sync::CancellationToken,
    visibility: Arc<Mutex<Visibility>>,
    state_changed: Arc<Condvar>,
}

#[derive(Clone, Copy)]
struct Visibility {
    visible: bool,
    minimized: bool,
    focused: bool,
    inactive_since_ms: Option<i64>,
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

impl Default for JobsControl {
    fn default() -> Self {
        Self {
            cancellation: tokio_util::sync::CancellationToken::new(),
            visibility: Arc::new(Mutex::new(Visibility {
                visible: true,
                minimized: false,
                focused: true,
                inactive_since_ms: None,
            })),
            state_changed: Arc::new(Condvar::new()),
        }
    }
}

impl JobsControl {
    /// Cancellation passed to active runners; app exit calls [`Self::shutdown`].
    #[must_use]
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    /// Cancels health work as part of the app's bounded shutdown sequence.
    pub fn shutdown(&self) {
        self.cancellation.cancel();
    }

    /// Updates Tauri window state and returns whether scheduled checks are paused.
    pub fn update_window(
        &self,
        visible: bool,
        minimized: bool,
        focused: bool,
        now_ms: i64,
    ) -> bool {
        let mut state = self
            .visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.visible = visible;
        state.minimized = minimized;
        if state.focused && !focused {
            state.inactive_since_ms = Some(now_ms);
        } else if focused {
            state.inactive_since_ms = None;
        }
        state.focused = focused;
        let paused = !state.visible
            || state.minimized
            || state
                .inactive_since_ms
                .is_some_and(|since| now_ms.saturating_sub(since) > 60_000);
        drop(state);
        self.state_changed.notify_all();
        paused
    }
    /// Blocks without polling while the app is hidden, minimized or inactive past the idle grace.
    pub fn wait_until_window_active(&self) {
        let mut state = self
            .visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.visible
            || state.minimized
            || state
                .inactive_since_ms
                .is_some_and(|since| system_now_ms().saturating_sub(since) > 60_000)
        {
            state = self
                .state_changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    /// Whether the window is currently hidden, minimized or inactive beyond the idle grace period.
    #[must_use]
    pub fn window_is_paused(&self, now_ms: i64) -> bool {
        let state = self
            .visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.visible
            || state.minimized
            || state
                .inactive_since_ms
                .is_some_and(|since| now_ms.saturating_sub(since) > 60_000)
    }
    /// Applies the tracked window state to the scheduler's pause/resume deadlines.
    pub fn apply_window_to_scheduler(
        &self,
        scheduler: &mut HealthScheduler,
        now_ms: i64,
        stagger_ms: u64,
    ) {
        let state = self
            .visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let paused = !state.visible
            || state.minimized
            || state
                .inactive_since_ms
                .is_some_and(|since| now_ms.saturating_sub(since) > 60_000);
        scheduler.set_visible(!paused, now_ms, stagger_ms);
    }
}

#[cfg(test)]
#[test]
fn hidden_workers_wait_for_window_resume_without_polling() {
    let control = JobsControl::default();
    control.update_window(false, false, false, 1);

    let waiting = control.clone();
    let (resumed_tx, resumed_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        waiting.wait_until_window_active();
        let _ = resumed_tx.send(());
    });

    assert!(
        resumed_rx.recv_timeout(Duration::from_millis(600)).is_err(),
        "a hidden window must keep its worker asleep"
    );
    control.update_window(true, false, true, 601);
    assert!(
        resumed_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
        "visible state wakes the paused worker"
    );
    handle.join().expect("worker exits after visibility update");
}

#[cfg(test)]
#[test]
fn jobs_control_pauses_for_hidden_minimized_and_sixty_seconds_inactive() {
    let control = JobsControl::default();
    assert!(!control.update_window(true, false, true, 0));
    assert!(!control.window_is_paused(0));
    assert!(control.update_window(false, false, false, 1));
    assert!(control.window_is_paused(1));
    assert!(!control.update_window(true, false, true, 2));
    assert!(!control.window_is_paused(2));
    assert!(control.update_window(true, true, true, 3));
    assert!(control.window_is_paused(3));
    assert!(!control.update_window(true, false, true, 4));
    assert!(!control.update_window(true, false, false, 5));
    assert!(!control.window_is_paused(60_005));
    assert!(control.window_is_paused(60_006));
    assert!(control.update_window(true, false, false, 60_006));
}

#[test]
fn jobs_control_cancels_running_checks_on_shutdown() {
    let control = JobsControl::default();
    let cancellation = control.cancellation_token();
    assert!(!cancellation.is_cancelled());
    control.shutdown();
    assert!(cancellation.is_cancelled());
}
/// Loads managed MCP targets from configured stores. A malformed store is skipped with a warning.
pub fn load_targets(paths: &AppPaths, database: &Database) -> Result<Vec<HealthTarget>, JobError> {
    let mut scopes = vec![Scope::Global, Scope::User];
    if paths.project_store().is_some() {
        scopes.extend([Scope::Project, Scope::Local]);
    }
    let mut targets = Vec::new();
    for scope in scopes {
        let layout = match StoreLayout::for_scope(paths, scope) {
            Ok(layout) => layout,
            Err(error) => {
                tracing::warn!(%error, ?scope, "health scheduler could not resolve store");
                continue;
            }
        };
        let manifest = match StoreManifest::load(&layout) {
            Ok(loaded) => loaded.manifest,
            Err(error) => {
                tracing::debug!(%error, ?scope, "health scheduler skipped unavailable manifest");
                continue;
            }
        };
        let mcp = match McpStore::load(&layout) {
            Ok(loaded) => loaded.store,
            Err(error) => {
                tracing::warn!(%error, ?scope, "health scheduler skipped invalid MCP store");
                continue;
            }
        };
        for (slug, definition) in mcp.servers {
            let Ok(slug) = at_store::layout::Slug::parse(&slug) else {
                continue;
            };
            let Some(item) = manifest.item_by_slug(ItemKind::McpServer, &slug) else {
                tracing::warn!(server = %slug, ?scope, "MCP entry has no manifest identity; skipping scheduled check");
                continue;
            };
            if manifest
                .disabled()
                .iter()
                .any(|disabled| disabled.item_id == item.id)
            {
                continue;
            }
            let item_id = item.id.to_string();
            let history = at_db::health::history(database.connection(), &item_id, None, None)?;
            let manually_tested = history
                .iter()
                .any(|check| check.trigger == HealthTrigger::Manual);
            let (transport, host) = match &definition.transport {
                at_model::McpTransport::Stdio { .. } => (HealthTransport::Stdio, None),
                at_model::McpTransport::Http { url, .. } => {
                    (HealthTransport::Http, url.host_str().map(str::to_owned))
                }
            };
            let name = definition.display_name.clone();
            let server = McpServer::from_def(McpServerId::new(), scope, definition);
            targets.push(HealthTarget {
                id: item_id.clone(),
                item_id: Some(item_id),
                name,
                scope,
                project_id: None,
                agent_id: None,
                transport,
                host,
                manually_tested,
                server,
            });
        }
    }
    Ok(targets)
}
/// Abstracts scheduler wakeups for virtual-time tests.
#[allow(async_fn_in_trait)]
pub trait SchedulerTimer: Send + Sync {
    /// Waits for a timer wake or cancellation; returns false on cancellation.
    async fn wait(
        &self,
        delay: Duration,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> bool;
}

/// Production timer implementation. Tests should use a virtual timer.
pub struct TokioSchedulerTimer;

impl SchedulerTimer for TokioSchedulerTimer {
    async fn wait(
        &self,
        delay: Duration,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> bool {
        tokio::select! {
            () = tokio::time::sleep(delay) => true,
            () = cancellation.cancelled() => false,
        }
    }
}

/// Inputs needed by the long-running settings/store refresh loop.
pub struct SchedulerEnvironment<'a, R, T> {
    /// Resolved app roots.
    pub paths: &'a AppPaths,
    /// Health history and preferences.
    pub database: &'a Mutex<Database>,
    /// Shared network authorization state.
    pub network: &'a NetworkState,
    /// Injected clock.
    pub clock: &'a dyn Clock,
    /// Deterministic jitter source.
    pub jitter: &'a dyn Jitter,
    /// MCP check implementation.
    pub runner: &'a R,
    /// Injected timer.
    pub timer: &'a T,
    /// Tauri event publisher for status changes.
    pub event_sink: Option<&'a dyn HealthEventSink>,
}

/// Runs policy refresh, store discovery and scheduled checks until app shutdown.
pub async fn run_scheduler<R: HealthRunner, T: SchedulerTimer>(
    scheduler: &mut HealthScheduler,
    control: &JobsControl,
    environment: &SchedulerEnvironment<'_, R, T>,
) -> Result<(), JobError> {
    let cancellation = control.cancellation_token();
    while !cancellation.is_cancelled() {
        let (settings, targets) = {
            let database = environment
                .database
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                at_db::settings::load(database.connection())?,
                load_targets(environment.paths, &database)?,
            )
        };
        let background = match settings.background_health {
            BackgroundHealth::Off => at_model::BackgroundHealth::Off,
            BackgroundHealth::Local => at_model::BackgroundHealth::Local,
            BackgroundHealth::All => at_model::BackgroundHealth::All,
        };
        environment.network.set_background_health(background);
        let now_ms = environment.clock.now_ms();
        let stagger_ms = environment.jitter.delay_ms(2_000);
        control.apply_window_to_scheduler(scheduler, now_ms, stagger_ms);
        let policy = SchedulePolicy {
            interval: settings.health_interval,
            background: settings.background_health,
            online: !environment.network.policy().offline(),
        };
        let due = scheduler.due_checks(now_ms, policy, &targets, environment.jitter);
        for check in due {
            if cancellation.is_cancelled() {
                break;
            }
            scheduler
                .execute(
                    check,
                    &ExecutionContext {
                        clock: environment.clock,
                        interval: settings.health_interval,
                        runner: environment.runner,
                        network: environment.network,
                        database: environment.database,
                        cancellation: &cancellation,
                        deadline: HEALTH_CHECK_DEADLINE,
                        jitter: environment.jitter,
                        event_sink: environment.event_sink,
                    },
                )
                .await?;
        }
        if !environment
            .timer
            .wait(Duration::from_secs(1), &cancellation)
            .await
        {
            break;
        }
    }
    Ok(())
}

fn eligible(target: &HealthTarget, background: BackgroundHealth) -> bool {
    match (background, target.transport) {
        (BackgroundHealth::All, _) => true,
        (BackgroundHealth::Local, HealthTransport::Stdio) => target.manually_tested,
        _ => false,
    }
}

fn interval_ms(interval: HealthInterval) -> i64 {
    i64::from(interval.minutes()).saturating_mul(60_000)
}
/// Failure while authorizing or persisting a scheduled health result.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// An HTTP target lacked a policy host.
    #[error("scheduled HTTP target has no host")]
    MissingHttpHost,
    /// The central network policy refused the request.
    #[error(transparent)]
    NetworkDenied(#[from] at_model::network::NetworkDenied),
    /// The health history insert or retention prune failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// A settings row was unreadable.
    #[error(transparent)]
    Settings(#[from] at_db::settings::SettingsError),
}

fn retry_ms(failures: u8) -> i64 {
    let minutes = 5_u64
        .saturating_mul(1_u64 << failures.saturating_sub(1).min(4))
        .min(60);
    (minutes * 60_000) as i64
}

fn bounded_jitter(jitter: &dyn Jitter) -> i64 {
    jitter.delay_ms(30_000).min(30_000) as i64
}

/// Scheduler cadence jitter bound.
pub const MAX_JITTER: Duration = Duration::from_secs(30);

/// Maximum wall-clock duration of one scheduled MCP health check.
pub const HEALTH_CHECK_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedJitter(u64);
    impl Jitter for FixedJitter {
        fn delay_ms(&self, max_ms: u64) -> u64 {
            self.0.min(max_ms)
        }
    }

    fn target(id: &str, transport: HealthTransport, manually_tested: bool) -> HealthTarget {
        let mcp_transport = match transport {
            HealthTransport::Stdio => at_model::McpTransport::Stdio {
                command: "test-server".into(),
                args: Vec::new(),
                cwd: None,
                env: Default::default(),
            },
            HealthTransport::Http => at_model::McpTransport::Http {
                url: url::Url::parse("https://health.example.test").expect("fixture URL"),
                headers: Default::default(),
                oauth: None,
            },
        };
        let server = McpServer::from_def(
            McpServerId::new(),
            Scope::User,
            at_model::McpServerDef {
                display_name: id.into(),
                description: None,
                transport: mcp_transport,
                extra: Default::default(),
            },
        );
        HealthTarget {
            id: id.into(),
            name: id.into(),
            item_id: None,
            scope: Scope::User,
            project_id: None,
            agent_id: None,
            transport,
            host: (transport == HealthTransport::Http).then(|| "health.example.test".into()),
            manually_tested,
            server,
        }
    }
    #[derive(Default)]
    struct EventRecorder(Mutex<Vec<McpHealthChangedEvent>>);

    impl HealthEventSink for EventRecorder {
        fn publish(&self, event: McpHealthChangedEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    struct FailingRunner;

    impl HealthRunner for FailingRunner {
        async fn check(
            &self,
            _target: &HealthTarget,
            _network: &NetworkState,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> CheckResult {
            CheckResult::Failed {
                code: "connection_failed".into(),
                redacted_error: "MCP health check failed".into(),
            }
        }
    }

    struct HangingRunner;
    impl HealthRunner for HangingRunner {
        async fn check(
            &self,
            _target: &HealthTarget,
            _network: &NetworkState,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> CheckResult {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hanging_check_times_out_and_the_next_server_runs() {
        let mut scheduler = HealthScheduler::new();
        let targets = [
            target("hang", HealthTransport::Stdio, true),
            target("next", HealthTransport::Stdio, true),
        ];
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        let jitter = FixedJitter(0);
        scheduler.due_checks(0, policy, &targets, &jitter);
        let due = scheduler.due_checks(300_000, policy, &targets, &jitter);
        let database = Mutex::new(Database::open_in_memory().expect("database initializes"));
        let network = NetworkState::default();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let mut due = due.into_iter();
        scheduler
            .execute(
                due.next().expect("first check due"),
                &ExecutionContext {
                    clock: &TestClock(300_000),
                    interval: HealthInterval::FiveMinutes,
                    runner: &HangingRunner,
                    network: &network,
                    database: &database,
                    cancellation: &cancellation,
                    deadline: HEALTH_CHECK_DEADLINE,
                    jitter: &jitter,
                    event_sink: None,
                },
            )
            .await
            .expect("hung check is recorded as failure");
        scheduler
            .execute(
                due.next().expect("second check due"),
                &ExecutionContext {
                    clock: &TestClock(300_000),
                    interval: HealthInterval::FiveMinutes,
                    runner: &PassingRunner,
                    network: &network,
                    database: &database,
                    cancellation: &cancellation,
                    deadline: HEALTH_CHECK_DEADLINE,
                    jitter: &jitter,
                    event_sink: None,
                },
            )
            .await
            .expect("next server check succeeds");
        let database = database.lock().expect("database lock");
        let rows: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM health_checks", [], |row| row.get(0))
            .expect("both checks persisted");
        assert_eq!(rows, 2);
        let timeout: i64 = database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM health_checks WHERE error_code = 'check_timeout'",
                [],
                |row| row.get(0),
            )
            .expect("timeout failure recorded");
        assert_eq!(timeout, 1);
    }

    #[tokio::test]
    async fn repeated_identical_failures_persist_but_publish_one_status_event() {
        let mut scheduler = HealthScheduler::new();
        let target = target("test", HealthTransport::Stdio, true);
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        let jitter = FixedJitter(0);
        let database = Mutex::new(Database::open_in_memory().expect("database initializes"));
        let network = NetworkState::default();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let events = EventRecorder::default();
        scheduler.due_checks(0, policy, std::slice::from_ref(&target), &jitter);

        for attempt in 0..10 {
            let now_ms = if attempt == 0 {
                300_000
            } else {
                scheduler.states["test"].due_at_ms
            };
            let due = scheduler
                .due_checks(now_ms, policy, std::slice::from_ref(&target), &jitter)
                .into_iter()
                .next()
                .expect("failure retry due");
            scheduler
                .execute(
                    due,
                    &ExecutionContext {
                        clock: &TestClock(now_ms),
                        interval: HealthInterval::FiveMinutes,
                        runner: &FailingRunner,
                        network: &network,
                        database: &database,
                        cancellation: &cancellation,
                        deadline: HEALTH_CHECK_DEADLINE,
                        jitter: &jitter,
                        event_sink: Some(&events),
                    },
                )
                .await
                .expect("failed check is recorded");
        }

        assert_eq!(
            events.0.lock().unwrap().len(),
            1,
            "ten identical failures are one state, not ten events"
        );
        let rows = health_row_count(&database);
        assert_eq!(rows, 10, "every check is still persisted");

        // The next state change (failure -> success) publishes exactly one more event.
        let now_ms = scheduler.states["test"].due_at_ms;
        let due = scheduler
            .due_checks(now_ms, policy, std::slice::from_ref(&target), &jitter)
            .into_iter()
            .next()
            .expect("recovery check due");
        scheduler
            .execute(
                due,
                &ExecutionContext {
                    clock: &TestClock(now_ms),
                    interval: HealthInterval::FiveMinutes,
                    runner: &PassingRunner,
                    network: &network,
                    database: &database,
                    cancellation: &cancellation,
                    deadline: HEALTH_CHECK_DEADLINE,
                    jitter: &jitter,
                    event_sink: Some(&events),
                },
            )
            .await
            .expect("recovery check is recorded");
        let recorded = events.0.lock().unwrap();
        assert_eq!(
            recorded.len(),
            2,
            "one event per state transition, not per check"
        );
        assert_eq!(recorded[1].status, McpHealthEventStatus::Ok);
        assert_eq!(health_row_count(&database), 11);
    }

    /// Counts persisted health rows.
    fn health_row_count(database: &Mutex<Database>) -> i64 {
        database
            .lock()
            .expect("database lock")
            .connection()
            .query_row("SELECT COUNT(*) FROM health_checks", [], |row| row.get(0))
            .expect("count health rows")
    }

    #[tokio::test]
    async fn hidden_window_writes_no_scheduled_rows_and_resume_writes_one_per_due_server() {
        let mut scheduler = HealthScheduler::new();
        let targets = [
            target("alpha", HealthTransport::Stdio, true),
            target("beta", HealthTransport::Stdio, true),
        ];
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        let jitter = FixedJitter(0);
        let database = Mutex::new(Database::open_in_memory().expect("database initializes"));
        let network = NetworkState::default();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let control = JobsControl::default();
        // One visible pass registers each server's first due time, as the real loop does.
        control.apply_window_to_scheduler(&mut scheduler, 0, 1_500);
        assert!(
            scheduler
                .due_checks(0, policy, &targets, &jitter)
                .is_empty()
        );
        control.update_window(false, false, false, 0);

        // Thirty simulated minutes minimized: the loop sees no due check and writes nothing.
        let mut now_ms = 0_i64;
        for _ in 0..1_800 {
            control.apply_window_to_scheduler(&mut scheduler, now_ms, 1_500);
            let due = scheduler.due_checks(now_ms, policy, &targets, &jitter);
            assert!(due.is_empty(), "no check runs while the window is hidden");
            now_ms += 1_000;
        }
        assert_eq!(
            health_row_count(&database),
            0,
            "a hidden window writes zero scheduled rows"
        );

        // Visible again: the stagger puts every overdue server within two simulated seconds.
        control.update_window(true, false, true, now_ms);
        let mut executed = 0_usize;
        for tick in 0..=2 {
            let at = now_ms + tick * 1_000;
            control.apply_window_to_scheduler(&mut scheduler, at, 1_500);
            for check in scheduler.due_checks(at, policy, &targets, &jitter) {
                scheduler
                    .execute(
                        check,
                        &ExecutionContext {
                            clock: &TestClock(at),
                            interval: HealthInterval::FiveMinutes,
                            runner: &PassingRunner,
                            network: &network,
                            database: &database,
                            cancellation: &cancellation,
                            deadline: HEALTH_CHECK_DEADLINE,
                            jitter: &jitter,
                            event_sink: None,
                        },
                    )
                    .await
                    .expect("resumed check is recorded");
                executed += 1;
            }
        }
        assert_eq!(
            executed, 2,
            "exactly one row per due server within two simulated seconds"
        );
        assert_eq!(health_row_count(&database), 2);
    }

    fn policy(
        interval: HealthInterval,
        background: BackgroundHealth,
        online: bool,
    ) -> SchedulePolicy {
        SchedulePolicy {
            interval,
            background,
            online,
        }
    }

    struct TestClock(i64);
    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0
        }
    }

    struct PassingRunner;
    impl HealthRunner for PassingRunner {
        async fn check(
            &self,
            _target: &HealthTarget,
            _network: &NetworkState,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> CheckResult {
            CheckResult::Ok {
                latency_ms: 12,
                tools_count: 3,
            }
        }
    }

    #[tokio::test]
    async fn injected_clock_and_runner_record_scheduled_health() {
        let mut scheduler = HealthScheduler::new();
        let target = target("test", HealthTransport::Stdio, true);
        let zero = FixedJitter(0);
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        scheduler.due_checks(0, policy, std::slice::from_ref(&target), &zero);
        let due = scheduler.due_checks(300_000, policy, std::slice::from_ref(&target), &zero);
        let database = Mutex::new(Database::open_in_memory().expect("database initializes"));
        scheduler
            .execute(
                due.into_iter().next().expect("scheduled target due"),
                &ExecutionContext {
                    clock: &TestClock(123_456),
                    interval: HealthInterval::FiveMinutes,
                    runner: &PassingRunner,
                    network: &NetworkState::default(),
                    database: &database,
                    cancellation: &tokio_util::sync::CancellationToken::new(),
                    deadline: HEALTH_CHECK_DEADLINE,
                    jitter: &zero,
                    event_sink: None,
                },
            )
            .await
            .expect("scheduled check persists");
        let database = database.lock().unwrap();
        let row: (i64, String, String) = database
            .connection()
            .query_row(
                "SELECT checked_at, status, trigger FROM health_checks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("health row exists");
        assert_eq!(row, (123_456, "ok".into(), "scheduled".into()));
    }

    #[test]
    fn gates_scope_and_offline_http_without_running_stdio() {
        let mut scheduler = HealthScheduler::new();
        let servers = [
            target("untested", HealthTransport::Stdio, false),
            target("tested", HealthTransport::Stdio, true),
            target("http", HealthTransport::Http, true),
        ];
        let zero = FixedJitter(0);
        assert!(
            scheduler
                .due_checks(
                    0,
                    policy(HealthInterval::Off, BackgroundHealth::All, true),
                    &servers,
                    &zero
                )
                .is_empty()
        );
        assert!(
            scheduler
                .due_checks(
                    0,
                    policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true),
                    &servers,
                    &zero
                )
                .is_empty()
        );
        scheduler.due_checks(
            0,
            policy(HealthInterval::FiveMinutes, BackgroundHealth::All, true),
            &servers,
            &zero,
        );
        let due = scheduler.due_checks(
            300_000,
            policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true),
            &servers,
            &zero,
        );
        assert_eq!(due, vec![DueCheck::Run(servers[1].clone())]);
        scheduler.finish(
            "tested",
            300_000,
            &CheckResult::Ok {
                latency_ms: 1,
                tools_count: 0,
            },
            HealthInterval::FiveMinutes,
            &zero,
        );
        let due = scheduler.due_checks(
            600_000,
            policy(HealthInterval::FiveMinutes, BackgroundHealth::All, false),
            std::slice::from_ref(&servers[2]),
            &zero,
        );
        assert_eq!(due, vec![DueCheck::SkippedOffline(servers[2].clone())]);
    }

    #[test]
    fn retries_cap_and_success_resets_interval() {
        let mut scheduler = HealthScheduler::new();
        let server = target("x", HealthTransport::Stdio, true);
        let zero = FixedJitter(0);
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        scheduler.due_checks(0, policy, std::slice::from_ref(&server), &zero);
        for (index, minutes) in [5_i64, 10, 20, 40, 60, 60].into_iter().enumerate() {
            let now = if index == 0 {
                300_000
            } else {
                scheduler.states["x"].due_at_ms
            };
            assert_eq!(
                scheduler
                    .due_checks(now, policy, std::slice::from_ref(&server), &zero)
                    .len(),
                1
            );
            scheduler.finish(
                "x",
                now,
                &CheckResult::Failed {
                    code: "failed".into(),
                    redacted_error: "failed".into(),
                },
                HealthInterval::FiveMinutes,
                &zero,
            );
            assert_eq!(scheduler.states["x"].due_at_ms, now + minutes * 60_000);
        }
        let now = scheduler.states["x"].due_at_ms;
        scheduler.due_checks(now, policy, std::slice::from_ref(&server), &zero);
        scheduler.finish(
            "x",
            now,
            &CheckResult::Ok {
                latency_ms: 1,
                tools_count: 1,
            },
            HealthInterval::FifteenMinutes,
            &zero,
        );
        assert_eq!(scheduler.states["x"].due_at_ms, now + 900_000);
    }

    #[test]
    fn visibility_pauses_and_staggers_resume() {
        let mut scheduler = HealthScheduler::new();
        let server = target("x", HealthTransport::Stdio, true);
        let zero = FixedJitter(0);
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true);
        scheduler.due_checks(0, policy, &[server], &zero);
        scheduler.set_visible(false, 10, 0);
        scheduler.set_visible(true, 10_000, 2_000);
        assert!(
            scheduler
                .due_checks(
                    300_000,
                    policy,
                    &[target("x", HealthTransport::Stdio, true)],
                    &zero
                )
                .is_empty()
        );
        assert_eq!(scheduler.states["x"].due_at_ms, 311_990);
        let mut long_hidden = HealthScheduler::new();
        let targets = [
            target("first", HealthTransport::Stdio, true),
            target("second", HealthTransport::Stdio, true),
        ];
        long_hidden.due_checks(0, policy, &targets, &zero);
        long_hidden.set_visible(false, 0, 0);
        assert!(
            long_hidden
                .due_checks(1_800_000, policy, &targets, &zero)
                .is_empty()
        );
        long_hidden.set_visible(true, 1_800_000, 2_000);
        assert!(
            long_hidden
                .due_checks(1_800_000, policy, &targets, &zero)
                .is_empty()
        );
        let resumed = long_hidden.due_checks(1_802_000, policy, &targets, &zero);
        assert_eq!(resumed.len(), 2);
    }

    #[test]
    fn overlapping_invocations_are_skipped_and_http_names_registered_action() {
        let mut scheduler = HealthScheduler::new();
        let server = target("http", HealthTransport::Http, true);
        let zero = FixedJitter(0);
        let policy = policy(HealthInterval::FiveMinutes, BackgroundHealth::All, true);
        scheduler.due_checks(0, policy, std::slice::from_ref(&server), &zero);
        assert!(
            scheduler
                .due_checks(300_000, policy, std::slice::from_ref(&server), &zero)
                .len()
                == 1
        );
        assert!(
            scheduler
                .due_checks(900_000, policy, std::slice::from_ref(&server), &zero)
                .is_empty()
        );
        assert_eq!(
            HealthScheduler::network_action(&server),
            Some(NetworkAction::ScheduledHttpHealthCheck)
        );
        assert_eq!(
            HealthScheduler::network_trigger(),
            NetworkTrigger::Background
        );
    }

    #[test]
    fn jitter_is_capped_at_thirty_seconds() {
        let mut scheduler = HealthScheduler::new();
        let server = target("x", HealthTransport::Stdio, true);
        scheduler.due_checks(
            0,
            policy(HealthInterval::FiveMinutes, BackgroundHealth::Local, true),
            &[server],
            &FixedJitter(90_000),
        );
        assert_eq!(scheduler.states["x"].due_at_ms, 330_000);
    }
}
