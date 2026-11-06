//! Bounded project discovery under configured roots.
//!
//! The project picker is built from folders on disk, not from a user-maintained list. A walk of
//! `~/Projects` plus any folders the user added finds the projects and hands the database a diff
//! it can upsert:
//!
//! - a folder is a project when it contains one of [`PROJECT_MARKERS`];
//! - the walk is depth-bounded ([`ScanConfig::with_max_depth`]) and never descends into a
//!   detected project, so a nested repository collapses into the shallowest match instead of
//!   being listed twice;
//! - `node_modules`, `target`, `dist`, virtualenvs and every hidden directory are skipped, so a
//!   scan of a normal `~/Projects` does not walk dependency trees;
//! - symlinks are never followed, which rules out a directory cycle entirely — a root that is
//!   itself a symlink is resolved once before walking;
//! - a missing root, a denied directory or an unreadable entry becomes a [`ScanWarning`] and the
//!   scan continues;
//! - the scan is cancellable through a [`ScanObserver`] and reports progress every
//!   [`ScanConfig::progress_every`] entries, which is what lets [`spawn_scan`] run as a job.
//!
//! Discovery only reads the filesystem: it never executes anything found in a repository and
//! never interprets a file's content. [`ProjectDiff`] carries the result to the registry through
//! [`apply_diff`], which touches new and missing projects only — an unchanged project is never
//! written, so a scan cannot move its `last_used_at`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Folder under the home directory that is scanned by default.
pub const DEFAULT_ROOT_DIR: &str = "Projects";

/// Default depth bound: a project nested three levels below a root is found.
pub const DEFAULT_MAX_DEPTH: usize = 4;

/// Hard upper bound for [`ScanConfig::with_max_depth`].
pub const MAX_SCAN_DEPTH: usize = 8;

/// Default number of entries between two progress reports.
pub const DEFAULT_PROGRESS_EVERY: usize = 100;

/// Entry that marks a folder as a project, in detection order.
pub const PROJECT_MARKERS: [&str; 5] = [".git", ".agentuna", ".claude", ".agents", ".mcp.json"];

/// Directory names the walk never descends into.
///
/// Hidden directories are skipped as a rule, so `.venv`, `.git` and `.next` are covered by the
/// hidden check; this list holds the heavy names that are not hidden.
pub const SKIPPED_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "venv",
    "dist",
    "build",
    "out",
    "vendor",
    "__pycache__",
    "pods",
    "deriveddata",
];

/// Why a folder is treated as a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ProjectMarker {
    /// A `.git` directory or file.
    Git,
    /// An AgenTuna project store.
    Store,
    /// Claude Code project configuration.
    ClaudeCode,
    /// The shared `.agents` folder.
    Agents,
    /// A project-level `.mcp.json`.
    McpFile,
}

impl ProjectMarker {
    /// The entry name that signals this marker.
    #[must_use]
    pub const fn entry_name(self) -> &'static str {
        match self {
            Self::Git => ".git",
            Self::Store => ".agentuna",
            Self::ClaudeCode => ".claude",
            Self::Agents => ".agents",
            Self::McpFile => ".mcp.json",
        }
    }

    /// The marker an entry name signals, if any.
    fn from_entry(name: &str) -> Option<Self> {
        match name {
            ".git" => Some(Self::Git),
            ".agentuna" => Some(Self::Store),
            ".claude" => Some(Self::ClaudeCode),
            ".agents" => Some(Self::Agents),
            ".mcp.json" => Some(Self::McpFile),
            _ => None,
        }
    }
}

/// One folder discovered as a project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiscoveredProject {
    /// Absolute, symlink-free path of the project folder.
    pub path: PathBuf,
    /// Display name: the folder name.
    pub name: String,
    /// Root the folder was found under.
    pub source_root: PathBuf,
    /// Distance below the root; `0` when the root itself is a project.
    pub depth: usize,
    /// Markers present, in [`PROJECT_MARKERS`] order; never empty.
    pub markers: Vec<ProjectMarker>,
}

