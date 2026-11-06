//! Debounced filesystem watching for the stores and for agent configuration paths.
//!
//! [`WatchService`] watches the user store, the active project store and every agent path the
//! application detected (skills directories, MCP files and rules files), coalesces the events
//! into [`WatchBatch`]es and delivers them through a bounded queue.
//!
//! The caller decides *what* to watch: this module never resolves agent paths itself. Build
//! [`WatchRoot`]s from [`AppPaths`](../../at_store/paths/struct.AppPaths.html) and from the
//! adapters' `agent_paths` result, then hand them to [`WatchConfig`]. That keeps the crate free
//! of adapter knowledge and lets the application watch whatever it currently considers active.
//!
//! # Behaviour that callers can rely on
//!
//! * One debounce window ([`DEFAULT_DEBOUNCE`], 300 ms) per burst of changes: fifty writes to the
//!   same file in quick succession produce one batch.
//! * Self-writes are suppressed: record every [`WriteOutcome`] in the [`RecentWrites`] registry
//!   shared with the service and the events caused by those writes never reach a batch. The
//!   suppression re-reads the file, so an external write that lands inside the suppression
//!   window is still reported.
//! * Build artefacts and editor litter are ignored: `.git/`, `node_modules/`, `*.agentuna-tmp-*`
//!   and `*.swp` never appear in a batch. A watched store root also hides its own `cache/` and
//!   `backups/` subtrees, so store scratch never looks like a user change.
//! * Renames are reported once, as [`WatchChange::Renamed`] carrying the old and the new path.
//! * A burst too large to describe (more than [`DEFAULT_MAX_BATCH_PATHS`] paths) collapses into a
//!   single [`WatchEvent::RescanRequired`]; a full queue does the same. The queue never exceeds
//!   its capacity, so memory stays bounded.
//! * A watched path that disappears yields [`WatchEvent::PathGone`]; the watch moves to the
//!   nearest existing ancestor and the subtree is watched again once the path comes back.
//!
//! # Platform notes
//!
//! macOS FSEvents reports a rename as one event per side and does not pair them, and it reports a
//! removed subtree as a single event for the directory. This module therefore normalises events
//! against the filesystem (and against the inode index it keeps) instead of trusting the event
//! kind, so the batches describe what is true after the burst rather than what the platform
//! happened to report.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use time::OffsetDateTime;

use crate::atomic::WriteOutcome;
use crate::hash::sha256_hex;

/// Debounce window applied to a burst of changes.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// How long a recorded write keeps suppressing the events it caused.
pub const DEFAULT_SUPPRESS_TTL: Duration = Duration::from_secs(2);

/// Capacity of the batch queue.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;

/// Largest number of paths a single batch may carry before it collapses into a rescan.
pub const DEFAULT_MAX_BATCH_PATHS: usize = 512;

/// How long a burst has to be quiet before events are reported again after a rescan.
pub const DEFAULT_BURST_QUIET: Duration = Duration::from_secs(1);

/// How long [`WatchService::stop`] waits for the worker thread to report completion.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(1);

/// How long [`WatchService::start`] waits for the watcher to be installed.
pub const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on the inode index used to recognise renames.
const PATH_INDEX_CAPACITY: usize = 100_000;

/// How often the worker wakes without events to notice a stopped service or a returning root.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Debouncer type used by the service.
type FsDebouncer = Debouncer<RecommendedWatcher, RecommendedCache>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a [`WatchRoot`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WatchRootKind {
    /// The per-user store (`~/.agentuna`).
    UserStore,
    /// The active project store (`<project>/.agentuna`).
    ProjectStore,
    /// The machine-wide Global store.
    GlobalStore,
    /// An agent's skills directory.
    SkillsDir,
    /// An agent's MCP configuration file.
    McpFile,
    /// An agent's rules file (for example `CLAUDE.md`).
    RulesFile,
    /// Anything else the caller wants to watch.
    Other,
}

impl WatchRootKind {
    /// Whether this kind names a directory rather than a file.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(
            self,
            Self::UserStore | Self::ProjectStore | Self::GlobalStore | Self::SkillsDir
        )
    }

    /// Stable identifier for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserStore => "user_store",
            Self::ProjectStore => "project_store",
            Self::GlobalStore => "global_store",
            Self::SkillsDir => "skills_dir",
            Self::McpFile => "mcp_file",
            Self::RulesFile => "rules_file",
            Self::Other => "other",
        }
    }
}

/// One path the service watches, and why.
///
/// Directory roots are watched recursively. File roots are watched through their parent
/// directory, because agent configuration files are replaced atomically and the resulting inode
/// replacement would otherwise drop a watch on the file itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchRoot {
    /// Absolute path of the store, directory or file.
    pub path: PathBuf,
    /// What the path represents.
    pub kind: WatchRootKind,
    /// Whether a directory root is watched recursively.
    pub recursive: bool,
}

impl WatchRoot {
    /// A root whose recursion follows the kind (directories recursive, files not).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, kind: WatchRootKind) -> Self {
        Self {
            path: path.into(),
            kind,
            recursive: kind.is_directory(),
        }
    }

    /// A directory root.
    #[must_use]
    pub fn dir(path: impl Into<PathBuf>, kind: WatchRootKind) -> Self {
        Self {
            path: path.into(),
            kind,
            recursive: true,
        }
    }

    /// A file root.
    #[must_use]
    pub fn file(path: impl Into<PathBuf>, kind: WatchRootKind) -> Self {
        Self {
            path: path.into(),
            kind,
            recursive: false,
        }
    }

    /// Overrides the recursion flag.
    #[must_use]
    pub fn with_recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }
}

/// Directories, names and suffixes that never reach a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreRules {
    excluded: Vec<PathBuf>,
    names: Vec<String>,
    pairs: Vec<(String, String)>,
    suffixes: Vec<String>,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self::standard()
    }
}

