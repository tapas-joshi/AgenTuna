//! Async runtime bridge: one tokio runtime, cancellable jobs and typed webview events.
//!
//! The bridge owns a single multi-threaded tokio runtime created on first use and shut
//! down with the app. Long-running work (scans, git, MCP, hashing) is submitted as a
//! [`JobSpec`] and reports progress through a [`ProgressSender`] handed to the job
//! closure. One forwarder task drains a bounded channel and emits these webview events:
//!
//! | Event | Payload |
//! | --- | --- |
//! | [`EVENT_JOB_STARTED`] | [`JobStartedEvent`] |
//! | [`EVENT_JOB_PROGRESS`] | [`JobProgressEvent`], coalesced to at most 10 per second |
//! | [`EVENT_JOB_FINISHED`] | [`JobFinishedEvent`], exactly once per job |
//!
//! Jobs are cooperative: cancellation stops a job within one scheduler turn because the
//! runner selects on the cancel token. A panicking job is contained with
//! [`std::panic::catch_unwind`] and reported as [`JobError::Panic`].

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use ts_rs::TS;

/// Webview event emitted when a job starts running.
pub const EVENT_JOB_STARTED: &str = "agentuna:job-started";
/// Webview event emitted when a job reports progress (at most 10 times per second).
pub const EVENT_JOB_PROGRESS: &str = "agentuna:job-progress";
/// Webview event emitted exactly once when a job reaches a terminal state.
pub const EVENT_JOB_FINISHED: &str = "agentuna:job-finished";

/// Progress updates are coalesced to one emission per this interval.
const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Bounded progress channel; excess progress ticks are dropped, never queued unboundedly.
const PROGRESS_CHANNEL_CAPACITY: usize = 256;

/// Stable webview event names for the async bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum JobEventName {
    /// A job started.
    #[serde(rename = "agentuna:job-started")]
    Started,
    /// A job reported progress.
    #[serde(rename = "agentuna:job-progress")]
    Progress,
    /// A job reached a terminal state.
    #[serde(rename = "agentuna:job-finished")]
    Finished,
}

impl JobEventName {
    /// The event name used on the Tauri event channel.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => EVENT_JOB_STARTED,
            Self::Progress => EVENT_JOB_PROGRESS,
            Self::Finished => EVENT_JOB_FINISHED,
        }
    }
}

/// Error returned when a string is not a well-formed [`JobId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid job id: expected `job_` followed by 32 lowercase hex digits")]
pub struct JobIdError;

/// Identifier of one bridge job; it is also the operations-log key for the job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct JobId(String);

impl JobId {
    /// Allocates a new sortable job id.
    #[must_use]
    pub fn new() -> Self {
        Self(format!("job_{}", uuid::Uuid::now_v7().simple()))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for JobId {
    type Err = JobIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(suffix) = value.strip_prefix("job_") else {
            return Err(JobIdError);
        };
        if suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(JobIdError)
        }
    }
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum JobState {
    /// The job is executing.
    Running,
    /// The job finished successfully.
    Succeeded,
    /// The job returned an error or panicked.
    Failed,
    /// The job was cancelled.
    Cancelled,
    /// The app quit while the job was still running.
    Interrupted,
}

impl JobState {
    /// True for every state except [`JobState::Running`].
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Typed job failure delivered to the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum JobError {
    /// The job panicked; the message is the panic payload rendered as text.
    Panic {
        /// Panic text.
        message: String,
    },
    /// The job returned an ordinary failure.
    Failed {
        /// Failure text.
        message: String,
    },
}

impl JobError {
    /// The human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Panic { message } | Self::Failed { message } => message,
        }
    }
}

/// Payload of [`EVENT_JOB_STARTED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct JobStartedEvent {
    /// Job identifier.
    pub id: JobId,
    /// Human-readable job name.
    pub name: String,
}

/// Payload of [`EVENT_JOB_PROGRESS`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct JobProgressEvent {
    /// Job identifier.
    pub id: JobId,
    /// Human-readable job name.
    pub name: String,
    /// Completion fraction in `0.0..=1.0`, when the job reports one.
    pub fraction: Option<f64>,
    /// Progress text.
    pub label: String,
}