/// Which roots to walk and how far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    roots: Vec<PathBuf>,
    max_depth: usize,
    progress_every: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ScanConfig {
    /// A scan of `roots` at the default depth and progress interval.
    ///
    /// A configuration with no roots scans nothing; [`Self::defaults_for`] supplies the
    /// documented default root.
    #[must_use]
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut roots: Vec<PathBuf> = roots.into_iter().collect();
        roots.sort();
        roots.dedup();
        Self {
            roots,
            max_depth: DEFAULT_MAX_DEPTH,
            progress_every: DEFAULT_PROGRESS_EVERY,
        }
    }

    /// The default configuration: `~/Projects` only.
    #[must_use]
    pub fn defaults_for(home: &Path) -> Self {
        Self::new([home.join(DEFAULT_ROOT_DIR)])
    }

    /// The roots to walk, sorted.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Adds a user-configured root.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        if !self.roots.contains(&root) {
            self.roots.push(root);
            self.roots.sort();
        }
        self
    }

    /// Depth bound for the walk, clamped to `1..=`[`MAX_SCAN_DEPTH`].
    #[must_use]
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth.clamp(1, MAX_SCAN_DEPTH);
        self
    }

    /// The depth bound.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Entries between two progress reports; `0` is treated as `1`.
    #[must_use]
    pub fn with_progress_every(mut self, every: usize) -> Self {
        self.progress_every = every.max(1);
        self
    }

    /// Entries between two progress reports, at least `1`.
    #[must_use]
    pub fn progress_every(&self) -> usize {
        self.progress_every.max(1)
    }
}

/// One progress report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanProgress {
    /// Directory entries examined so far.
    pub visited: usize,
    /// Projects found so far.
    pub projects_found: usize,
    /// Directory being listed when the report was produced.
    pub current: PathBuf,
}

/// Receives progress and cancellation for a scan.
///
/// Implementations must be cheap to call and must not block; the scan calls them on its own
/// thread, once per entry for cancellation and once per [`ScanConfig::progress_every`] entries
/// for progress.
pub trait ScanObserver: Send + Sync {
    /// Whether the scan should stop at the next safe point.
    fn is_cancelled(&self) -> bool {
        false
    }

    /// Receives a progress report.
    fn progress(&self, progress: &ScanProgress) {
        let _ = progress;
    }
}

/// A [`ScanObserver`] that never cancels and ignores progress.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoObserver;

impl ScanObserver for NoObserver {}

/// A cancellable [`ScanObserver`] a caller can flip from another thread.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag {
    cancelled: Arc<AtomicBool>,
}

impl CancelFlag {
    /// A flag that is not set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the scan to stop.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether the scan was asked to stop.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl ScanObserver for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Why one path could not be scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ScanWarningKind {
    /// The root does not exist.
    MissingRoot,
    /// The root exists but is not a directory.
    NotADirectory,
    /// The process may not list the directory.
    PermissionDenied,
    /// The directory or an entry could not be read for another reason.
    Unreadable,
}

/// A path the scan skipped instead of aborting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanWarning {
    /// Path that could not be scanned.
    pub path: PathBuf,
    /// What went wrong.
    pub kind: ScanWarningKind,
    /// Operating-system message, for the log and the UI.
    pub message: String,
}

/// The result of one scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Projects found, sorted by path.
    pub projects: Vec<DiscoveredProject>,
    /// Problems reported instead of aborting, sorted by path.
    pub warnings: Vec<ScanWarning>,
    /// Directory entries examined.
    pub visited: usize,
    /// Directories skipped because of their name.
    pub skipped_directories: usize,
    /// Symlinks skipped because the walk does not follow them.
    pub skipped_symlinks: usize,
    /// Directories not descended because of the depth bound.
    pub depth_limited: usize,
    /// Whether a cancellation stopped the scan early.
    pub cancelled: bool,
}

impl ScanOutcome {
    /// Whether the scan completed without a cancellation.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.cancelled
    }
}

/// Scans with no observer.
#[must_use]
pub fn scan(config: &ScanConfig) -> ScanOutcome {
    scan_with(config, &NoObserver)
}

/// Scans `config`, reporting progress and cancellation to `observer`.
///
/// Never executes anything it finds and never writes: discovery is read-only.
#[must_use]
pub fn scan_with(config: &ScanConfig, observer: &dyn ScanObserver) -> ScanOutcome {
    let mut state = ScanState {
        progress_every: config.progress_every(),
        ..ScanState::default()
    };
    let mut seen_roots: BTreeSet<PathBuf> = BTreeSet::new();
    for root in config.roots() {
        if observer.is_cancelled() {
            state.cancelled = true;
            break;
        }
        let canonical = match fs::canonicalize(root) {
            Ok(canonical) => canonical,
            Err(error) => {
                state.warn(root, &error);
                continue;
            }
        };
        if !seen_roots.insert(canonical.clone()) {
            continue;
        }
        if !canonical.is_dir() {
            state.warnings.push(ScanWarning {
                path: root.clone(),
                kind: ScanWarningKind::NotADirectory,
                message: "not a directory".to_owned(),
            });
            continue;
        }
        state.walk(config, observer, &canonical);
        if state.cancelled {
            break;
        }
    }
    state.finish()
}

