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
//!   shared with the service and the events caused by those writes never reach a batch.
//!   Attribution is by content, so a starved watcher thread that delivers the event long after
//!   the write still has it suppressed, while an external write that lands inside the window is
//!   reported immediately because its bytes hash differently.
//! * Build artefacts and editor litter are ignored: `.git/`, `node_modules/`, `*.agentuna-tmp-*`
//!   and `*.swp` never appear in a batch. A watched store root also hides its own `cache/` and
//!   `backups/` subtrees, so store scratch never looks like a user change.
//! * Renames are reported once, as [`WatchChange::Renamed`] carrying the old and the new path.
//! * A burst too large to describe (more than [`DEFAULT_MAX_BATCH_PATHS`] paths) collapses into a
//!   single [`WatchEvent::RescanRequired`]; a full queue does the same. The queue never exceeds
//!   its capacity, so memory stays bounded.
//! * A watched path that disappears yields [`WatchEvent::PathGone`]; the watch moves to the
//!   nearest existing ancestor and the subtree is watched again once the path comes back. The
//!   disappearance is noticed from a delivered event or, because a platform may not report the
//!   removal of a watched root at all, from a periodic stat-only recheck (see
//!   `ROOT_RECHECK_INTERVAL`).
//!
//! # Platform notes
//!
//! macOS FSEvents reports a rename as one event per side and does not pair them, and it reports a
//! removed subtree as a single event for the directory. This module therefore normalises events
//! against the filesystem (and against the inode index it keeps) instead of trusting the event
//! kind, so the batches describe what is true after the burst rather than what the platform
//! happened to report.
//!
//! Windows (`ReadDirectoryChangesW`) has no stable file id in `std`, so the inode index is empty
//! there and a rename arrives as a delete plus a create. That is a coarser batch, not a weaker
//! guarantee: every reported path is still checked against the filesystem before it is emitted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use time::OffsetDateTime;

use crate::atomic::WriteOutcome;
use crate::hash::sha256_hex;

/// Debounce window applied to a burst of changes.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// How long a recorded write keeps suppressing events for a path whose content can no longer be
/// read: a delete racing the read is the only case the content comparison cannot decide.
pub const DEFAULT_SUPPRESS_TTL: Duration = Duration::from_secs(5);

/// Capacity of the batch queue.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;

/// Largest number of paths a single batch may carry before it collapses into a rescan.
pub const DEFAULT_MAX_BATCH_PATHS: usize = 512;

/// How long a burst has to be quiet before events are reported again after a rescan.
pub const DEFAULT_BURST_QUIET: Duration = Duration::from_secs(1);

/// How long [`WatchService::stop`] waits for the worker thread to report completion.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(1);

/// How long [`WatchService::start`] waits for the platform watches before reporting *started*
/// and letting installation finish in the background.
///
/// Installing the watches is fast on a quiet filesystem (milliseconds). Right after a
/// skill-manager run, though, the platform stream start can block for seconds while the OS
/// catches up with the churn, so `start` never waits longer than this grace: the caller learns
/// immediately that the service is coming up, and [`WatchService::status`] reports
/// [`WatchStatus::Starting`] until the watches are in.
pub const DEFAULT_READY_GRACE: Duration = Duration::from_millis(750);

/// Upper bound on the files and directories indexed per watched root, including the trees
/// behind symlinked skill entries.
///
/// A root beyond the bound is not indexed; it is reported as too large via
/// [`WatchService::status`] and degrades to the periodic rescan the consumers already run on
/// [`WatchEvent::RescanRequired`]. Watching never becomes unavailable because a tree is large.
pub const DEFAULT_MAX_ENTRIES_PER_ROOT: usize = 20_000;

/// Upper bound on symlinked skill entries per root whose target directory gets its own watch.
///
/// Beyond the bound the root stops gaining target watches and degrades to periodic rescan,
/// exactly like [`DEFAULT_MAX_ENTRIES_PER_ROOT`].
pub const DEFAULT_MAX_LINK_TARGETS_PER_ROOT: usize = 256;

/// Upper bound on the inode index used to recognise renames.
const PATH_INDEX_CAPACITY: usize = 100_000;
/// How often the worker re-stats a root while another root is missing.
///
/// A missing root has no watch of its own, so its return is seen by the parent watch or by this
/// stat. Five seconds keeps the retry cheap while a store is absent.
const MISSING_ROOT_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// How often the worker re-stats a root while every root is present.
///
/// A removed root is not guaranteed to produce an event. macOS `FSEvents` reports `RootChanged`
/// only for a stream created with `kFSEventStreamCreateFlagWatchRoot`, which `notify` does not
/// set, so the removal of a root reaches this worker only through the parent watch — and on a
/// loaded arm64 CI runner that delivered nothing at all for 30 s while the store was gone. This
/// stat-only recheck makes `PathGone` independent of any delivered event. The cost is one
/// `symlink_metadata` per root and never a tree walk, so an idle watcher stays cheap and the index
/// is still never re-walked.
const ROOT_RECHECK_INTERVAL: Duration = Duration::from_secs(3);

/// What one reconcile pass wants from the platform watcher.
type WatchPlan = BTreeMap<PathBuf, RecursiveMode>;

/// Installs a whole watch plan in **one** platform session.
///
/// macOS rebuilds the entire FSEvents stream on every individual watch change, and a long run
/// of stop/start cycles can leave the stream dead, so a reconcile never calls `watch` per
/// path: obsolete paths are removed and every new path added through one `paths_mut` session,
/// and `commit` restarts the stream once.
fn install_plan(
    watcher: &mut RecommendedWatcher,
    desired: &WatchPlan,
    counters: &Counters,
    policy: ReconcileMode,
    watched: &mut HashMap<PathBuf, RecursiveMode>,
    on_refused: &mut dyn FnMut(&Path, &notify::Error),
) -> Result<(), WatchError> {
    let obsolete: Vec<PathBuf> = watched
        .iter()
        .filter(|(path, mode)| desired.get(*path) != Some(*mode))
        .map(|(path, _)| path.clone())
        .collect();
    let adds: Vec<&PathBuf> = desired
        .iter()
        .filter(|(path, _)| !watched.contains_key(*path))
        .map(|(path, _)| path)
        .collect();
    if obsolete.is_empty() && adds.is_empty() {
        // Nothing to change: do not touch the stream. Recreating it would drop the events
        // that arrive while the new stream spins up.
        return Ok(());
    }
    let mut session = watcher.paths_mut();
    for path in &obsolete {
        if session.remove(path).is_err() {
            // A vanished path makes the removal fail; the watch is gone either way.
            counters.errors.fetch_add(1, Ordering::Relaxed);
        }
        watched.remove(path);
    }
    for (path, mode) in desired {
        if watched.contains_key(path) {
            continue;
        }
        if let Err(source) = session.add(path, *mode) {
            if policy == ReconcileMode::Strict {
                return Err(WatchError::WatchFailed {
                    path: path.clone(),
                    source,
                });
            }
            on_refused(path, &source);
        } else {
            watched.insert(path.clone(), *mode);
        }
    }
    session.commit().map_err(WatchError::Watcher)?;
    Ok(())
}

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
/// Share one registry between the write path and the watch service. The write pipeline registers
/// expected content before installing it, so watcher events cannot race ahead of suppression.
///
/// A record stands for the *content* AgenTuna installed, not for a single event: every event for
/// that path is suppressed for as long as the file still holds a recorded hash, no matter how late
/// the platform delivers it. A record is dropped when the file holds different bytes (the change
/// is then the user's and is reported) or, for a path that cannot be read at all, when the
/// configured time-to-live elapses. Nothing is consumed, so two events for one write are two
/// suppressions.
#[derive(Debug)]
pub struct RecentWrites {
    ttl: Duration,
    entries: Mutex<HashMap<PathBuf, Vec<Recorded>>>,
    suppressed: AtomicU64,
}
static WRITE_TICKET_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
struct Recorded {
    id: u64,
    after_hash: String,
    at: Instant,
}