/// Payload of [`EVENT_JOB_FINISHED`]. Exactly one is emitted per job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct JobFinishedEvent {
    /// Job identifier.
    pub id: JobId,
    /// Human-readable job name.
    pub name: String,
    /// Terminal state: `succeeded`, `failed`, `cancelled` or `interrupted`.
    pub state: JobState,
    /// Failure details; present only when `state` is `failed`.
    pub error: Option<JobError>,
}

/// Snapshot of a running job, returned by the `jobs_list` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct JobSnapshot {
    /// Job identifier.
    pub id: JobId,
    /// Human-readable job name.
    pub name: String,
    /// Current state; snapshots only list running jobs.
    pub state: JobState,
    /// Unix epoch milliseconds when the job started.
    pub started_at_unix_ms: i64,
}

/// One typed job lifecycle event handed to an [`EventSink`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum JobEvent {
    /// A job started.
    Started(JobStartedEvent),
    /// A job reported progress.
    Progress(JobProgressEvent),
    /// A job reached a terminal state.
    Finished(JobFinishedEvent),
}

impl JobEvent {
    /// The webview event name for this event.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Started(_) => JobEventName::Started.as_str(),
            Self::Progress(_) => JobEventName::Progress.as_str(),
            Self::Finished(_) => JobEventName::Finished.as_str(),
        }
    }
}

/// Destination for typed job events: the Tauri app in production, a recorder in tests.
pub trait EventSink: Send + Sync + 'static {
    /// Emits exactly one event. Implementations must not block for long.
    fn emit(&self, event: JobEvent);
}

/// [`EventSink`] that forwards events to the webview through the Tauri event system.
pub struct TauriEventSink {
    app: tauri::AppHandle,
}

impl TauriEventSink {
    /// Creates a sink that emits events from `app`.
    #[must_use]
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit(&self, event: JobEvent) {
        let result = match event {
            JobEvent::Started(payload) => self.app.emit(EVENT_JOB_STARTED, payload),
            JobEvent::Progress(payload) => self.app.emit(EVENT_JOB_PROGRESS, payload),
            JobEvent::Finished(payload) => self.app.emit(EVENT_JOB_FINISHED, payload),
        };
        if let Err(error) = result {
            tracing::warn!(error = %error, "failed to emit job event to the webview");
        }
    }
}

/// A job that was still running when the app began shutting down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedJob {
    /// Job identifier; also the operations-log row key.
    pub id: JobId,
    /// Human-readable job name.
    pub name: String,
    /// Unix epoch milliseconds when the job started.
    pub started_at_unix_ms: i64,
}

/// Typed error returned by an [`OperationsLog`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("operations log write failed: {message}")]
pub struct OperationsLogError {
    /// Redacted description of the failure.
    pub message: String,
}

impl OperationsLogError {
    /// Creates an error with a redacted message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Durable operations log used by the bridge at shutdown.
///
/// The SQLite-backed implementation belongs to TF-009 (`at-db`). Until it is wired in,
/// [`TracingOperationsLog`] records the same row as a structured log entry. The bridge
/// is the only writer: it calls [`OperationsLog::record_interrupted`] exactly once per
/// job that was running when the app quit.
pub trait OperationsLog: Send + Sync + 'static {
    /// Records one `interrupted` operations row for a job that outlived shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsLogError`] when the row cannot be written. The bridge logs the
    /// failure and continues shutting down.
    fn record_interrupted(&self, job: &InterruptedJob) -> Result<(), OperationsLogError>;
}

/// Fallback [`OperationsLog`] that emits a structured `tracing` record instead of a row.
///
/// Replace it with the `at-db` implementation once TF-009 exposes its write API.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingOperationsLog;