/// Runs a scan on a blocking thread and returns its handle.
///
/// Progress and cancellation reach the job through `observer`, so the UI never waits on the
/// walk. Must be called from inside a tokio runtime.
#[must_use]
pub fn spawn_scan(
    config: ScanConfig,
    observer: Arc<dyn ScanObserver>,
) -> tokio::task::JoinHandle<ScanOutcome> {
    tokio::task::spawn_blocking(move || scan_with(&config, observer.as_ref()))
}

/// What a scan added since the registry was last updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiff {
    /// Projects on disk that the registry does not know.
    pub new: Vec<DiscoveredProject>,
    /// Projects both on disk and in the registry.
    pub unchanged: Vec<DiscoveredProject>,
    /// Registry paths that no longer contain a project marker, or no longer exist.
    pub missing: Vec<PathBuf>,
}

impl ProjectDiff {
    /// Compares a scan result with the paths the registry already holds.
    ///
    /// Paths are compared canonically so a registry row written through a symlink or a different
    /// case is not reported as missing and re-added.
    #[must_use]
    pub fn between(discovered: &[DiscoveredProject], known: &[PathBuf]) -> Self {
        let known_keys: BTreeSet<PathBuf> = known.iter().map(|path| canonical_or(path)).collect();
        let discovered_keys: BTreeSet<PathBuf> = discovered
            .iter()
            .map(|project| canonical_or(&project.path))
            .collect();
        let mut new = Vec::new();
        let mut unchanged = Vec::new();
        for project in discovered {
            if known_keys.contains(&canonical_or(&project.path)) {
                unchanged.push(project.clone());
            } else {
                new.push(project.clone());
            }
        }
        let missing = known
            .iter()
            .filter(|path| !discovered_keys.contains(&canonical_or(path)))
            .cloned()
            .collect();
        Self {
            new,
            unchanged,
            missing,
        }
    }

    /// Whether the registry already matches the disk.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.new.is_empty() && self.missing.is_empty()
    }
}

/// Why a [`ProjectRegistry`] could not record a change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", .path.display())]
pub struct ScanApplyError {
    /// Path whose row could not be written.
    pub path: PathBuf,
    /// Database message.
    pub message: String,
}

/// The seam a scan diff is applied to.
///
/// The SQLite implementation lives outside this crate; discovery only depends on these two
/// operations. An unchanged project never reaches this trait: [`apply_diff`] dispatches new and
/// missing rows only, so a scan cannot refresh (or clear) an existing project's `last_used_at`.
pub trait ProjectRegistry {
    /// Registers a project found on disk.
    ///
    /// # Errors
    ///
    /// Any [`ScanApplyError`] the caller's implementation reports.
    fn insert_discovered(&mut self, project: &DiscoveredProject) -> Result<(), ScanApplyError>;

    /// Flags a registered project that no longer looks like a project on disk.
    ///
    /// # Errors
    ///
    /// Any [`ScanApplyError`] the caller's implementation reports.
    fn mark_missing(&mut self, path: &Path) -> Result<(), ScanApplyError>;
}

/// Applies a diff to `registry` and returns how many rows were touched.
///
/// Unchanged projects are skipped on purpose: they are already registered, and re-writing them
/// would move `last_used_at` on every scan.
///
/// # Errors
///
/// The first [`ScanApplyError`] the registry reports; earlier rows stay applied, as a diff is
/// idempotent and the next scan recomputes it.
pub fn apply_diff(
    diff: &ProjectDiff,
    registry: &mut dyn ProjectRegistry,
) -> Result<usize, ScanApplyError> {
    let mut applied = 0;
    for project in &diff.new {
        registry.insert_discovered(project)?;
        applied += 1;
    }
    for path in &diff.missing {
        registry.mark_missing(path)?;
        applied += 1;
    }
    Ok(applied)
}

/// Accumulates one scan.
struct ScanState {
    projects: Vec<DiscoveredProject>,
    seen_projects: BTreeSet<PathBuf>,
    warnings: Vec<ScanWarning>,
    visited: usize,
    progress_every: usize,
    skipped_directories: usize,
    skipped_symlinks: usize,
    depth_limited: usize,
    cancelled: bool,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            seen_projects: BTreeSet::new(),
            warnings: Vec::new(),
            visited: 0,
            progress_every: DEFAULT_PROGRESS_EVERY,
            skipped_directories: 0,
            skipped_symlinks: 0,
            depth_limited: 0,
            cancelled: false,
        }
    }
}