impl IgnoreRules {
    /// Build output, editor litter and AgenTuna's own staging files.
    ///
    /// Store scratch directories are added by the service itself from the watched store roots,
    /// because only the store layer knows where the store lives.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            excluded: Vec::new(),
            names: vec![".git".to_owned(), "node_modules".to_owned()],
            pairs: Vec::new(),
            suffixes: vec![".agentuna-tmp-".to_owned(), ".swp".to_owned()],
        }
    }

    /// Ignores everything below `dir` (for example the absolute backup and cache directories).
    pub fn exclude_tree(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.excluded.push(dir.into());
        self
    }

    /// Ignores a directory or file name at any depth.
    pub fn exclude_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.names.push(name.into());
        self
    }

    /// Ignores `child` when it sits directly inside `parent` (for example `.agentuna` / `cache`).
    pub fn exclude_pair(
        &mut self,
        parent: impl Into<String>,
        child: impl Into<String>,
    ) -> &mut Self {
        self.pairs.push((parent.into(), child.into()));
        self
    }

    /// Ignores a file whose name contains `suffix` (for example `.swp`).
    pub fn exclude_suffix(&mut self, suffix: impl Into<String>) -> &mut Self {
        self.suffixes.push(suffix.into());
        self
    }

    /// Whether `path` is ignored.
    #[must_use]
    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.excluded.iter().any(|dir| path.starts_with(dir)) {
            return true;
        }
        let mut previous: Option<&str> = None;
        for component in path.components() {
            let std::path::Component::Normal(part) = component else {
                previous = None;
                continue;
            };
            let Some(part) = part.to_str() else {
                previous = None;
                continue;
            };
            if self.names.iter().any(|name| name == part) {
                return true;
            }
            if let Some(parent) = previous
                && self
                    .pairs
                    .iter()
                    .any(|(pair_parent, child)| pair_parent == parent && child == part)
            {
                return true;
            }
            previous = Some(part);
        }
        match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => self
                .suffixes
                .iter()
                .any(|suffix| name.contains(suffix.as_str())),
            None => false,
        }
    }
}

/// Registry of writes AgenTuna performed itself.
///
/// Share one registry between the write path and the watch service:
/// record every [`WriteOutcome`] with [`Self::record_outcome`] and the service suppresses the
/// events that write caused. An entry stops suppressing when the file no longer holds the
/// recorded content (someone else wrote it) or when [`Self::ttl`] elapses.
#[derive(Debug)]
pub struct RecentWrites {
    ttl: Duration,
    entries: Mutex<HashMap<PathBuf, Vec<Recorded>>>,
    suppressed: AtomicU64,
}

#[derive(Debug, Clone)]
struct Recorded {
    after_hash: String,
    at: Instant,
}

impl RecentWrites {
    /// A registry whose entries live for [`DEFAULT_SUPPRESS_TTL`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_SUPPRESS_TTL)
    }

    /// A registry whose entries live for `ttl`.
    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
            suppressed: AtomicU64::new(0),
        }
    }

    /// How long an entry keeps suppressing events.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Records a write that is happening now.
    pub fn record(&self, path: impl Into<PathBuf>, after_hash: impl Into<String>) {
        self.record_at(path, after_hash, Instant::now());
    }

    /// Records a write observed at `at`.
    ///
    /// Tests use the explicit instant to exercise the TTL without sleeping.
    pub fn record_at(&self, path: impl Into<PathBuf>, after_hash: impl Into<String>, at: Instant) {
        let mut entries = lock(&self.entries);
        let recorded = entries.entry(path.into()).or_default();
        recorded.push(Recorded {
            after_hash: after_hash.into(),
            at,
        });
        // Keep at most a handful of records per path; only the newest can match the file.
        if recorded.len() > 8 {
            let excess = recorded.len() - 8;
            recorded.drain(..excess);
        }
    }

    /// Records the outcome of a write performed through [`crate::apply`].
    pub fn record_outcome(&self, path: &Path, outcome: &WriteOutcome) {
        self.record(path.to_path_buf(), outcome.after_hash.clone());
    }

    /// Whether the event for `path` was caused by a recorded write.
    ///
    /// Increments [`Self::suppressed_count`] when it returns `true`.
    #[must_use]
    pub fn should_suppress(&self, path: &Path) -> bool {
        self.should_suppress_at(path, Instant::now())
    }

    /// [`Self::should_suppress`] evaluated at `now`.
    #[must_use]
    pub fn should_suppress_at(&self, path: &Path, now: Instant) -> bool {
        let recorded = {
            let mut entries = lock(&self.entries);
            let Some(recorded) = entries.get_mut(path) else {
                return false;
            };
            recorded.retain(|entry| now.saturating_duration_since(entry.at) <= self.ttl);
            if recorded.is_empty() {
                entries.remove(path);
                return false;
            }
            recorded.clone()
        };
        let Ok(bytes) = fs::read(path) else {
            return false;
        };
        let hash = sha256_hex(&bytes);
        if recorded.iter().any(|entry| entry.after_hash == hash) {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Number of events suppressed since this registry was created.
    #[must_use]
    pub fn suppressed_count(&self) -> u64 {
        self.suppressed.load(Ordering::Relaxed)
    }

    /// Number of paths currently recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    /// Whether nothing is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Forgets every recorded write.
    pub fn clear(&self) {
        lock(&self.entries).clear();
    }
}

impl Default for RecentWrites {
    fn default() -> Self {
        Self::new()
    }
}

/// One change inside a [`WatchBatch`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum WatchChange {
    /// The path did not exist before this batch.
    Created(PathBuf),
    /// The path existed and now holds new content or metadata.
    Modified(PathBuf),
    /// The path existed and is gone.
    Removed(PathBuf),
    /// The path moved; the old location is gone and the new one holds the same inode.
    Renamed {
        /// Location before the rename.
        from: PathBuf,
        /// Location after the rename.
        to: PathBuf,
    },
}