impl OperationsLog for TracingOperationsLog {
    fn record_interrupted(&self, job: &InterruptedJob) -> Result<(), OperationsLogError> {
        tracing::warn!(
            operation_id = %job.id,
            job_name = %job.name,
            status = "interrupted",
            started_at_unix_ms = job.started_at_unix_ms,
            "operation interrupted by app shutdown"
        );
        Ok(())
    }
}

/// Error returned when the bridge cannot start a job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// The job name was empty.
    #[error("job name must not be empty")]
    InvalidName,
    /// The bridge is shutting down and accepts no new jobs.
    #[error("the job bridge is shutting down")]
    ShuttingDown,
    /// The tokio runtime could not be created.
    #[error("failed to start the job runtime: {message}")]
    Runtime {
        /// OS error text.
        message: String,
    },
    /// The runtime exists but its channel is gone.
    #[error("the job bridge is not running")]
    NotStarted,
}

/// Cooperative cancellation token shared by the handle and the job runner.
#[derive(Debug, Clone)]
pub struct CancelToken {
    sender: watch::Sender<bool>,
}

impl CancelToken {
    /// Creates a token that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self { sender }
    }

    /// Requests cancellation and wakes the runner.
    pub fn cancel(&self) {
        // `send` refuses when no receiver exists; the store must still flip so that a
        // runner which subscribes later observes the cancellation.
        self.sender.send_replace(true);
    }

    /// True once [`CancelToken::cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    /// Resolves when the token is cancelled.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut receiver = self.sender.subscribe();
        if self.is_cancelled() {
            return;
        }
        let _ = receiver.changed().await;
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle used by jobs to report progress.
#[derive(Debug, Clone)]
pub struct ProgressSender {
    id: JobId,
    tx: mpsc::Sender<BridgeMessage>,
}

impl ProgressSender {
    /// The job this sender belongs to.
    #[must_use]
    pub fn id(&self) -> &JobId {
        &self.id
    }

    /// Reports progress without a completion fraction.
    pub fn message(&self, label: impl Into<String>) {
        self.send(None, label.into());
    }

    /// Reports a completion fraction, clamped to `0.0..=1.0`, and a label.
    pub fn fraction(&self, fraction: f64, label: impl Into<String>) {
        let fraction = if fraction.is_finite() {
            Some(fraction.clamp(0.0, 1.0))
        } else {
            None
        };
        self.send(fraction, label.into());
    }

    fn send(&self, fraction: Option<f64>, label: String) {
        // Progress is lossy by design: a full channel drops ticks and the next one wins.
        let _ = self.tx.try_send(BridgeMessage::Progress {
            id: self.id.clone(),
            fraction,
            label,
        });
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send + 'static>>;

/// Description of one unit of background work.
pub struct JobSpec {
    name: String,
    work: Box<dyn FnOnce(ProgressSender) -> BoxFuture + Send + 'static>,
}

impl JobSpec {
    /// Creates a job. `work` receives the progress sender for this job.
    pub fn new<W, F>(name: impl Into<String>, work: W) -> Self
    where
        W: FnOnce(ProgressSender) -> F + Send + 'static,
        F: Future<Output = Result<(), JobError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            work: Box::new(move |progress| Box::pin(work(progress))),
        }
    }

    /// The human-readable job name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Cloneable front-end handle for a running job.
#[derive(Clone)]
pub struct JobHandle {
    id: JobId,
    name: String,
    started_at_unix_ms: i64,
    token: CancelToken,
    progress: ProgressSender,
    shared: Arc<Shared>,
}

impl JobHandle {
    /// The job identifier.
    #[must_use]
    pub fn id(&self) -> &JobId {
        &self.id
    }

    /// The human-readable job name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The progress sender handed to the job.
    #[must_use]
    pub fn progress(&self) -> &ProgressSender {
        &self.progress
    }

    /// A snapshot of this job while it runs.
    #[must_use]
    pub fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            state: JobState::Running,
            started_at_unix_ms: self.started_at_unix_ms,
        }
    }

    /// Requests cancellation and emits the `cancelled` finished event immediately.
    ///
    /// Any result produced later by the runner is dropped because the job id is no
    /// longer in the registry.
    pub fn cancel(&self) {
        self.shared.cancel(&self.id);
    }

    /// True once cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Result of [`JobBridge::shutdown`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Jobs that were still running when the grace period ended.
    pub interrupted: usize,
}