#[derive(Debug)]
pub(crate) struct WriteTicket {
    path: PathBuf,
    id: u64,
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
    ///
    /// Paths are canonicalized (resolving the part that exists) so this registry uses the same
    /// spelling as platform watcher events, notably macOS paths under `/private/var` and
    /// destinations that a dotfile manager symlinks.
    pub fn record(&self, path: impl Into<PathBuf>, after_hash: impl Into<String>) {
        let path = canonical_path(&path.into());
        self.record_at(path, after_hash, Instant::now());
    }

    /// Records a write observed at `at`, storing `path` exactly as spelled.
    ///
    /// Tests use the explicit instant to exercise the time window without sleeping. Production
    /// code goes through [`Self::record`] or the write pipeline's ticket, which both resolve the
    /// path the way the platform reports it.
    pub fn record_at(&self, path: impl Into<PathBuf>, after_hash: impl Into<String>, at: Instant) {
        let id = WRITE_TICKET_ID.fetch_add(1, Ordering::Relaxed);
        self.append_record(path.into(), after_hash.into(), at, id);
    }

    fn append_record(&self, path: PathBuf, after_hash: String, at: Instant, id: u64) {
        let mut entries = lock(&self.entries);
        let recorded = entries.entry(path).or_default();
        recorded.push(Recorded { id, after_hash, at });
        // Keep at most a handful of records per path; only the newest can match the file.
        if recorded.len() > 8 {
            let excess = recorded.len() - 8;
            recorded.drain(..excess);
        }
    }

    pub(crate) fn record_expected(&self, path: &Path, after_hash: String) -> WriteTicket {
        let path = canonical_path(path);
        let id = WRITE_TICKET_ID.fetch_add(1, Ordering::Relaxed);
        self.append_record(path.clone(), after_hash, Instant::now(), id);
        WriteTicket { path, id }
    }

    pub(crate) fn commit_expected(&self, ticket: &WriteTicket) {
        if let Some(recorded) = lock(&self.entries).get_mut(&ticket.path)
            && let Some(entry) = recorded.iter_mut().find(|entry| entry.id == ticket.id)
        {
            entry.at = Instant::now();
        }
    }

    pub(crate) fn cancel_expected(&self, ticket: &WriteTicket) {
        let mut entries = lock(&self.entries);
        let empty = entries.get_mut(&ticket.path).is_some_and(|recorded| {
            recorded.retain(|entry| entry.id != ticket.id);
            recorded.is_empty()
        });
        if empty {
            entries.remove(&ticket.path);
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
    ///
    /// The decision is by content: when the file holds a hash AgenTuna recorded, every event for
    /// that path belongs to that write, however many events the platform produces for it and
    /// however late a starved watcher delivers them. Different bytes are the user's change and are
    /// reported immediately. `now` only bounds records whose file cannot be read.
    #[must_use]
    pub fn should_suppress_at(&self, path: &Path, now: Instant) -> bool {
        // Sample the record ids first: a write that registers between this and the read below
        // installed bytes the read cannot see yet, so its record must stay.
        let sampled = {
            let entries = lock(&self.entries);
            match entries.get(path) {
                Some(recorded) if !recorded.is_empty() => {
                    recorded.iter().map(|entry| entry.id).collect::<Vec<u64>>()
                }
                _ => return false,
            }
        };
        let observed = fs::read(path).map(|bytes| sha256_hex(&bytes));
        let mut entries = lock(&self.entries);
        let Some(recorded) = entries.get_mut(path) else {
            return false;
        };
        let suppressed = match observed.as_deref() {
            Ok(hash) => {
                let mut matched = false;
                for entry in &mut *recorded {
                    if entry.after_hash == hash {
                        // Slide the window so a burst of late events stays bounded by one TTL
                        // past the last one, never expiring under a live burst.
                        entry.at = now;
                        matched = true;
                    }
                }
                if !matched {
                    recorded.retain(|entry| !sampled.contains(&entry.id));
                }
                matched
            }
            // No content to compare: the time window alone decides.
            Err(_) => {
                recorded.retain(|entry| now.saturating_duration_since(entry.at) <= self.ttl);
                !recorded.is_empty()
            }
        };
        if recorded.is_empty() {
            entries.remove(path);
        }
        drop(entries);
        if suppressed {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
        }
        suppressed
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
    /// The worker ended before the service was ready.
    #[error("the watch worker ended before it was ready")]
    WorkerGone,
    /// The worker panicked.
    #[error("the watch worker panicked")]
    WorkerPanicked,
}

/// Why one watched root is not being watched as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RootDegradation {
    /// The platform refused the watch; the worker retries it on every root recheck.
    WatchFailed(String),
    /// The root holds more files or directories than the configured bound. It is not indexed
    /// and falls back to the periodic rescan consumers run for
    /// [`WatchEvent::RescanRequired`].
    TooLarge,
    /// A linked skill entry resolves outside every allowed link root, so it was not followed.
    ///
    /// The link entry itself is still watched; the directory it points at is not indexed or
    /// watched, and changes inside that directory are covered by the periodic rescan only if
    /// they also touch a watched path.
    LinkTargetOutsideRoots {
        /// The symlink entry inside the watched root.
        link: PathBuf,
        /// Canonical directory the link resolves to.
        target: PathBuf,
    },
}

impl std::fmt::Display for RootDegradation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WatchFailed(message) => write!(formatter, "watch refused: {message}"),
            Self::TooLarge => write!(
                formatter,
                "root too large to index; covered by periodic rescan"
            ),
            Self::LinkTargetOutsideRoots { link, target } => write!(
                formatter,
                "{}: link target {} is outside the allowed roots; not followed",
                link.display(),
                target.display()
            ),
        }
    }
}

/// One root that is not watched as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedRoot {
    /// The configured root path.
    pub path: PathBuf,
    /// Why the root is degraded.
    pub reason: RootDegradation,
}

/// What the service is currently doing, queried with [`WatchService::status`].
///
/// A start never fails because a tree is large or a platform stream is slow to come up: the
/// worst case is [`WatchStatus::Degraded`], and the worker keeps retrying degraded roots on
/// every root recheck.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatchStatus {
    /// The worker is running and the platform watches are not confirmed installed yet.
    Starting,
    /// Every root is watched as configured.
    Watching,
    /// Nothing could be watched. The worker stays alive and keeps retrying the install.
    Failed(String),
    /// Watching is up, but at least one root is degraded. Changes in the degraded roots are
    /// covered by the periodic rescan, never lost.
    Degraded(Vec<DegradedRoot>),
}