impl WatchChange {
    /// The path the change is about, or the new location for a rename.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Created(path) | Self::Modified(path) | Self::Removed(path) => path,
            Self::Renamed { to, .. } => to,
        }
    }
}

/// A coalesced group of changes observed within one debounce window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchBatch {
    /// Every affected path, sorted; a rename contributes both sides.
    pub paths: Vec<PathBuf>,
    /// The changes, sorted and deduplicated.
    pub kinds: Vec<WatchChange>,
    /// When the batch was produced.
    pub observed_at: OffsetDateTime,
}

impl WatchBatch {
    fn build(mut kinds: Vec<WatchChange>, observed_at: OffsetDateTime) -> Self {
        kinds.sort();
        kinds.dedup();
        let mut paths = Vec::with_capacity(kinds.len());
        for kind in &kinds {
            match kind {
                WatchChange::Created(path)
                | WatchChange::Modified(path)
                | WatchChange::Removed(path) => paths.push(path.clone()),
                WatchChange::Renamed { from, to } => {
                    paths.push(from.clone());
                    paths.push(to.clone());
                }
            }
        }
        paths.sort();
        paths.dedup();
        Self {
            paths,
            kinds,
            observed_at,
        }
    }

    /// Whether the batch carries no changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Number of changes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether any change refers to `path`.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|candidate| candidate == path)
    }

    /// Every rename in the batch as `(from, to)` pairs.
    #[must_use]
    pub fn renames(&self) -> Vec<(&Path, &Path)> {
        self.kinds
            .iter()
            .filter_map(|kind| match kind {
                WatchChange::Renamed { from, to } => Some((from.as_path(), to.as_path())),
                _ => None,
            })
            .collect()
    }
}

/// Anything the service delivers to its consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatchEvent {
    /// A coalesced group of changes.
    Batch(WatchBatch),
    /// Too much changed at once; the consumer must rescan its sources.
    RescanRequired {
        /// When the service decided a rescan was needed.
        observed_at: OffsetDateTime,
    },
    /// A watched path disappeared. The service keeps running and watches the nearest existing
    /// ancestor until the path returns.
    PathGone {
        /// The watched path that disappeared.
        path: PathBuf,
        /// When the disappearance was noticed.
        observed_at: OffsetDateTime,
    },
}

impl WatchEvent {
    /// The batch, when this is a batch.
    #[must_use]
    pub fn batch(&self) -> Option<&WatchBatch> {
        match self {
            Self::Batch(batch) => Some(batch),
            _ => None,
        }
    }

    /// When the event was produced.
    #[must_use]
    pub fn observed_at(&self) -> OffsetDateTime {
        match self {
            Self::Batch(batch) => batch.observed_at,
            Self::RescanRequired { observed_at } | Self::PathGone { observed_at, .. } => {
                *observed_at
            }
        }
    }
}

/// Counters describing what the service has done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchStats {
    /// Batches queued for the consumer.
    pub batches: u64,
    /// Times a rescan was requested.
    pub rescan_required: u64,
    /// Watched paths that disappeared.
    pub path_gone: u64,
    /// Events suppressed because AgenTuna wrote the file itself.
    pub suppressed: u64,
    /// Batches or events dropped because the queue was full.
    pub overflow: u64,
    /// Watcher errors reported by the platform.
    pub errors: u64,
    /// Events dropped because they were not relevant to any watched root.
    pub ignored: u64,
}

/// Why the service could not start or stop.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WatchError {
    /// [`WatchService::start`] was called on a running service.
    #[error("the watch service is already running")]
    AlreadyRunning,
    /// The platform watcher could not be created.
    #[error("the platform watcher could not be created: {0}")]
    Watcher(#[source] notify::Error),
    /// A watch could not be installed on a path.
    #[error("{}: cannot watch: {source}", .path.display())]
    WatchFailed {
        /// Path that could not be watched.
        path: PathBuf,
        /// Platform error.
        #[source]
        source: notify::Error,
    },
    /// The worker thread could not be spawned.
    #[error("the watch worker could not be started: {0}")]
    Spawn(#[source] io::Error),
    /// The watcher did not report readiness in time.
    #[error("the watch service did not start within {DEFAULT_START_TIMEOUT:?}")]
    StartTimeout,
    /// The worker ended before the service was ready.
    #[error("the watch worker ended before it was ready")]
    WorkerGone,
    /// The worker panicked.
    #[error("the watch worker panicked")]
    WorkerPanicked,
}

/// Everything the caller must decide before the service starts.
#[derive(Debug)]
pub struct WatchConfig {
    /// Paths to watch, built from the app paths and the detected agent paths.
    pub roots: Vec<WatchRoot>,
    /// Debounce window.
    pub debounce: Duration,
    /// Capacity of the batch queue; one slot is reserved for a rescan notice.
    pub capacity: usize,
    /// Largest number of paths a batch may carry before it collapses into a rescan.
    pub max_batch_paths: usize,
    /// How long a burst must be quiet before events flow again after a rescan.
    pub burst_quiet: Duration,
    /// Paths, names and suffixes that never reach a batch.
    pub ignored: IgnoreRules,
    /// Registry of AgenTuna's own writes.
    pub registry: Arc<RecentWrites>,
}

impl WatchConfig {
    /// A configuration with the product defaults.
    #[must_use]
    pub fn new(roots: impl IntoIterator<Item = WatchRoot>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
            debounce: DEFAULT_DEBOUNCE,
            capacity: DEFAULT_QUEUE_CAPACITY,
            max_batch_paths: DEFAULT_MAX_BATCH_PATHS,
            burst_quiet: DEFAULT_BURST_QUIET,
            ignored: IgnoreRules::standard(),
            registry: Arc::new(RecentWrites::new()),
        }
    }

    /// Replaces the debounce window.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Replaces the queue capacity (clamped to at least two).
    #[must_use]
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity.max(2);
        self
    }

    /// Replaces the burst threshold; `0` disables burst detection.
    #[must_use]
    pub fn with_max_batch_paths(mut self, max_batch_paths: usize) -> Self {
        self.max_batch_paths = max_batch_paths;
        self
    }

    /// Replaces how long a burst must be quiet before events flow again.
    #[must_use]
    pub fn with_burst_quiet(mut self, burst_quiet: Duration) -> Self {
        self.burst_quiet = burst_quiet;
        self
    }

    /// Replaces the ignore rules.
    #[must_use]
    pub fn with_ignored(mut self, ignored: IgnoreRules) -> Self {
        self.ignored = ignored;
        self
    }

    /// Shares a registry with the code that performs writes.
    #[must_use]
    pub fn with_registry(mut self, registry: Arc<RecentWrites>) -> Self {
        self.registry = registry;
        self
    }
}