impl ScanState {
    /// Walks one canonical root iteratively, never recursing and never following a symlink.
    fn walk(&mut self, config: &ScanConfig, observer: &dyn ScanObserver, root: &Path) {
        let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
        while let Some((directory, depth)) = stack.pop() {
            if observer.is_cancelled() {
                self.cancelled = true;
                return;
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    self.warn(&directory, &error);
                    continue;
                }
            };
            let mut markers: Vec<ProjectMarker> = Vec::new();
            let mut children: Vec<(PathBuf, usize)> = Vec::new();
            for entry in entries {
                if observer.is_cancelled() {
                    self.cancelled = true;
                    return;
                }
                self.visited += 1;
                if self.visited.is_multiple_of(self.progress_every) {
                    observer.progress(&ScanProgress {
                        visited: self.visited,
                        projects_found: self.projects.len(),
                        current: directory.clone(),
                    });
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.warn(&directory, &error);
                        continue;
                    }
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(marker) = ProjectMarker::from_entry(&name) {
                    markers.push(marker);
                }
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) => {
                        self.warn(&entry.path(), &error);
                        continue;
                    }
                };
                if file_type.is_symlink() {
                    self.skipped_symlinks += 1;
                    continue;
                }
                if !file_type.is_dir() {
                    continue;
                }
                if is_skipped_directory(&name) {
                    self.skipped_directories += 1;
                    continue;
                }
                if depth + 1 > config.max_depth() {
                    self.depth_limited += 1;
                    continue;
                }
                children.push((entry.path(), depth + 1));
            }
            if !markers.is_empty() {
                markers.sort_unstable();
                markers.dedup();
                let path = canonical_or(&directory);
                if self.seen_projects.insert(path.clone()) {
                    self.projects.push(DiscoveredProject {
                        name: path.file_name().map_or_else(
                            || path.to_string_lossy().into_owned(),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                        path: path.clone(),
                        source_root: root.to_path_buf(),
                        depth,
                        markers,
                    });
                }
                // A detected project is not descended into: nested repositories collapse into
                // the shallowest match.
            } else {
                stack.extend(children);
            }
        }
    }

    /// Records a path the scan could not read.
    fn warn(&mut self, path: &Path, error: &io::Error) {
        let kind = match error.kind() {
            io::ErrorKind::PermissionDenied => ScanWarningKind::PermissionDenied,
            io::ErrorKind::NotFound => ScanWarningKind::MissingRoot,
            io::ErrorKind::NotADirectory => ScanWarningKind::NotADirectory,
            _ => ScanWarningKind::Unreadable,
        };
        self.warnings.push(ScanWarning {
            path: path.to_path_buf(),
            kind,
            message: error.to_string(),
        });
    }

    /// Sorts and dedupes the accumulated result.
    fn finish(mut self) -> ScanOutcome {
        self.projects
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.projects
            .dedup_by(|left, right| left.path == right.path);
        self.warnings
            .sort_by(|left, right| (&left.path, left.kind).cmp(&(&right.path, right.kind)));
        self.warnings
            .dedup_by(|left, right| left.path == right.path && left.kind == right.kind);
        ScanOutcome {
            projects: self.projects,
            warnings: self.warnings,
            visited: self.visited,
            skipped_directories: self.skipped_directories,
            skipped_symlinks: self.skipped_symlinks,
            depth_limited: self.depth_limited,
            cancelled: self.cancelled,
        }
    }
}

/// Whether the walk refuses to descend into a directory of this name.
fn is_skipped_directory(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lowered = name.to_ascii_lowercase();
    SKIPPED_DIR_NAMES.contains(&lowered.as_str())
}

/// Canonicalises when possible so nested and symlinked spellings compare equal.
fn canonical_or(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_and_heavy_directories_are_skipped() {
        for name in [".venv", ".git", ".next", "node_modules", "target", "dist"] {
            assert!(is_skipped_directory(name), "{name} must be skipped");
        }
        for name in ["src", "projects", "my-app"] {
            assert!(!is_skipped_directory(name), "{name} must be walked");
        }
    }

    #[test]
    fn a_marker_maps_back_to_its_entry_name() {
        for name in PROJECT_MARKERS {
            let marker = ProjectMarker::from_entry(name).expect("marker is known");
            assert_eq!(marker.entry_name(), name);
        }
        assert!(ProjectMarker::from_entry("README.md").is_none());
    }

    #[test]
    fn the_depth_bound_is_clamped() {
        assert_eq!(ScanConfig::default().with_max_depth(0).max_depth(), 1);
        assert_eq!(
            ScanConfig::default()
                .with_max_depth(MAX_SCAN_DEPTH + 5)
                .max_depth(),
            MAX_SCAN_DEPTH
        );
        assert_eq!(
            ScanConfig::default()
                .with_progress_every(0)
                .progress_every(),
            1
        );
    }
}