impl std::fmt::Display for WatchStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(formatter, "starting"),
            Self::Watching => write!(formatter, "watching"),
            Self::Failed(message) => write!(formatter, "not watching: {message}"),
            Self::Degraded(roots) => {
                write!(formatter, "watching with degraded roots:")?;
                for root in roots {
                    write!(formatter, " {} ({})", root.path.display(), root.reason)?;
                }
                Ok(())
            }
        }
    }
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
    /// Largest number of files and directories indexed per root, including linked target
    /// trees.
    pub max_entries_per_root: usize,
    /// Largest number of symlinked skill entries per root whose target gets its own watch.
    pub max_link_targets_per_root: usize,
    /// Roots a symlinked skill entry's target must resolve inside to be followed.
    ///
    /// Skill managers install skills as links into a central library, so a change inside the
    /// library folder is a change to the skill. The service only follows a link whose canonical
    /// target lies inside one of these roots; a link to any other directory (a link to `/`, an
    /// escaping target, another user's home) is not walked or watched, and the root reports
    /// [`RootDegradation::LinkTargetOutsideRoots`]. Empty (the default) follows no link target
    /// at all: the link entry itself is still watched.
    pub allowed_link_roots: Vec<PathBuf>,
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
            max_entries_per_root: DEFAULT_MAX_ENTRIES_PER_ROOT,
            max_link_targets_per_root: DEFAULT_MAX_LINK_TARGETS_PER_ROOT,
            allowed_link_roots: Vec::new(),
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

    /// Replaces the per-root bound on indexed files and directories.
    #[must_use]
    pub fn with_max_entries_per_root(mut self, max_entries_per_root: usize) -> Self {
        self.max_entries_per_root = max_entries_per_root.max(1);
        self
    }

    /// Replaces the per-root bound on watched symlink targets.
    #[must_use]
    pub fn with_max_link_targets_per_root(mut self, max_link_targets_per_root: usize) -> Self {
        self.max_link_targets_per_root = max_link_targets_per_root.max(1);
        self
    }

    /// Restricts link-target following to directories inside these roots.
    ///
    /// The roots should be canonical (the injected home and project root, for example); they
    /// are canonicalized when the service builds its roots, and roots that cannot be
    /// canonicalized are ignored. An empty list disables following entirely.
    #[must_use]
    pub fn with_allowed_link_roots(
        mut self,
        allowed_link_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        self.allowed_link_roots = allowed_link_roots.into_iter().collect();
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

/// Watch service: one worker thread, one platform watcher, one bounded queue.
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
                wake_sender: Mutex::new(None),
                status: Mutex::new(WatchStatus::Starting),
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
    /// `start` waits for the platform watches for [`DEFAULT_READY_GRACE`]. A refusal the
    /// platform reports immediately is returned as [`WatchError::WatchFailed`]; when the
    /// platform is simply slow (a busy stream start right after a skill-manager run), `start`
    /// reports *started* after the grace and installation finishes in the background, with
    /// [`WatchService::status`] reporting [`WatchStatus::Starting`] until the watches are in.
    /// Seeding the change index always happens after `start` returns.
    ///
    /// # Errors
    /// [`WatchError::AlreadyRunning`] when the service is already started;
    /// [`WatchError::Watcher`], [`WatchError::WatchFailed`], [`WatchError::Spawn`] and
    /// [`WatchError::WorkerGone`] when the platform refuses to watch the configured roots.
    pub fn start(&self) -> Result<(), WatchError> {
        let mut run = lock(&self.run);
        if run.handle.is_some() {
            return Err(WatchError::AlreadyRunning);
        }
        self.shared.stop.store(false, Ordering::SeqCst);
        self.shared.set_status(WatchStatus::Starting);
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
        match ready_rx.recv_timeout(DEFAULT_READY_GRACE) {
            Ok(Ok(())) => {
                run.handle = Some(handle);
                run.done = Some(done_rx);
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            // Slow platform install: the service is coming up; the worker finishes the
            // watches and the index in the background and keeps retrying refused roots.
            Err(RecvTimeoutError::Timeout) => {
                run.handle = Some(handle);
                run.done = Some(done_rx);
                Ok(())
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                Err(WatchError::WorkerGone)
            }
        }
    }

    /// What the service is currently doing.
    ///
    /// The status is maintained by the worker: [`WatchStatus::Starting`] until the platform
    /// watches are confirmed, then [`WatchStatus::Watching`], [`WatchStatus::Degraded`] for
    /// refused or oversized roots (which the worker keeps retrying), or
    /// [`WatchStatus::Failed`] when nothing could be watched at all.
    #[must_use]
    pub fn status(&self) -> WatchStatus {
        self.shared
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| {
                WatchStatus::Failed("the watch service status is unavailable".to_owned())
            })
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
            if let Some(sender) = lock(&self.shared.wake_sender).as_ref() {
                // Wake the worker so `stop` is observed immediately; the empty event is
                // discarded by the quiet-window collector.
                let _ = sender.send(Ok(notify::Event::new(notify::EventKind::Other)));
            }
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
    #[cfg(test)]
    fn idle_work_counts(&self) -> (u64, u64) {
        (
            self.shared.counters.index_seeds.load(Ordering::Relaxed),
            self.shared.counters.root_checks.load(Ordering::Relaxed),
        )
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
    wake_sender: Mutex<Option<mpsc::Sender<notify::Result<Event>>>>,
    status: Mutex<WatchStatus>,
}

impl Shared {
    fn set_status(&self, status: WatchStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }
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
    #[cfg(test)]
    index_seeds: AtomicU64,
    #[cfg(test)]
    root_checks: AtomicU64,
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
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    *lock(&shared.wake_sender) = Some(tx.clone());
    // Never let the platform backend follow a symlink when it installs a recursive watch.
    // Linux inotify follows symlinked children by default, which would watch an arbitrary
    // directory behind a direct-child skill link; the link targets allowed by
    // `WatchConfig::allowed_link_roots` are added explicitly below instead.
    let watcher_config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = match notify::RecommendedWatcher::new(
        move |result| {
            let _ = tx.send(result);
        },
        watcher_config,
    ) {
        Ok(watcher) => watcher,
        Err(source) => {
            let _ = ready.send(Err(WatchError::Watcher(source)));
            return;
        }
    };
    let mut roots = Roots::build_with_link_roots(&config.roots, &config.allowed_link_roots);
    // The first install reports per-root refusals synchronously through the ready channel,
    // which keeps the historical contract for callers that drop refused roots. A refusal that
    // arrives after the ready grace in `start` (the receiver is gone) degrades the service to
    // per-root retries instead of aborting it.
    let installed = roots.reconcile(&mut watcher, &shared.counters, ReconcileMode::Strict);
    shared.set_status(roots.watch_status());
    let ignored = roots.ignore_rules(&config.ignored);
    let mut burst = BurstGuard::new(config.max_batch_paths, config.burst_quiet);
    let mut reseed = false;
    let refused = installed.is_err();
    let handshake = ready.send(installed);
    if refused && handshake.is_ok() {
        // The caller is still waiting on the handshake and learns about the refusal.
        return;
    }

    // The index is seeded after `start` has returned: the platform watches are already
    // installed (or retrying), so the caller never waits on a tree walk. Events queued by the
    // platform during the seed are processed once it completes.
    let mut index = PathIndex::new(PATH_INDEX_CAPACITY);
    count_seed(shared);
    index.seed(
        &mut roots,
        &ignored,
        &shared.stop,
        config.max_entries_per_root,
        config.max_link_targets_per_root,
    );
    roots.finish_seed(&mut watcher, shared);

    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        // A missing root has no watch of its own; everything else is event-driven except for the
        // stat-only root recheck, which is what makes `PathGone` independent of event delivery.
        // The same recheck retries refused watches and unresolved linked skill targets, so a
        // degraded service heals in the background with a fixed cadence.
        let idle_wait = if roots.has_missing() {
            MISSING_ROOT_RETRY_INTERVAL
        } else {
            ROOT_RECHECK_INTERVAL
        };
        let received = match rx.recv_timeout(idle_wait) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => {
                if roots.poll(&mut watcher, shared) {
                    reseed_index(
                        &mut index,
                        &mut roots,
                        &ignored,
                        shared,
                        config,
                        &mut watcher,
                    );
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => None,
        };
        let Some(result) = received else {
            break;
        };
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if reseed {
            reseed_index(
                &mut index,
                &mut roots,
                &ignored,
                shared,
                config,
                &mut watcher,
            );
            reseed = false;
        }
        match result {
            Ok(event) => {
                // Quiet window: fold everything that arrives within one debounce window into
                // a single processing pass, so a burst of platform events still produces one
                // classified batch.
                let mut events = vec![event];
                loop {
                    match rx.recv_timeout(config.debounce) {
                        Ok(Ok(more)) => events.push(more),
                        Ok(Err(_)) => {}
                        Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                            break;
                        }
                    }
                }
                let needs_rescan = process(
                    events,
                    shared,
                    config,
                    &ignored,
                    &mut roots,
                    &mut index,
                    &mut burst,
                    &mut watcher,
                );
                reseed |= needs_rescan;
            }
            Err(error) => {
                shared.counters.errors.fetch_add(1, Ordering::Relaxed);
                shared.force_rescan(OffsetDateTime::now_utc());
                reseed = true;
                let _ = error;
            }
        }
        // Verify the roots after every batch, including batches delivered for an ancestor
        // watched on behalf of a root that is missing.
        if roots.poll(&mut watcher, shared) {
            reseed_index(
                &mut index,
                &mut roots,
                &ignored,
                shared,
                config,
                &mut watcher,
            );
        }
    }
    *lock(&shared.wake_sender) = None;
    drop(watcher);
    let _ = done.send(());
}

/// Rebuilds the inode index from what is on disk now.
///
/// Only a root that appeared again needs this: the tree behind the old paths no longer exists.
fn reseed_index(
    index: &mut PathIndex,
    roots: &mut Roots,
    ignored: &IgnoreRules,
    shared: &Shared,
    config: &WatchConfig,
    watcher: &mut RecommendedWatcher,
) {
    count_seed(shared);
    let mut fresh = PathIndex::new(PATH_INDEX_CAPACITY);
    fresh.seed(
        roots,
        ignored,
        &shared.stop,
        config.max_entries_per_root,
        config.max_link_targets_per_root,
    );
    roots.finish_seed(watcher, shared);
    *index = fresh;
}

/// Counts an index seed in test builds so the idle test can prove nothing is re-walked.
fn count_seed(shared: &Shared) {
    #[cfg(test)]
    shared.counters.index_seeds.fetch_add(1, Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = shared;
}

/// Processes one debounce cycle. Returns whether the inode index has to be rebuilt.
#[allow(clippy::too_many_arguments)]
fn process(
    events: Vec<Event>,
    shared: &Shared,
    config: &WatchConfig,
    ignored: &IgnoreRules,
    roots: &mut Roots,
    index: &mut PathIndex,
    burst: &mut BurstGuard,
    watcher: &mut RecommendedWatcher,
) -> bool {
    let mut reported: BTreeSet<PathBuf> = BTreeSet::new();
    for event in events {
        for path in event.paths {
            reported.insert(path);
        }
    }
    // Events inside a watched link target are attributed to the link path before anything
    // else: every downstream step (relevance, suppression, classification) then works in the
    // spelling the agents and the deployment records use. Several agents can link the same
    // library skill, so one target event becomes one candidate per link.
    let mut candidates: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for path in reported {
        let attributed = roots.attribute_event(&path);
        for link_path in attributed {
            candidates.entry(link_path).or_insert_with(|| path.clone());
        }
    }
    let mut unrelated = 0usize;
    candidates.retain(|link_path, target_path| {
        if roots.is_present_directory_root(link_path) {
            return false;
        }
        if ignored.is_ignored(link_path) {
            return false;
        }
        if !roots.is_relevant(link_path) {
            unrelated += 1;
            return false;
        }
        // Self-writes are recorded under the canonical spelling AgenTuna wrote through, which
        // for a linked skill is the library target; check both spellings.
        if config.registry.should_suppress(link_path)
            || config.registry.should_suppress(target_path)
        {
            index.refresh(link_path);
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
    let reported_paths: BTreeSet<PathBuf> = candidates.keys().cloned().collect();
    // Classify surviving paths first: a rename arrives as an event for each side, and the new
    // side has to be matched against the old inode before the old path is reported as removed.
    let mut existing = Vec::new();
    let mut missing = Vec::new();
    for link_path in candidates.keys() {
        match fs::symlink_metadata(link_path) {
            Ok(meta) => existing.push((link_path, meta)),
            Err(_) => missing.push(link_path),
        }
    }
    for (path, meta) in existing {
        let directory = meta.is_dir();
        if let Some(change) = index.classify_existing(path, &meta) {
            let scan_subtree = directory
                && matches!(
                    &change,
                    WatchChange::Created(_) | WatchChange::Renamed { .. }
                );
            changes.push(change);
            if scan_subtree
                && index.discover_subtree(
                    path,
                    ignored,
                    &reported_paths,
                    &mut changes,
                    config.max_batch_paths,
                )
            {
                shared.force_rescan(observed_at);
                return true;
            }
        }
    }
    for path in missing {
        if let Some(change) = index.classify_missing(path) {
            changes.push(change);
        }
    }
    if changes.len() > config.max_batch_paths {
        shared.force_rescan(observed_at);
        return true;
    }
    let batch = WatchBatch::build(changes, observed_at);
    if !batch.is_empty() {
        shared.push_batch(batch);
    }
    // Link bookkeeping: resolve new or replaced skill links, forget removed ones. Runs after
    // the batch is queued so the consumer sees the link change itself first.
    update_links(roots, index, watcher, shared, config, &candidates);
    false
}

/// Registers and unregisters linked skill entries after a batch.
///
/// A created or renamed-to symlink that is a direct child of a watched root gains a target
/// watch and a bounded link-space index walk; a removed or replaced link loses both. Root
/// bounds still apply: a root past its link-target bound degrades to periodic rescan.
fn update_links(
    roots: &mut Roots,
    index: &mut PathIndex,
    watcher: &mut RecommendedWatcher,
    shared: &Shared,
    config: &WatchConfig,
    candidates: &BTreeMap<PathBuf, PathBuf>,
) {
    let mut changed = false;
    for link_path in candidates.keys() {
        // A removed path classified above may still resolve if it was recreated within the
        // same burst; the current state decides.
        let state = fs::symlink_metadata(link_path);
        if roots.registered_link(link_path).is_some() {
            let still_symlink = state.as_ref().is_ok_and(|meta| meta.is_symlink());
            if !still_symlink {
                // The link is gone or was replaced by a real file or directory; the
                // link-space index entries go with it.
                roots.unregister_link(link_path);
                index.remove_prefix(link_path);
                changed = true;
            }
            continue;
        }
        let Ok(meta) = state else {
            continue;
        };
        if !meta.is_symlink() {
            continue;
        }
        // Only direct children of a watched root are registered as linked skills.
        let Some(root_index) = roots.root_of_child(link_path) else {
            continue;
        };
        if let Ok(target) = fs::canonicalize(link_path)
            && target.is_dir()
        {
            // A link is the entry the agent loads, but the directory behind it is only walked
            // and watched when the caller allowed that tree: a link to `/` or to another user's
            // home must never turn into a recursive watch.
            if !roots.allow_link_target(&target) {
                roots.record_outside_link(
                    root_index,
                    link_path.clone(),
                    target,
                    config.max_link_targets_per_root,
                );
                changed = true;
                continue;
            }
            if roots.register_link(
                root_index,
                link_path.clone(),
                target.clone(),
                config.max_link_targets_per_root,
            ) {
                let mut budget = WalkBudget::new(config.max_entries_per_root);
                index.walk(
                    &target,
                    link_path,
                    &config.ignored,
                    roots,
                    root_index,
                    &mut budget,
                    &shared.stop,
                    config.max_link_targets_per_root,
                    false,
                );
            } else {
                roots.roots[root_index].too_large = true;
            }
            changed = true;
        }
    }
    if changed {
        if roots
            .reconcile(watcher, &shared.counters, ReconcileMode::Degrade)
            .is_err()
        {
            shared.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
        shared.set_status(roots.watch_status());
    }
}

/// A symlinked skill entry inside a watched root whose target directory is watched as well.
///
/// Policy: skill managers install skills as symlinks into the agent roots, so a change inside
/// the linked library folder *is* a change to the skill. The service watches the resolved
/// target directory (recursively, bounded by the per-root entry and link-target bounds) and
/// attributes every event to the link path, so consumers see changes under the spelling the
/// agents and the deployment records use. Links are registered only for direct children of a
/// watched root; symlinked directories deeper inside a target tree are never followed, which
/// keeps the walk finite and the watch set bounded. Changes missed that way are covered by the
/// periodic rescan.
#[derive(Debug)]
struct LinkWatch {
    /// The symlink entry, spelled under its canonical watched root.
    link: PathBuf,
    /// The canonical directory the link resolves to.
    target: PathBuf,
}

/// One root as the worker tracks it.
#[derive(Debug)]
struct RootRuntime {
    root: WatchRoot,
    present: bool,
    watch_path: Option<PathBuf>,
    /// Files and directories indexed for this root, including linked target trees.
    seed_entries: usize,
    /// Symlinked entries of this root whose target directory is watched.
    link_targets: usize,
    /// A configured bound was exceeded; the root degrades to periodic rescan.
    too_large: bool,
    /// The platform refused a watch for this root; retried on every root recheck.
    watch_failed: Option<String>,
    /// Symlinked entries that did not resolve during seeding; retried on the root recheck.
    pending_links: Vec<PathBuf>,
    /// Linked entries whose target resolved outside every allowed link root.
    outside_links: Vec<(PathBuf, PathBuf)>,
}

/// The configured roots plus the watches actually installed.
#[derive(Debug, Default)]
struct Roots {
    roots: Vec<RootRuntime>,
    watched: HashMap<PathBuf, RecursiveMode>,
    links: Vec<LinkWatch>,
    /// Canonical roots a link target must resolve inside to be followed.
    link_roots: Vec<PathBuf>,
}

/// The watch installation policy for one reconcile pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileMode {
    /// A refused watch aborts the install (the pre-ready handshake contract).
    Strict,
    /// A refused watch is recorded per root and retried on the next root recheck.
    Degrade,
}

impl Roots {
    #[cfg(test)]
    fn build(roots: &[WatchRoot]) -> Self {
        Self::build_with_link_roots(roots, &[])
    }

    /// Builds the runtimes with the link-target policy the caller configured.
    fn build_with_link_roots(roots: &[WatchRoot], allowed_link_roots: &[PathBuf]) -> Self {
        let mut link_roots = Vec::new();
        for root in allowed_link_roots {
            if let Ok(canonical) = fs::canonicalize(root)
                && canonical.is_dir()
                && !link_roots.contains(&canonical)
            {
                link_roots.push(canonical);
            }
        }
        let runtimes = roots
            .iter()
            .map(|root| {
                let mut root = root.clone();
                // The platform reports events on resolved paths (`/private/var/...` on macOS),
                // so the roots have to be resolved too or nothing ever matches them.
                root.path = canonical_path(&root.path);
                let present = fs::symlink_metadata(&root.path).is_ok();
                RootRuntime {
                    watch_path: desired_watch_path(&root, present),
                    root,
                    present,
                    seed_entries: 0,
                    link_targets: 0,
                    too_large: false,
                    watch_failed: None,
                    pending_links: Vec::new(),
                    outside_links: Vec::new(),
                }
            })
            .collect();
        Self {
            roots: runtimes,
            watched: HashMap::new(),
            links: Vec::new(),
            link_roots,
        }
    }

    /// Whether a canonical link target may be walked and watched.
    ///
    /// With no configured roots nothing is followed: watching a link entry is safe, walking an
    /// arbitrary directory behind it is not.
    fn allow_link_target(&self, target: &Path) -> bool {
        self.link_roots.iter().any(|root| target.starts_with(root))
    }

    /// Records a linked entry whose target resolved outside the allowed roots.
    ///
    /// Bounded by the same per-root link-target bound, so a root full of escaping links cannot
    /// grow the status list without limit.
    fn record_outside_link(
        &mut self,
        root_index: usize,
        link: PathBuf,
        target: PathBuf,
        link_target_cap: usize,
    ) {
        let Some(runtime) = self.roots.get_mut(root_index) else {
            return;
        };
        if runtime.outside_links.len() >= link_target_cap
            || runtime
                .outside_links
                .iter()
                .any(|(known, _)| *known == link)
        {
            return;
        }
        runtime.outside_links.push((link, target));
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

    fn is_present_directory_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|runtime| {
            runtime.present && runtime.root.kind.is_directory() && runtime.root.path == path
        })
    }

    /// The root index holding `path` as the root itself or as a direct child, if any.
    ///
    /// Symlinked skill entries are only registered for direct children of a watched root, so a
    /// parent lookup is enough to attribute a discovered link.
    fn root_of_child(&self, link: &Path) -> Option<usize> {
        let parent = link.parent()?;
        self.roots
            .iter()
            .position(|runtime| runtime.root.path == parent)
    }

    /// Registers a linked skill entry for event attribution and target watching.
    ///
    /// `link_target_cap` is the configured per-root bound. Returns `false` when the root
    /// already watches its bound of link targets; the caller then degrades the root to
    /// periodic rescan.
    fn register_link(
        &mut self,
        root_index: usize,
        link: PathBuf,
        target: PathBuf,
        link_target_cap: usize,
    ) -> bool {
        if self.links.iter().any(|known| known.link == link) {
            return true;
        }
        // A target inside an already recursively watched root needs no watch of its own; the
        // root walk covers it under the real spelling.
        if self.is_relevant(&target) {
            return true;
        }
        if self.roots[root_index].link_targets >= link_target_cap {
            return false;
        }
        self.roots[root_index].link_targets += 1;
        self.links.push(LinkWatch { link, target });
        true
    }

    /// Forgets the link registration for `link`, if any. Returns the target that was watched.
    fn unregister_link(&mut self, link: &Path) -> Option<PathBuf> {
        let position = self.links.iter().position(|known| known.link == link)?;
        let removed = self.links.remove(position);
        if let Some(runtime) = self.root_of_child(&removed.link) {
            self.roots[runtime].link_targets = self.roots[runtime].link_targets.saturating_sub(1);
        }
        Some(removed.target)
    }

    /// Drops every link registration; used before a full re-seed rebuilds them.
    fn reset_links(&mut self) {
        self.links.clear();
        for runtime in &mut self.roots {
            runtime.link_targets = 0;
            runtime.pending_links.clear();
            runtime.outside_links.clear();
            runtime.seed_entries = 0;
            runtime.too_large = false;
        }
    }

    /// Link-space spellings of an event that happened inside a watched link target.
    ///
    /// Several agents can link the same library skill, so one target event becomes one
    /// attributed path per link. A path outside every watched target maps to itself.
    fn attribute_event(&self, path: &Path) -> Vec<PathBuf> {
        let mut attributed = Vec::new();
        for link in &self.links {
            let relative = if path == link.target {
                None
            } else {
                path.strip_prefix(&link.target).ok()
            };
            if let Some(relative) = relative {
                attributed.push(link.link.join(relative));
            }
        }
        if attributed.is_empty() {
            attributed.push(path.to_path_buf());
        }
        attributed
    }

    /// The registered link that `link` belongs to, if any.
    fn registered_link(&self, link: &Path) -> Option<&LinkWatch> {
        self.links.iter().find(|known| known.link == link)
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
            // Directory roots deliberately get no parent watch: their disappearance and return
            // are covered by the stat-only root recheck (`poll`), and every avoided watch is
            // one fewer platform stream restart on macOS, where each `watch` call rebuilds the
            // whole stream. File roots keep watching through their parent by construction
            // (`desired_watch_path`).
        }
        // Linked skill targets are watched recursively so a change inside the linked library
        // folder reaches the service and is attributed to the link path.
        for link in &self.links {
            desired
                .entry(link.target.clone())
                .and_modify(|existing| *existing = RecursiveMode::Recursive)
                .or_insert(RecursiveMode::Recursive);
        }
        desired
    }

    /// The watch installation policy for one pass.
    fn reconcile(
        &mut self,
        watcher: &mut RecommendedWatcher,
        counters: &Counters,
        policy: ReconcileMode,
    ) -> Result<(), WatchError> {
        let desired = self.desired();
        let mut refused: Vec<(PathBuf, String)> = Vec::new();
        {
            let watched = &mut self.watched;
            install_plan(
                watcher,
                &desired,
                counters,
                policy,
                watched,
                &mut |path, source| {
                    refused.push((path.to_path_buf(), source.to_string()));
                },
            )?;
        }
        let refused_paths: BTreeSet<PathBuf> =
            refused.iter().map(|(path, _)| path.clone()).collect();
        for path in desired.keys() {
            if !refused_paths.contains(path) {
                self.clear_watch_failed(path);
            }
        }
        for (path, message) in refused {
            self.mark_watch_failed(&path, message);
        }
        Ok(())
    }

    /// Runtimes whose configured watch includes `path`.
    fn runtimes_for_watch_path(&self, path: &Path) -> Vec<usize> {
        self.roots
            .iter()
            .enumerate()
            .filter(|(_, runtime)| {
                runtime.watch_path.as_deref() == Some(path)
                    || runtime.root.path == path
                    || (runtime.root.kind.is_directory()
                        && runtime.root.path.parent() == Some(path))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn mark_watch_failed(&mut self, path: &Path, message: String) {
        for index in self.runtimes_for_watch_path(path) {
            self.roots[index].watch_failed = Some(message.clone());
        }
    }

    fn clear_watch_failed(&mut self, path: &Path) {
        for index in self.runtimes_for_watch_path(path) {
            self.roots[index].watch_failed = None;
        }
    }

    /// The condition the service reports to its consumer.
    fn watch_status(&self) -> WatchStatus {
        let mut degraded: Vec<DegradedRoot> = Vec::new();
        let mut first_failure = String::new();
        for runtime in &self.roots {
            if let Some(message) = &runtime.watch_failed {
                degraded.push(DegradedRoot {
                    path: runtime.root.path.clone(),
                    reason: RootDegradation::WatchFailed(message.clone()),
                });
                if first_failure.is_empty() {
                    first_failure.clone_from(message);
                }
            }
            if runtime.too_large {
                degraded.push(DegradedRoot {
                    path: runtime.root.path.clone(),
                    reason: RootDegradation::TooLarge,
                });
            }
            for (link, target) in &runtime.outside_links {
                degraded.push(DegradedRoot {
                    path: runtime.root.path.clone(),
                    reason: RootDegradation::LinkTargetOutsideRoots {
                        link: link.clone(),
                        target: target.clone(),
                    },
                });
            }
        }
        if degraded.is_empty() {
            return WatchStatus::Watching;
        }
        if self.watched.is_empty() {
            // Nothing at all could be watched; the worker keeps retrying in the background.
            return WatchStatus::Failed(first_failure);
        }
        WatchStatus::Degraded(degraded)
    }

    /// Installs watches for links discovered while seeding and reports the resulting
    /// condition; oversized roots ask their consumer for a periodic rescan.
    fn finish_seed(&mut self, watcher: &mut RecommendedWatcher, shared: &Shared) {
        if self
            .reconcile(watcher, &shared.counters, ReconcileMode::Degrade)
            .is_err()
        {
            shared.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
        if self.roots.iter().any(|runtime| runtime.too_large) {
            shared.force_rescan(OffsetDateTime::now_utc());
        }
        shared.set_status(self.watch_status());
    }

    fn has_missing(&self) -> bool {
        self.roots.iter().any(|runtime| !runtime.present)
    }

    /// Re-stats every root and reports whether one of them came back.
    ///
    /// Cheap by construction: one `symlink_metadata` per root plus a bounded retry of links
    /// that did not resolve when they were seeded. It runs after every delivered batch and on
    /// the idle recheck, so a root that vanished without an event is still reported as
    /// [`WatchEvent::PathGone`], and a refused watch or a late-materialising library is
    /// retried in the background.
    fn poll(&mut self, watcher: &mut RecommendedWatcher, shared: &Shared) -> bool {
        let mut changed = false;
        let mut returned = false;
        for runtime in &mut self.roots {
            #[cfg(test)]
            shared.counters.root_checks.fetch_add(1, Ordering::Relaxed);
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
                returned = true;
            }
        }
        if changed
            && self
                .reconcile(watcher, &shared.counters, ReconcileMode::Degrade)
                .is_err()
        {
            shared.counters.errors.fetch_add(1, Ordering::Relaxed);
        }
        if !returned && self.retry_pending_links() {
            // A library materialised behind a dangling skill link; the caller re-seeds, which
            // registers the link and watches its target.
            returned = true;
        }
        returned
    }

    /// Probes the links that did not resolve during the last seed.
    ///
    /// One `canonicalize` per pending link, bounded by the entries the seed walked; a hit
    /// asks the caller for a re-seed, which registers the link and watches its target.
    fn retry_pending_links(&mut self) -> bool {
        let pending: Vec<PathBuf> = self
            .roots
            .iter()
            .flat_map(|runtime| runtime.pending_links.iter().cloned())
            .collect();
        pending
            .into_iter()
            .any(|link| fs::canonicalize(&link).is_ok())
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
///
/// Used for watch roots and for recorded writes alike: both must use the spelling the platform
/// reports, and a destination that does not exist yet has no inode to canonicalize, so its parent
/// is resolved instead. Without that a create under `/var` or a symlinked dotfile manager
/// directory would record one spelling and be matched against the other.
fn canonical_path(path: &Path) -> PathBuf {
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

/// Bounds the seeding work per root: files and directories indexed for one root, including the
/// trees behind its linked skill targets.
struct WalkBudget {
    used: usize,
    limit: usize,
}

impl WalkBudget {
    fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    /// Takes one entry from the budget; `false` means the root exceeded the bound and the
    /// caller marks it too large.
    fn take(&mut self) -> bool {
        if self.used >= self.limit {
            return false;
        }
        self.used += 1;
        true
    }
}

/// A stable identity for a path across a rename, when the platform offers one.
///
/// Unix pairs the device and inode. Windows `ReadDirectoryChangesW` reports a rename as one old
/// name and one new name with no shared key and std exposes no file id without an unstable
/// feature, so pairing is disabled there: a rename is then reported as a delete plus a create.
/// Both describe what is true on disk; only the shape of the batch differs.
fn inode_key(meta: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        if meta.file_type().is_symlink() {
            None
        } else {
            Some((meta.dev(), meta.ino()))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
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

    /// Indexes every root for change classification.
    ///
    /// Symlinked skill entries that are direct children of a root are resolved and their
    /// target tree is indexed under the *link* spelling, so events attributed to the link
    /// classify against what is really inside the linked skill. The walk never follows a
    /// symlink deeper inside a tree, which keeps it finite and bounded: once a root exhausts
    /// its configured entry bound, the root is marked too large and degrades to periodic
    /// rescan. Dangling links are recorded for the root recheck to retry.
    fn seed(
        &mut self,
        roots: &mut Roots,
        ignored: &IgnoreRules,
        stop: &AtomicBool,
        max_entries_per_root: usize,
        max_link_targets_per_root: usize,
    ) {
        roots.reset_links();
        for index in 0..roots.roots.len() {
            let path = roots.roots[index].root.path.clone();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                self.walk(
                    &path,
                    &path,
                    ignored,
                    roots,
                    index,
                    &mut WalkBudget::new(max_entries_per_root),
                    stop,
                    max_link_targets_per_root,
                    true,
                );
            } else {
                roots.roots[index].seed_entries += 1;
                self.insert(path.clone(), inode_key(&meta));
            }
        }
    }

    /// Walks one directory, inserting paths under `prefix`.
    ///
    /// `dir` and `prefix` differ only for linked skill targets, where `dir` is the resolved
    /// library directory and `prefix` the link path the agents use. `register_links` is true
    /// only for the top level of a watched root: symlinked skill entries are registered there,
    /// never inside a target tree, which keeps the walk finite and the watch set bounded.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        dir: &Path,
        prefix: &Path,
        ignored: &IgnoreRules,
        roots: &mut Roots,
        root_index: usize,
        budget: &mut WalkBudget,
        stop: &AtomicBool,
        link_target_cap: usize,
        register_links: bool,
    ) {
        let mut stack = vec![(dir.to_path_buf(), prefix.to_path_buf())];
        while let Some((dir, prefix)) = stack.pop() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if self.known.len() >= self.capacity {
                // The global index is a memory bound, not a "root too large" signal; the
                // per-root behaviour is governed by the walk budget.
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
                let insert_path = prefix.join(entry.file_name());
                let is_symlink = meta.is_symlink();
                if !budget.take() {
                    roots.roots[root_index].too_large = true;
                    return;
                }
                roots.roots[root_index].seed_entries += 1;
                if is_symlink {
                    self.insert(insert_path.clone(), None);
                    if register_links {
                        match fs::canonicalize(&path) {
                            Ok(target) if target.is_dir() => {
                                if !roots.allow_link_target(&target) {
                                    // Refused with a typed reason instead of walking an
                                    // arbitrary directory behind the entry.
                                    roots.record_outside_link(
                                        root_index,
                                        insert_path,
                                        target,
                                        link_target_cap,
                                    );
                                    continue;
                                }
                                if roots.register_link(
                                    root_index,
                                    insert_path.clone(),
                                    target.clone(),
                                    link_target_cap,
                                ) {
                                    // Index the linked skill's content under the link
                                    // spelling; bounded by the same budget.
                                    self.walk(
                                        &target,
                                        &insert_path,
                                        ignored,
                                        roots,
                                        root_index,
                                        budget,
                                        stop,
                                        link_target_cap,
                                        false,
                                    );
                                } else {
                                    roots.roots[root_index].too_large = true;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => roots.roots[root_index].pending_links.push(insert_path),
                        }
                    }
                    continue;
                }
                self.insert(insert_path.clone(), inode_key(&meta));
                if meta.is_dir() {
                    stack.push((path, insert_path));
                }
            }
        }
    }

    /// Removes a subtree spelling from the index, for example a linked skill whose link was
    /// replaced or removed.
    fn remove_prefix(&mut self, prefix: &Path) {
        let dropped: Vec<PathBuf> = self
            .known
            .iter()
            .filter(|path| path == &prefix || path.starts_with(prefix))
            .cloned()
            .collect();
        for path in dropped {
            self.known.remove(&path);
            self.by_inode.retain(|_, known| known != &path);
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
    #[cfg(test)]
    fn classify(&mut self, path: &Path) -> Option<WatchChange> {
        match fs::symlink_metadata(path) {
            Ok(meta) => self.classify_existing(path, &meta),
            Err(_) => self.classify_missing(path),
        }
    }

    fn classify_existing(&mut self, path: &Path, meta: &fs::Metadata) -> Option<WatchChange> {
        let key = inode_key(meta);
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

    fn classify_missing(&mut self, path: &Path) -> Option<WatchChange> {
        if self.remove(path) {
            Some(WatchChange::Removed(path.to_path_buf()))
        } else {
            None
        }
    }

    /// Finds files that appeared before inotify installed its watch on a new directory.
    fn discover_subtree(
        &mut self,
        root: &Path,
        ignored: &IgnoreRules,
        candidates: &BTreeSet<PathBuf>,
        changes: &mut Vec<WatchChange>,
        max_changes: usize,
    ) -> bool {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
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
                if meta.is_dir() {
                    stack.push(path.clone());
                }
                if candidates.contains(&path) || self.known.contains(&path) {
                    continue;
                }
                if let Some(change) = self.classify_existing(&path, &meta) {
                    changes.push(change);
                    if changes.len() > max_changes {
                        return true;
                    }
                }
            }
        }
        false
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

// Unix-only fixtures: POSIX symlinks, permission bits and raw errno constants. The
// Windows twins arrive with the slice that enables the Windows test suite.
#[cfg(all(test, unix))]
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
    fn idle_worker_rechecks_roots_without_rewalking_or_emitting() {
        let dir = TempDir::new().expect("temp");
        let root = dir.path().join("store");
        fs::create_dir_all(&root).expect("store");
        std::thread::sleep(Duration::from_millis(2_500));
        let service = WatchService::new(WatchConfig::new([WatchRoot::dir(
            &root,
            WatchRootKind::UserStore,
        )]));

        service.start().expect("start watcher");
        let (seeds, _) = service.idle_work_counts();
        assert_eq!(seeds, 1, "the initial index is seeded once");

        // Idle work is one stat per root per interval and nothing else: no tree walk, no event.
        let began = Instant::now();
        assert!(
            service
                .next_event(ROOT_RECHECK_INTERVAL + Duration::from_millis(750))
                .is_none(),
            "an idle watcher must not emit an event"
        );
        let elapsed = began.elapsed();
        let (idle_seeds, checks) = service.idle_work_counts();
        assert_eq!(
            idle_seeds, seeds,
            "idle rechecks must not re-index the tree"
        );
        let allowed = elapsed.as_millis() / ROOT_RECHECK_INTERVAL.as_millis() + 2;
        assert!(
            u128::from(checks) <= allowed,
            "idle rechecks must stay on the slow timer: {checks} root stats in {elapsed:?}"
        );
        assert!(checks >= 1, "the timer must recheck the root at least once");

        // The recheck is what makes the disappearance independent of a delivered event. A batch
        // describing the store's files can arrive first, so skip to the PathGone.
        fs::remove_dir_all(&root).expect("delete the store");
        let deadline = Instant::now() + ROOT_RECHECK_INTERVAL * 4;
        let mut gone = false;
        while Instant::now() < deadline {
            if let Some(WatchEvent::PathGone { .. }) =
                service.next_event(Duration::from_millis(200))
            {
                gone = true;
                break;
            }
        }
        assert!(
            gone,
            "a deleted root must be reported even when the platform delivers nothing"
        );
        assert_eq!(
            service.idle_work_counts().0,
            seeds,
            "reporting PathGone must not re-index the tree"
        );
        service.stop().expect("stop watcher");
    }

    #[test]
    fn a_root_that_vanishes_is_reported_by_the_stat_check_alone() {
        let dir = TempDir::new().expect("temp");
        let base = fs::canonicalize(dir.path()).expect("canonical");
        let store = base.join("store");
        fs::create_dir_all(store.join("skills")).expect("store");
        let shared = Shared {
            queue: Mutex::new(Queue::default()),
            condvar: Condvar::new(),
            stop: AtomicBool::new(false),
            capacity: 8,
            counters: Counters::default(),
            registry: Arc::new(RecentWrites::new()),
            wake_sender: Mutex::new(None),
            status: Mutex::new(WatchStatus::Starting),
        };
        let mut watcher = notify::recommended_watcher(|_| {}).expect("platform watcher");
        let mut roots = Roots::build(&[WatchRoot::dir(&store, WatchRootKind::UserStore)]);
        roots
            .reconcile(&mut watcher, &shared.counters, ReconcileMode::Strict)
            .expect("watch the store");

        // No watcher event takes part in this check: it stats the root itself, which is the
        // guarantee a platform that never reports a removed root's removal must still meet.
        assert!(
            !roots.poll(&mut watcher, &shared),
            "a present root reports nothing"
        );
        assert!(
            lock(&shared.queue).events.is_empty(),
            "a present root is quiet"
        );

        fs::remove_dir_all(&store).expect("delete the store");
        assert!(!roots.poll(&mut watcher, &shared), "nothing returned");
        let event = lock(&shared.queue)
            .events
            .pop_front()
            .expect("PathGone is queued");
        match event {
            WatchEvent::PathGone { path, .. } => assert_eq!(path, store),
            other => panic!("expected PathGone, got {other:?}"),
        }
        assert!(roots.has_missing(), "a missing root stays tracked");
        assert_eq!(
            roots.watched.keys().collect::<Vec<_>>(),
            vec![&base],
            "the watch falls back to the nearest existing ancestor"
        );

        // The return is noticed the same way and asks for a fresh index.
        fs::create_dir_all(&store).expect("recreate the store");
        assert!(
            roots.poll(&mut watcher, &shared),
            "a returned root asks for an index re-seed"
        );
        assert!(!roots.has_missing(), "the root is present again");
        assert_eq!(
            roots.watched.get(&store),
            Some(&RecursiveMode::Recursive),
            "the recursive watch is re-attached"
        );
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
    fn recent_writes_refresh_the_suppression_window_for_late_notifications() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        touch(&file);
        let registry = RecentWrites::with_ttl(Duration::from_secs(2));
        let start = Instant::now();
        registry.record_at(&file, sha256_hex(b"x"), start);

        assert!(registry.should_suppress_at(&file, start + Duration::from_secs(1)));
        assert!(
            registry.should_suppress_at(&file, start + Duration::from_millis(2_500)),
            "a suppressed native event extends suppression for later events in the same burst"
        );
        assert_eq!(registry.suppressed_count(), 2);
    }

    #[test]
    fn recent_writes_expire_only_when_the_content_cannot_be_read() {
        // A path that is gone cannot be compared, so the time-to-live alone decides; a path that
        // still holds the recorded bytes is attributed by content, however old the record is.
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        let registry = RecentWrites::with_ttl(Duration::from_secs(2));
        let now = Instant::now();
        registry.record_at(&file, sha256_hex(b"x"), now - Duration::from_secs(3));
        assert!(!registry.should_suppress_at(&file, now));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn recent_writes_suppress_every_event_for_one_write() {
        // The macOS failure this guards against: FSEvents reports the atomic rename more than
        // once and delivers the later event after the suppression window has passed. The bytes on
        // disk are still ours, so both events are ours too: nothing is consumed, nothing expires.
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        fs::write(&file, b"written").expect("write");
        let registry = RecentWrites::with_ttl(Duration::from_secs(2));
        let start = Instant::now();
        registry.record_at(&file, sha256_hex(b"written"), start);

        assert!(registry.should_suppress_at(&file, start + Duration::from_millis(200)));
        assert!(
            registry.should_suppress_at(&file, start + Duration::from_millis(5_000)),
            "a second event for the same write is a suppression, not a drift notice"
        );
        assert_eq!(registry.suppressed_count(), 2);
        assert_eq!(
            registry.len(),
            1,
            "the record stays for the next late event"
        );
    }

    #[test]
    fn recent_writes_report_an_external_edit_immediately() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        fs::write(&file, b"written").expect("write");
        let registry = RecentWrites::with_ttl(Duration::from_secs(30));
        let start = Instant::now();
        registry.record_at(&file, sha256_hex(b"written"), start);

        fs::write(&file, b"user edited").expect("external write");
        assert!(
            !registry.should_suppress_at(&file, start),
            "different bytes inside the window are the user's change"
        );
        assert_eq!(registry.suppressed_count(), 0);
        assert_eq!(registry.len(), 0, "a superseded record is dropped");
    }

    #[test]
    fn recent_writes_keep_nothing_after_a_later_external_edit() {
        let dir = TempDir::new().expect("temp");
        let file = dir.path().join("config.json");
        fs::write(&file, b"written").expect("write");
        let registry = RecentWrites::with_ttl(Duration::from_secs(30));
        let start = Instant::now();
        registry.record_at(&file, sha256_hex(b"written"), start);
        assert!(registry.should_suppress_at(&file, start + Duration::from_secs(10)));

        fs::write(&file, b"user edited").expect("external write");
        assert!(!registry.should_suppress_at(&file, start + Duration::from_secs(11)));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn recent_writes_resolve_a_destination_that_does_not_exist_yet() {
        // A create has no inode to canonicalize, so the parent is resolved instead. Otherwise the
        // record keeps the caller's spelling while the watcher reports the resolved one, and a
        // path under `/var` (every macOS temporary directory) is never suppressed.
        let dir = TempDir::new().expect("temp");
        let created = dir.path().join("new/nested/config.json");
        let registry = RecentWrites::new();
        registry.record(&created, sha256_hex(b"new"));
        fs::create_dir_all(created.parent().expect("parent")).expect("parents");
        fs::write(&created, b"new").expect("write");

        let reported = fs::canonicalize(&created).expect("canonical");
        assert!(registry.should_suppress_at(&reported, Instant::now()));
    }

    #[test]
    fn path_index_reports_create_modify_remove_and_rename() {
        let dir = TempDir::new().expect("temp");
        let root = dir.path();
        let rules = IgnoreRules::standard();
        let mut roots = Roots::build(&[WatchRoot::dir(root, WatchRootKind::UserStore)]);
        let mut index = PathIndex::new(1000);
        index.seed(
            &mut roots,
            &rules,
            &AtomicBool::new(false),
            DEFAULT_MAX_ENTRIES_PER_ROOT,
            DEFAULT_MAX_LINK_TARGETS_PER_ROOT,
        );

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
        let mut roots = Roots::build(&[WatchRoot::dir(root, WatchRootKind::UserStore)]);
        index.seed(
            &mut roots,
            &IgnoreRules::standard(),
            &AtomicBool::new(false),
            DEFAULT_MAX_ENTRIES_PER_ROOT,
            DEFAULT_MAX_LINK_TARGETS_PER_ROOT,
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