/// Watch service: one worker thread, one debouncer, one bounded queue.
#[derive(Debug)]
pub struct WatchService {
    config: Arc<WatchConfig>,
    shared: Arc<Shared>,
    run: Mutex<RunState>,
}

#[derive(Debug, Default)]
struct RunState {
    handle: Option<JoinHandle<()>>,
    done: Option<Receiver<()>>,
}

impl WatchService {
    /// Builds a service. Nothing is watched until [`Self::start`].
    #[must_use]
    pub fn new(config: WatchConfig) -> Self {
        let capacity = config.capacity.max(2);
        let registry = Arc::clone(&config.registry);
        Self {
            config: Arc::new(config),
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                condvar: Condvar::new(),
                stop: AtomicBool::new(false),
                capacity,
                counters: Counters::default(),
                registry,
            }),
            run: Mutex::new(RunState::default()),
        }
    }

    /// The registry shared with the write path.
    #[must_use]
    pub fn registry(&self) -> &Arc<RecentWrites> {
        &self.config.registry
    }

    /// The paths this service was configured with.
    #[must_use]
    pub fn roots(&self) -> &[WatchRoot] {
        &self.config.roots
    }

    /// Whether the worker thread is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        lock(&self.run).handle.is_some()
    }

    /// Installs the watches and starts the worker.
    ///
    /// # Errors
    /// [`WatchError::AlreadyRunning`] when the service is already started;
    /// [`WatchError::Watcher`], [`WatchError::WatchFailed`], [`WatchError::Spawn`] and
    /// [`WatchError::StartTimeout`] when the platform refuses to watch the configured roots.
    pub fn start(&self) -> Result<(), WatchError> {
        let mut run = lock(&self.run);
        if run.handle.is_some() {
            return Err(WatchError::AlreadyRunning);
        }
        self.shared.stop.store(false, Ordering::SeqCst);
        {
            let mut queue = lock(&self.shared.queue);
            queue.events.clear();
            queue.rescan_queued = false;
        }
        let shared = Arc::clone(&self.shared);
        let config = Arc::clone(&self.config);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("agentuna-watch".to_owned())
            .spawn(move || run_worker(&shared, &config, &ready_tx, &done_tx))
            .map_err(WatchError::Spawn)?;
        match ready_rx.recv_timeout(DEFAULT_START_TIMEOUT) {
            Ok(Ok(())) => {
                run.handle = Some(handle);
                run.done = Some(done_rx);
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.shared.stop.store(true, Ordering::SeqCst);
                let _ = handle.join();
                Err(WatchError::StartTimeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                Err(WatchError::WorkerGone)
            }
        }
    }

    /// Stops the worker and releases the platform watcher.
    ///
    /// Stopping a service that is not running is not an error.
    ///
    /// # Errors
    /// [`WatchError::WorkerPanicked`] when the worker thread panicked.
    pub fn stop(&self) -> Result<(), WatchError> {
        let (handle, done) = {
            let mut run = lock(&self.run);
            let handle = run.handle.take();
            let done = run.done.take();
            self.shared.stop.store(true, Ordering::SeqCst);
            (handle, done)
        };
        let Some(handle) = handle else {
            return Ok(());
        };
        if let Some(done) = done {
            let _ = done.recv_timeout(DEFAULT_STOP_TIMEOUT);
        }
        match handle.join() {
            Ok(()) => Ok(()),
            Err(_) => Err(WatchError::WorkerPanicked),
        }
    }

    /// Number of events waiting for the consumer.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        lock(&self.shared.queue).events.len()
    }

    /// Pops the oldest event, if any.
    #[must_use]
    pub fn try_next(&self) -> Option<WatchEvent> {
        let mut queue = lock(&self.shared.queue);
        let event = queue.events.pop_front()?;
        if matches!(event, WatchEvent::RescanRequired { .. }) {
            queue.rescan_queued = false;
        }
        Some(event)
    }

    /// Waits up to `timeout` for the next event.
    ///
    /// Returns `None` when nothing arrived in time.
    #[must_use]
    pub fn next_event(&self, timeout: Duration) -> Option<WatchEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut queue = lock(&self.shared.queue);
                if let Some(event) = queue.events.pop_front() {
                    if matches!(event, WatchEvent::RescanRequired { .. }) {
                        queue.rescan_queued = false;
                    }
                    return Some(event);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let guard = lock(&self.shared.queue);
            drop(
                self.shared
                    .condvar
                    .wait_timeout(guard, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }

    /// A snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> WatchStats {
        let counters = &self.shared.counters;
        WatchStats {
            batches: counters.batches.load(Ordering::Relaxed),
            rescan_required: counters.rescan_required.load(Ordering::Relaxed),
            path_gone: counters.path_gone.load(Ordering::Relaxed),
            suppressed: self.shared.registry.suppressed_count(),
            overflow: counters.overflow.load(Ordering::Relaxed),
            errors: counters.errors.load(Ordering::Relaxed),
            ignored: counters.ignored.load(Ordering::Relaxed),
        }
    }
}

impl Drop for WatchService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    condvar: Condvar,
    stop: AtomicBool,
    capacity: usize,
    counters: Counters,
    registry: Arc<RecentWrites>,
}