enum BridgeMessage {
    Progress {
        id: JobId,
        fraction: Option<f64>,
        label: String,
    },
    Finished {
        id: JobId,
        name: String,
        state: JobState,
        error: Option<JobError>,
    },
}

struct JobEntry {
    name: String,
    started_at_unix_ms: i64,
    token: CancelToken,
    state: JobState,
}

struct Shared {
    sink: Arc<dyn EventSink>,
    operations: Arc<dyn OperationsLog>,
    jobs: Mutex<HashMap<JobId, JobEntry>>,
    jobs_changed: Condvar,
    shutting_down: AtomicBool,
}

impl Shared {
    /// Removes the job and emits its single terminal `cancelled` event.
    fn cancel(&self, id: &JobId) -> bool {
        let entry = {
            let mut jobs = lock(&self.jobs);
            jobs.remove(id)
        };
        let Some(entry) = entry else {
            return false;
        };
        entry.token.cancel();
        self.jobs_changed.notify_all();
        if !self.shutting_down.load(Ordering::Acquire) {
            self.sink.emit(JobEvent::Finished(JobFinishedEvent {
                id: id.clone(),
                name: entry.name,
                state: JobState::Cancelled,
                error: None,
            }));
        }
        true
    }
}

struct Engine {
    runtime: Option<Runtime>,
    forwarder: Option<JoinHandle<()>>,
    progress_tx: Option<mpsc::Sender<BridgeMessage>>,
}

/// Owns the application tokio runtime and the job registry.
pub struct JobBridge {
    shared: Arc<Shared>,
    engine: Mutex<Engine>,
}

impl JobBridge {
    /// Creates a bridge. The runtime and forwarder start on the first [`JobBridge::spawn_job`].
    #[must_use]
    pub fn new(sink: Arc<dyn EventSink>, operations: Arc<dyn OperationsLog>) -> Self {
        Self {
            shared: Arc::new(Shared {
                sink,
                operations,
                jobs: Mutex::new(HashMap::new()),
                jobs_changed: Condvar::new(),
                shutting_down: AtomicBool::new(false),
            }),
            engine: Mutex::new(Engine {
                runtime: None,
                forwarder: None,
                progress_tx: None,
            }),
        }
    }

    /// Starts a job and returns its handle.
    ///
    /// # Errors
    ///
    /// [`BridgeError`] when the name is empty, the bridge is shutting down, or the
    /// runtime cannot be created.
    pub fn spawn_job(&self, spec: JobSpec) -> Result<JobHandle, BridgeError> {
        let name = spec.name.trim().to_owned();
        if name.is_empty() {
            return Err(BridgeError::InvalidName);
        }
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(BridgeError::ShuttingDown);
        }

        let mut engine = lock(&self.engine);
        ensure_started(&mut engine, &self.shared)?;
        let (tx, runtime) = match (engine.progress_tx.as_ref(), engine.runtime.as_ref()) {
            (Some(tx), Some(runtime)) => (tx.clone(), runtime),
            _ => return Err(BridgeError::NotStarted),
        };

        let id = JobId::new();
        let token = CancelToken::new();
        let started_at_unix_ms = now_unix_ms();
        {
            let mut jobs = lock(&self.shared.jobs);
            jobs.insert(
                id.clone(),
                JobEntry {
                    name: name.clone(),
                    started_at_unix_ms,
                    token: token.clone(),
                    state: JobState::Running,
                },
            );
        }

        self.shared.sink.emit(JobEvent::Started(JobStartedEvent {
            id: id.clone(),
            name: name.clone(),
        }));

        let progress = ProgressSender {
            id: id.clone(),
            tx: tx.clone(),
        };
        let runner = JobRunner {
            id: id.clone(),
            name: name.clone(),
            token: token.clone(),
            work: (spec.work)(progress.clone()),
            tx,
        };
        runtime.spawn(runner.run());