#[derive(Debug, Default)]
struct Queue {
    events: VecDeque<WatchEvent>,
    rescan_queued: bool,
}

#[derive(Debug, Default)]
struct Counters {
    batches: AtomicU64,
    rescan_required: AtomicU64,
    path_gone: AtomicU64,
    overflow: AtomicU64,
    errors: AtomicU64,
    ignored: AtomicU64,
}

impl Shared {
    /// Batches leave the last slot free so a rescan notice always fits.
    fn has_room_for_batch(&self, queue: &Queue) -> bool {
        queue.events.len() + 1 < self.capacity
    }

    fn push_batch(&self, batch: WatchBatch) {
        let mut queue = lock(&self.queue);
        if !self.has_room_for_batch(&queue) {
            self.counters.overflow.fetch_add(1, Ordering::Relaxed);
            self.queue_rescan(&mut queue, batch.observed_at);
            return;
        }
        self.counters.batches.fetch_add(1, Ordering::Relaxed);
        queue.events.push_back(WatchEvent::Batch(batch));
        self.condvar.notify_all();
    }

    fn force_rescan(&self, observed_at: OffsetDateTime) {
        let mut queue = lock(&self.queue);
        self.queue_rescan(&mut queue, observed_at);
        self.condvar.notify_all();
    }

    fn queue_rescan(&self, queue: &mut Queue, observed_at: OffsetDateTime) {
        if queue.rescan_queued {
            return;
        }
        // Batches keep one slot free so a rescan notice always fits.
        if queue.events.len() >= self.capacity {
            self.counters.overflow.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters
            .rescan_required
            .fetch_add(1, Ordering::Relaxed);
        queue.rescan_queued = true;
        queue
            .events
            .push_back(WatchEvent::RescanRequired { observed_at });
    }

    fn push_path_gone(&self, path: PathBuf) {
        let mut queue = lock(&self.queue);
        if !self.has_room_for_batch(&queue) {
            self.counters.overflow.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.path_gone.fetch_add(1, Ordering::Relaxed);
        queue.events.push_back(WatchEvent::PathGone {
            path,
            observed_at: OffsetDateTime::now_utc(),
        });
        self.condvar.notify_all();
    }
}

fn run_worker(
    shared: &Shared,
    config: &WatchConfig,
    ready: &SyncSender<Result<(), WatchError>>,
    done: &SyncSender<()>,
) {
    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = match new_debouncer(config.debounce, None, move |result| {
        let _ = tx.send(result);
    }) {
        Ok(debouncer) => debouncer,
        Err(source) => {
            let _ = ready.send(Err(WatchError::Watcher(source)));
            return;
        }
    };
    let mut roots = Roots::build(&config.roots);
    if let Err(error) = roots.reconcile(&mut debouncer, &shared.counters) {
        let _ = ready.send(Err(error));
        return;
    }
    let ignored = roots.ignore_rules(&config.ignored);
    let mut index = PathIndex::new(PATH_INDEX_CAPACITY);
    index.seed(&roots, &ignored);
    let mut burst = BurstGuard::new(config.max_batch_paths, config.burst_quiet);
    let mut reseed = false;
    if ready.send(Ok(())).is_err() {
        return;
    }

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(result) => {
                if reseed {
                    let mut fresh = PathIndex::new(PATH_INDEX_CAPACITY);
                    fresh.seed(&roots, &ignored);
                    index = fresh;
                    reseed = false;
                }
                match result {
                    Ok(events) => {
                        let needs_rescan = process(
                            events, shared, config, &ignored, &roots, &mut index, &mut burst,
                        );
                        reseed |= needs_rescan;
                    }
                    Err(errors) => {
                        shared
                            .counters
                            .errors
                            .fetch_add(errors.len() as u64, Ordering::Relaxed);
                        shared.force_rescan(OffsetDateTime::now_utc());
                        reseed = true;
                    }
                }
                roots.poll(&mut debouncer, shared);
            }
            Err(RecvTimeoutError::Timeout) => {
                roots.poll(&mut debouncer, shared);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(debouncer);
    let _ = done.send(());
}

/// Processes one debounce cycle. Returns whether the inode index has to be rebuilt.
fn process(
    events: Vec<notify_debouncer_full::DebouncedEvent>,
    shared: &Shared,
    config: &WatchConfig,
    ignored: &IgnoreRules,
    roots: &Roots,
    index: &mut PathIndex,
    burst: &mut BurstGuard,
) -> bool {
    let mut candidates: BTreeSet<PathBuf> = BTreeSet::new();
    for event in events {
        for path in event.event.paths {
            candidates.insert(path);
        }
    }
    let mut unrelated = 0usize;
    candidates.retain(|path| {
        if ignored.is_ignored(path) {
            return false;
        }
        if !roots.is_relevant(path) {
            unrelated += 1;
            return false;
        }
        true
    });
    shared
        .counters
        .ignored
        .fetch_add(unrelated as u64, Ordering::Relaxed);
    if candidates.is_empty() {
        return false;
    }
    candidates.retain(|path| {
        if config.registry.should_suppress(path) {
            index.refresh(path);
            false
        } else {
            true
        }
    });
    if candidates.is_empty() {
        return false;
    }
    let observed_at = OffsetDateTime::now_utc();
    match burst.observe(Instant::now(), candidates.len()) {
        Burst::Supress => return false,
        Burst::Rescan => {
            shared.force_rescan(observed_at);
            return true;
        }
        Burst::Accept => {}
    }
    let mut changes = Vec::with_capacity(candidates.len());
    // Classify surviving paths first: a rename arrives as an event for each side, and the new
    // side has to be matched against the old inode before the old path is reported as removed.
    let (existing, missing): (Vec<PathBuf>, Vec<PathBuf>) = candidates
        .into_iter()
        .partition(|path| fs::symlink_metadata(path).is_ok());
    for path in existing {
        if let Some(change) = index.classify(&path) {
            changes.push(change);
        }
    }
    for path in missing {
        if let Some(change) = index.classify(&path) {
            changes.push(change);
        }
    }
    let batch = WatchBatch::build(changes, observed_at);
    if !batch.is_empty() {
        shared.push_batch(batch);
    }
    false
}

/// One root as the worker tracks it.
#[derive(Debug)]
struct RootRuntime {
    root: WatchRoot,
    present: bool,
    watch_path: Option<PathBuf>,
}

/// The configured roots plus the watches actually installed.
#[derive(Debug, Default)]
struct Roots {
    roots: Vec<RootRuntime>,
    watched: HashMap<PathBuf, RecursiveMode>,
}

impl Roots {
    fn build(roots: &[WatchRoot]) -> Self {
        let runtimes = roots
            .iter()
            .map(|root| {
                let mut root = root.clone();
                // The platform reports events on resolved paths (`/private/var/...` on macOS),
                // so the roots have to be resolved too or nothing ever matches them.
                root.path = canonicalize_root(&root.path);
                let present = fs::symlink_metadata(&root.path).is_ok();
                RootRuntime {
                    watch_path: desired_watch_path(&root, present),
                    root,
                    present,
                }
            })
            .collect();
        Self {
            roots: runtimes,
            watched: HashMap::new(),
        }
    }

    /// The configured rules plus the scratch directories of every watched store.
    ///
    /// A store root keeps its caches and operation backups below itself; those trees change
    /// constantly and are never interesting to a consumer.
    fn ignore_rules(&self, base: &IgnoreRules) -> IgnoreRules {
        let mut rules = base.clone();
        for runtime in &self.roots {
            if matches!(
                runtime.root.kind,
                WatchRootKind::UserStore | WatchRootKind::ProjectStore | WatchRootKind::GlobalStore
            ) {
                rules.exclude_tree(runtime.root.path.join("cache"));
                rules.exclude_tree(runtime.root.path.join("backups"));
            }
        }
        rules
    }

    /// Whether `path` is the root itself or lies inside one.
    fn is_relevant(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|runtime| path == runtime.root.path || path.starts_with(&runtime.root.path))
    }

    fn desired(&self) -> BTreeMap<PathBuf, RecursiveMode> {
        let mut desired = BTreeMap::new();
        for runtime in &self.roots {
            let Some(path) = &runtime.watch_path else {
                continue;
            };
            let recursive = runtime.root.recursive && runtime.present && *path == runtime.root.path;
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            desired
                .entry(path.clone())
                .and_modify(|existing| {
                    if mode == RecursiveMode::Recursive {
                        *existing = RecursiveMode::Recursive;
                    }
                })
                .or_insert(mode);
        }
        desired
    }

    fn reconcile(
        &mut self,
        debouncer: &mut FsDebouncer,
        counters: &Counters,
    ) -> Result<(), WatchError> {
        let desired = self.desired();
        let obsolete: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|(path, mode)| desired.get(*path) != Some(*mode))
            .map(|(path, _)| path.clone())
            .collect();
        for path in obsolete {
            // A vanished path makes unwatch fail; the watch is gone either way.
            if debouncer.unwatch(&path).is_err() {
                counters.errors.fetch_add(1, Ordering::Relaxed);
            }
            self.watched.remove(&path);
        }
        for (path, mode) in desired {
            if self.watched.contains_key(&path) {
                continue;
            }
            debouncer
                .watch(&path, mode)
                .map_err(|source| WatchError::WatchFailed {
                    path: path.clone(),
                    source,
                })?;
            self.watched.insert(path, mode);
        }
        Ok(())
    }

    /// Notices roots that disappeared or returned and moves the watches accordingly.
    fn poll(&mut self, debouncer: &mut FsDebouncer, shared: &Shared) {
        let mut changed = false;
        for runtime in &mut self.roots {
            let exists = fs::symlink_metadata(&runtime.root.path).is_ok();
            if runtime.present && !exists {
                runtime.present = false;
                runtime.watch_path = desired_watch_path(&runtime.root, false);
                shared.push_path_gone(runtime.root.path.clone());
                changed = true;
            } else if !runtime.present && exists {
                runtime.present = true;
                runtime.watch_path = desired_watch_path(&runtime.root, true);
                changed = true;
            }
        }
        if changed && self.reconcile(debouncer, &shared.counters).is_err() {
            shared.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Where to install the watch for a root.
///
/// File roots are watched through their parent directory: agent configuration files are replaced
/// atomically, and a watch bound to the file's inode would stop reporting after the first
/// replacement.
fn desired_watch_path(root: &WatchRoot, present: bool) -> Option<PathBuf> {
    if root.kind.is_directory() {
        if present {
            Some(root.path.clone())
        } else {
            nearest_existing_ancestor(&root.path)
        }
    } else {
        root.path.parent().and_then(nearest_existing_ancestor)
    }
}

/// Resolves `path` as far as it exists, keeping the missing tail.
fn canonicalize_root(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;
    loop {
        if let Ok(mut resolved) = fs::canonicalize(current) {
            for part in missing.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if fs::symlink_metadata(current).is_ok() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

/// Inodes and paths seen so far, used to tell creates, deletes and renames apart.
#[derive(Debug)]
struct PathIndex {
    known: HashSet<PathBuf>,
    by_inode: HashMap<(u64, u64), PathBuf>,
    capacity: usize,
}

fn inode_key(meta: &fs::Metadata) -> Option<(u64, u64)> {
    if meta.file_type().is_symlink() {
        None
    } else {
        Some((meta.dev(), meta.ino()))
    }
}

impl PathIndex {
    fn new(capacity: usize) -> Self {
        Self {
            known: HashSet::new(),
            by_inode: HashMap::new(),
            capacity,
        }
    }

    fn seed(&mut self, roots: &Roots, ignored: &IgnoreRules) {
        for runtime in &roots.roots {
            let path = &runtime.root.path;
            let Ok(meta) = fs::symlink_metadata(path) else {
                continue;
            };
            if meta.is_dir() {
                self.walk(path, ignored);
            } else {
                self.insert(path.clone(), inode_key(&meta));
            }
        }
    }

    fn walk(&mut self, root: &Path, ignored: &IgnoreRules) {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if self.known.len() >= self.capacity {
                break;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if ignored.is_ignored(&path) {
                    continue;
                }
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                self.insert(path.clone(), inode_key(&meta));
                if meta.is_dir() {
                    stack.push(path);
                }
                if self.known.len() >= self.capacity {
                    break;
                }
            }
        }
    }

    fn insert(&mut self, path: PathBuf, key: Option<(u64, u64)>) {
        if self.known.len() >= self.capacity && !self.known.contains(&path) {
            return;
        }
        if let Some(key) = key {
            self.by_inode.entry(key).or_insert_with(|| path.clone());
        }
        self.known.insert(path);
    }

    fn remove(&mut self, path: &Path) -> bool {
        let removed = self.known.remove(path);
        if removed {
            self.by_inode.retain(|_, known| known != path);
        }
        removed
    }

    /// Updates the index after a suppressed event without reporting it.
    fn refresh(&mut self, path: &Path) {
        match fs::symlink_metadata(path) {
            Ok(meta) => self.insert(path.to_path_buf(), inode_key(&meta)),
            Err(_) => {
                self.remove(path);
            }
        }
    }

    /// Classifies the current state of `path` against what the index remembers.
    fn classify(&mut self, path: &Path) -> Option<WatchChange> {
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                let key = inode_key(&meta);
                if self.remove(path) {
                    self.insert(path.to_path_buf(), key);
                    return Some(WatchChange::Modified(path.to_path_buf()));
                }
                if let Some(key) = key
                    && let Some(previous) = self.by_inode.get(&key).cloned()
                    && previous != path
                    && fs::symlink_metadata(&previous).is_err()
                {
                    self.by_inode.remove(&key);
                    self.known.remove(&previous);
                    self.insert(path.to_path_buf(), Some(key));
                    return Some(WatchChange::Renamed {
                        from: previous,
                        to: path.to_path_buf(),
                    });
                }
                self.insert(path.to_path_buf(), key);
                Some(WatchChange::Created(path.to_path_buf()))
            }
            Err(_) => {
                if self.remove(path) {
                    Some(WatchChange::Removed(path.to_path_buf()))
                } else {
                    None
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Burst {
    Accept,
    Supress,
    Rescan,
}

/// Collapses a storm of changes into a single rescan.
///
/// A burst starts when more than `threshold` paths were seen within one quiet window and ends
/// when no further change arrived for `quiet`.
#[derive(Debug)]
struct BurstGuard {
    threshold: usize,
    quiet: Duration,
    count: usize,
    last: Option<Instant>,
    active: bool,
}

impl BurstGuard {
    fn new(threshold: usize, quiet: Duration) -> Self {
        Self {
            threshold,
            quiet,
            count: 0,
            last: None,
            active: false,
        }
    }

    fn observe(&mut self, now: Instant, paths: usize) -> Burst {
        if self.threshold == 0 {
            return Burst::Accept;
        }
        if self.active {
            let quiet = self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) >= self.quiet);
            if quiet {
                self.active = false;
                self.count = 0;
            } else {
                self.last = Some(now);
                return Burst::Supress;
            }
        }
        self.count = self.count.saturating_add(paths);
        self.last = Some(now);
        if self.count > self.threshold {
            self.active = true;
            return Burst::Rescan;
        }
        Burst::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, b"x").expect("write");
    }

    #[test]
    fn ignore_rules_cover_the_required_paths() {
        let rules = IgnoreRules::standard();
        let root = Path::new("/tmp/store");
        assert!(rules.is_ignored(&root.join(".git/config")));
        assert!(rules.is_ignored(&root.join("node_modules/left-pad/index.js")));
        assert!(rules.is_ignored(&root.join(".claude.json.agentuna-tmp-4242-1")));
        assert!(rules.is_ignored(&root.join(".config.json.swp")));
        assert!(!rules.is_ignored(&root.join("skills/demo/SKILL.md")));
        assert!(!rules.is_ignored(&root.join(".claude.json")));
        // A name that merely starts with an ignored name is not ignored.
        assert!(!rules.is_ignored(&root.join("node_modules_backup/file")));
    }

    #[test]
    fn store_roots_hide_their_scratch_trees() {
        let dir = TempDir::new().expect("temp");
        let canonical = fs::canonicalize(dir.path()).expect("canonical");
        let store = canonical.join("store");
        fs::create_dir_all(store.join("cache")).expect("cache");
        fs::create_dir_all(store.join("backups")).expect("backups");
        let roots = Roots::build(&[WatchRoot::dir(&store, WatchRootKind::UserStore)]);
        let rules = roots.ignore_rules(&IgnoreRules::standard());
        assert!(rules.is_ignored(&store.join("cache/state.bin")));
        assert!(rules.is_ignored(&store.join("backups/op-1/blob")));
        assert!(!rules.is_ignored(&store.join("skills/demo/SKILL.md")));
        // A skills directory is not a store: its own cache folder stays visible.
        let skills = Roots::build(&[WatchRoot::dir(&store, WatchRootKind::SkillsDir)]);
        let rules = skills.ignore_rules(&IgnoreRules::standard());
        assert!(!rules.is_ignored(&store.join("cache/state.bin")));
    }

    #[test]
    fn extra_ignores_are_honoured() {
        let mut rules = IgnoreRules::standard();
        rules
            .exclude_tree("/tmp/support/backups")
            .exclude_name("dist")
            .exclude_pair("store", "scratch")
            .exclude_suffix(".bak");
        assert!(rules.is_ignored(Path::new("/tmp/support/backups/op/blob")));
        assert!(rules.is_ignored(Path::new("/tmp/project/dist/bundle.js")));
        assert!(rules.is_ignored(Path::new("/tmp/project/store/scratch/x")));
        assert!(rules.is_ignored(Path::new("/tmp/project/settings.bak")));
        assert!(!rules.is_ignored(Path::new("/tmp/project/settings.json")));
    }

    #[test]
    fn burst_guard_collapses_a_storm_and_recovers_after_quiet() {
        let start = Instant::now();
        let mut guard = BurstGuard::new(10, Duration::from_millis(200));
        assert_eq!(guard.observe(start, 6), Burst::Accept);
        assert_eq!(guard.observe(start, 4), Burst::Accept);
        assert_eq!(guard.observe(start, 1), Burst::Rescan);
        assert_eq!(guard.observe(start, 100), Burst::Supress);
        assert_eq!(
            guard.observe(start + Duration::from_millis(100), 3),
            Burst::Supress
        );
        // The quiet window restarted when the last change arrived at +100 ms.
        assert_eq!(
            guard.observe(start + Duration::from_millis(250), 3),
            Burst::Supress
        );
        assert_eq!(
            guard.observe(start + Duration::from_millis(500), 3),
            Burst::Accept
        );
    }

    #[test]
    fn burst_guard_can_be_disabled() {
        let start = Instant::now();
        let mut guard = BurstGuard::new(0, Duration::from_millis(200));
        for _ in 0..100 {
            assert_eq!(guard.observe(start, 10_000), Burst::Accept);
        }
    }

    #[test]
    fn recent_writes_suppress_only_while_content_matches() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        touch(&file);
        let registry = RecentWrites::with_ttl(Duration::from_secs(2));
        let now = Instant::now();

        assert!(!registry.should_suppress_at(&file, now));
        registry.record_at(&file, sha256_hex(b"x"), now);
        assert!(registry.should_suppress_at(&file, now));
        assert_eq!(registry.suppressed_count(), 1);

        // An external write inside the window still counts as a change.
        fs::write(&file, b"y").expect("write");
        assert!(!registry.should_suppress_at(&file, now));
        assert_eq!(registry.suppressed_count(), 1);
    }

    #[test]
    fn recent_writes_expire_after_their_ttl() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        touch(&file);
        let registry = RecentWrites::with_ttl(Duration::from_secs(2));
        let now = Instant::now();
        registry.record_at(&file, sha256_hex(b"x"), now - Duration::from_secs(3));
        assert_eq!(registry.len(), 1);
        assert!(!registry.should_suppress_at(&file, now));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn path_index_reports_create_modify_remove_and_rename() {
        let dir = TempDir::new().expect("temp");
        let root = dir.path();
        let rules = IgnoreRules::standard();
        let roots = Roots::build(&[WatchRoot::dir(root, WatchRootKind::UserStore)]);
        let mut index = PathIndex::new(1000);
        index.seed(&roots, &rules);

        let first = root.join("a.json");
        touch(&first);
        assert_eq!(
            index.classify(&first),
            Some(WatchChange::Created(first.clone()))
        );
        fs::write(&first, b"changed").expect("write");
        assert_eq!(
            index.classify(&first),
            Some(WatchChange::Modified(first.clone()))
        );

        let second = root.join("b.json");
        fs::rename(&first, &second).expect("rename");
        assert_eq!(
            index.classify(&second),
            Some(WatchChange::Renamed {
                from: first.clone(),
                to: second.clone()
            })
        );
        fs::remove_file(&second).expect("remove");
        assert_eq!(
            index.classify(&second),
            Some(WatchChange::Removed(second.clone()))
        );
        // A path that was never known and is gone is not reported.
        assert_eq!(index.classify(&root.join("never-existed")), None);
    }

    #[test]
    fn path_index_ignores_symlinks_for_inode_pairing() {
        let dir = TempDir::new().expect("temp");
        let canonical = fs::canonicalize(dir.path()).expect("canonical");
        let root = canonical.as_path();
        let target = root.join("target.json");
        touch(&target);
        let link = root.join("link.json");
        symlink(&target, &link).expect("symlink");
        let mut index = PathIndex::new(100);
        index.seed(
            &Roots::build(&[WatchRoot::dir(root, WatchRootKind::UserStore)]),
            &IgnoreRules::standard(),
        );
        // The link exists and is known, so it is a modification of a known path.
        fs::remove_file(&link).expect("remove");
        symlink(&target, &link).expect("symlink");
        assert_eq!(
            index.classify(&link),
            Some(WatchChange::Modified(link.clone()))
        );
    }

    #[test]
    fn batch_deduplicates_and_sorts() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let batch = WatchBatch::build(
            vec![
                WatchChange::Modified(PathBuf::from("/b")),
                WatchChange::Created(PathBuf::from("/a")),
                WatchChange::Modified(PathBuf::from("/b")),
                WatchChange::Renamed {
                    from: PathBuf::from("/c"),
                    to: PathBuf::from("/d"),
                },
            ],
            at,
        );
        assert_eq!(batch.len(), 3);
        assert_eq!(
            batch.paths,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c"),
                PathBuf::from("/d"),
            ]
        );
        assert_eq!(batch.renames(), vec![(Path::new("/c"), Path::new("/d"))]);
        assert!(batch.contains(Path::new("/d")));
    }

    #[test]
    fn file_roots_watch_their_parent() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("claude.json");
        let root = WatchRoot::file(&file, WatchRootKind::McpFile);
        assert_eq!(
            desired_watch_path(&root, false),
            Some(dir.path().to_path_buf())
        );
        touch(&file);
        assert_eq!(
            desired_watch_path(&root, true),
            Some(dir.path().to_path_buf())
        );
    }

    #[test]
    fn missing_directory_roots_watch_the_nearest_existing_ancestor() {
        let dir = TempDir::new().expect("temp");
        let missing = dir.path().join("project/.agentuna");
        let root = WatchRoot::dir(&missing, WatchRootKind::ProjectStore);
        assert_eq!(
            desired_watch_path(&root, false),
            Some(dir.path().to_path_buf())
        );
    }
}