        Ok(JobHandle {
            id,
            name,
            started_at_unix_ms,
            token,
            progress,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Snapshots every running job, oldest first.
    #[must_use]
    pub fn jobs(&self) -> Vec<JobSnapshot> {
        let jobs = lock(&self.shared.jobs);
        let mut snapshots: Vec<JobSnapshot> = jobs
            .iter()
            .map(|(id, entry)| JobSnapshot {
                id: id.clone(),
                name: entry.name.clone(),
                state: entry.state,
                started_at_unix_ms: entry.started_at_unix_ms,
            })
            .collect();
        snapshots.sort_by(|left, right| {
            left.started_at_unix_ms
                .cmp(&right.started_at_unix_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        snapshots
    }

    /// Cancels a running job by id. Returns `false` when no such job is running.
    pub fn cancel_job(&self, id: &JobId) -> bool {
        self.shared.cancel(id)
    }

    /// Cancels every job, waits up to `grace` for the runners to stop, records one
    /// `interrupted` operations row per job that was still running, then stops the
    /// runtime. Calling it more than once is a no-op.
    pub fn shutdown(&self, grace: Duration) -> ShutdownReport {
        if self.shared.shutting_down.swap(true, Ordering::SeqCst) {
            return ShutdownReport::default();
        }
        let deadline = Instant::now() + grace;

        let pending: Vec<(JobId, JobEntry)> = {
            let jobs = lock(&self.shared.jobs);
            jobs.iter()
                .map(|(id, entry)| {
                    (
                        id.clone(),
                        JobEntry {
                            name: entry.name.clone(),
                            started_at_unix_ms: entry.started_at_unix_ms,
                            token: entry.token.clone(),
                            state: entry.state,
                        },
                    )
                })
                .collect()
        };
        for (_, entry) in &pending {
            entry.token.cancel();
        }

        {
            let mut jobs = lock(&self.shared.jobs);
            while !jobs.is_empty() {
                let now = Instant::now();
                let Some(remaining) = deadline.checked_duration_since(now) else {
                    break;
                };
                let (guard, _) = self
                    .shared
                    .jobs_changed
                    .wait_timeout(jobs, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                jobs = guard;
            }
        }

        let mut interrupted = Vec::new();
        {
            let mut jobs = lock(&self.shared.jobs);
            for (id, entry) in pending {
                jobs.remove(&id);
                interrupted.push(InterruptedJob {
                    id,
                    name: entry.name,
                    started_at_unix_ms: entry.started_at_unix_ms,
                });
            }
            // Anything spawned before the shutdown flag became visible is also unfinished.
            for (id, entry) in jobs.drain() {
                interrupted.push(InterruptedJob {
                    id,
                    name: entry.name,
                    started_at_unix_ms: entry.started_at_unix_ms,
                });
            }
            self.shared.jobs_changed.notify_all();
        }

        for job in &interrupted {
            self.shared.sink.emit(JobEvent::Finished(JobFinishedEvent {
                id: job.id.clone(),
                name: job.name.clone(),
                state: JobState::Interrupted,
                error: None,
            }));
            if let Err(error) = self.shared.operations.record_interrupted(job) {
                tracing::error!(operation_id = %job.id, error = %error, "failed to record interrupted operation");
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        {
            let mut engine = lock(&self.engine);
            engine.progress_tx = None;
            if let Some(forwarder) = engine.forwarder.take() {
                forwarder.abort();
            }
            if let Some(runtime) = engine.runtime.take() {
                runtime.shutdown_timeout(remaining);
            }
        }

        ShutdownReport {
            interrupted: interrupted.len(),
        }
    }
}

impl Drop for JobBridge {
    fn drop(&mut self) {
        // A dropped bridge must not leak worker threads or running jobs.
        self.shutdown(Duration::ZERO);
    }
}

struct JobRunner {
    id: JobId,
    name: String,
    token: CancelToken,
    work: BoxFuture,
    tx: mpsc::Sender<BridgeMessage>,
}

impl JobRunner {
    async fn run(self) {
        let cancelled = self.token.clone();
        let result = tokio::select! {
            biased;
            () = cancelled.cancelled() => JobResult::Cancelled,
            result = guard_future(self.work) => match result {
                Ok(Ok(())) => JobResult::Succeeded,
                Ok(Err(error)) => JobResult::Failed(error),
                Err(payload) => {
                    let message = panic_message(payload);
                    tracing::error!(
                        operation_id = %self.id,
                        job_name = %self.name,
                        panic = %message,
                        "job panicked"
                    );
                    JobResult::Failed(JobError::Panic { message })
                }
            },
        };

        let (state, error) = match result {
            JobResult::Succeeded => (JobState::Succeeded, None),
            JobResult::Failed(error) => (JobState::Failed, Some(error)),
            JobResult::Cancelled => (JobState::Cancelled, None),
        };
        let _ = self
            .tx
            .send(BridgeMessage::Finished {
                id: self.id,
                name: self.name,
                state,
                error,
            })
            .await;
    }
}

enum JobResult {
    Succeeded,
    Failed(JobError),
    Cancelled,
}

fn ensure_started(engine: &mut Engine, shared: &Arc<Shared>) -> Result<(), BridgeError> {
    if engine.runtime.is_some() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("agentuna-jobs")
        .build()
        .map_err(|error| BridgeError::Runtime {
            message: error.to_string(),
        })?;
    let (tx, rx) = mpsc::channel(PROGRESS_CHANNEL_CAPACITY);
    let forwarder = runtime.spawn(forward_events(rx, Arc::clone(shared)));
    engine.runtime = Some(runtime);
    engine.progress_tx = Some(tx);
    engine.forwarder = Some(forwarder);
    Ok(())
}

async fn forward_events(mut rx: mpsc::Receiver<BridgeMessage>, shared: Arc<Shared>) {
    let mut pending: HashMap<JobId, (Option<f64>, String)> = HashMap::new();
    let mut next_flush = tokio::time::Instant::now() + PROGRESS_FLUSH_INTERVAL;
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(next_flush) => {
                flush_progress(&shared, &mut pending);
                // Space emissions at least one interval apart so a delayed flush cannot
                // catch up in a burst and exceed the 10-per-second promise.
                next_flush = tokio::time::Instant::now() + PROGRESS_FLUSH_INTERVAL;
            }
            message = rx.recv() => {
                match message {
                    None => break,
                    Some(BridgeMessage::Progress { id, fraction, label }) => {
                        pending.insert(id, (fraction, label));
                    }
                    Some(BridgeMessage::Finished { id, name, state, error }) => {
                        pending.remove(&id);
                        let removed = {
                            let mut jobs = lock(&shared.jobs);
                            let removed = jobs.remove(&id).is_some();
                            if removed {
                                shared.jobs_changed.notify_all();
                            }
                            removed
                        };
                        if removed && !shared.shutting_down.load(Ordering::Acquire) {
                            shared.sink.emit(JobEvent::Finished(JobFinishedEvent { id, name, state, error }));
                        }
                    }
                }
            }
        }
    }
}

fn flush_progress(shared: &Shared, pending: &mut HashMap<JobId, (Option<f64>, String)>) {
    if pending.is_empty() {
        return;
    }
    let batch: Vec<(JobId, String, Option<f64>, String)> = {
        let jobs = lock(&shared.jobs);
        pending
            .drain()
            .filter_map(|(id, (fraction, label))| {
                jobs.get(&id)
                    .map(|entry| (id, entry.name.clone(), fraction, label))
            })
            .collect()
    };
    for (id, name, fraction, label) in batch {
        shared.sink.emit(JobEvent::Progress(JobProgressEvent {
            id,
            name,
            fraction,
            label,
        }));
    }
}

async fn guard_future(
    mut work: BoxFuture,
) -> Result<Result<(), JobError>, Box<dyn Any + Send + 'static>> {
    poll_fn(move |cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    })
    .await
}

fn panic_message(payload: Box<dyn Any + Send + 'static>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "job panicked with a non-string payload".to_owned()
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
