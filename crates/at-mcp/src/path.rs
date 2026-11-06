//! `PATH` discovery and command resolution for processes launched by the desktop app.
//!
//! A Finder-launched macOS app inherits the minimal `PATH` of `launchd`, not the one the
//! user's terminal has. `npx`, `uvx` and version-manager shims then "work in the terminal
//! but not in the app". [`PathResolver`] fixes that:
//!
//! 1. Ask the login shell once: `$SHELL -l -c 'printf %s "$PATH"'`, with a hard 2 s
//!    timeout and a 64 KiB output bound. The shell string is a compile-time constant
//!    passed as one argument; no untrusted value is ever interpolated into it.
//! 2. Merge the answer with the well-known directories in [`WELL_KNOWN_DIRS`].
//! 3. Cache the result for 60 s so a screen with twenty servers does not fork twenty
//!    login shells.
//! 4. On timeout, non-zero exit or empty output, fall back to the well-known list and
//!    mark the result [`ResolvedPath::degraded`] instead of pretending it worked.
//!
//! [`PathResolver::which`] returns the absolute path plus a [`PathSource`] tag and keeps
//! "exists but is not executable" ([`ResolveError::NotExecutable`]) distinct from "does
//! not exist" ([`ResolveError::NotFound`]). When a Node binary is missing from `PATH` but
//! present in an `nvm`, `fnm`, Volta, `asdf` or `mise` layout, `NotFound` carries a
//! [`ManagerHint`] naming the manager and version directory.
//!
//! Spawns must use the resolved path, not the bare command: [`PathResolver::command`]
//! builds a [`Command`] from the same lookup the health check used, so "the check passed
//! but the spawn failed" cannot happen because of `PATH`.
//!
//! Windows search rules are out of scope; [`WindowsPlatform`] is a stub whose
//! `is_supported` returns `false`, so resolution reports itself as unavailable rather
//! than picking up the wrong rules.

use std::collections::HashSet;
use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the login-shell probe may run before it is killed.
pub const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a search-path result is reused before the login shell is asked again.
pub const SEARCH_PATH_CACHE_TTL: Duration = Duration::from_secs(60);

/// Maximum number of bytes read from the login shell's standard output.
pub const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;

/// Directories every macOS GUI process needs but usually lacks.
///
/// The order is the merge order used when a login-shell answer is combined with the
/// fallback list.
pub const WELL_KNOWN_DIRS: [&str; 6] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// The login-shell command. Constant on purpose: nothing untrusted reaches the shell.
const LOGIN_SHELL_ARGS: [&str; 3] = ["-l", "-c", "printf %s \"$PATH\""];

/// Poll interval while waiting for the probe to exit.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Extra time allowed for the pipe reader after the child exited or was killed.
const PROBE_READ_GRACE: Duration = Duration::from_millis(250);

/// The well-known directory list as owned paths.
#[must_use]
pub fn default_search_dirs() -> Vec<PathBuf> {
    WELL_KNOWN_DIRS.iter().map(PathBuf::from).collect()
}

/// Where a directory in a resolved search path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathSource {
    /// The user's login shell (`$SHELL -l -c 'printf %s "$PATH"'`).
    LoginShell,
    /// The built-in well-known list ([`WELL_KNOWN_DIRS`]).
    Defaults,
    /// The caller passed a path (`/opt/homebrew/bin/npx`) instead of a bare name.
    Explicit,
}

impl PathSource {
    /// Stable tag for logs and diagnostics (`login_shell`, `defaults`, `explicit`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LoginShell => "login_shell",
            Self::Defaults => "defaults",
            Self::Explicit => "explicit",
        }
    }
}

impl fmt::Display for PathSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One directory in the search path, tagged with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchDir {
    /// Absolute directory.
    pub path: PathBuf,
    /// Where the entry came from.
    pub source: PathSource,
}

/// The result of PATH discovery, whether the login shell answered or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// Directories in lookup order, with duplicates removed.
    pub dirs: Vec<SearchDir>,
    /// Source of the list as a whole.
    pub source: PathSource,
    /// True when the login shell failed and the fallback list is being used.
    pub degraded: bool,
    /// Human-readable reason for `degraded`, suitable for a diagnostics panel.
    pub detail: Option<String>,
}

impl ResolvedPath {
    /// The bare directories, in lookup order.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.dirs.iter().map(|dir| dir.path.as_path())
    }

    /// Whether `dir` is part of this search path.
    #[must_use]
    pub fn contains(&self, dir: &Path) -> bool {
        self.dirs.iter().any(|candidate| candidate.path == dir)
    }
}

/// A resolved executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLocation {
    /// Absolute path to the executable.
    pub path: PathBuf,
    /// Which part of the search path supplied it.
    pub source: PathSource,
}

/// A Node.js version manager whose layout can hold an otherwise missing command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VersionManager {
    /// `~/.nvm/versions/node/<version>/bin`.
    Nvm,
    /// `~/.local/share/fnm/node-versions/<version>/installation/bin`.
    Fnm,
    /// `~/.volta/bin` shims.
    Volta,
    /// `~/.asdf/installs/nodejs/<version>/bin` or `~/.asdf/shims`.
    Asdf,
    /// `~/.local/share/mise/installs/node/<version>/bin` or `~/.local/share/mise/shims`.
    Mise,
}

impl VersionManager {
    /// Stable name for logs and hints (`nvm`, `fnm`, `volta`, `asdf`, `mise`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nvm => "nvm",
            Self::Fnm => "fnm",
            Self::Volta => "volta",
            Self::Asdf => "asdf",
            Self::Mise => "mise",
        }
    }

    /// Every manager this resolver knows about, in detection order.
    pub const ALL: [Self; 5] = [Self::Nvm, Self::Fnm, Self::Volta, Self::Asdf, Self::Mise];
}

impl fmt::Display for VersionManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A version-manager location that holds a command missing from `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerHint {
    /// Which manager owns the directory.
    pub manager: VersionManager,
    /// The concrete executable that was found.
    pub executable: PathBuf,
    /// The version directory (absent for shim-only layouts such as Volta).
    pub version_dir: Option<PathBuf>,
    /// Ready-to-display explanation naming the manager and directory.
    pub message: String,
}

/// Why a command could not be resolved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// The command name was empty or contained a NUL byte.
    #[error("`{command}` is not a valid command name: {reason}")]
    InvalidCommand {
        /// What the caller passed.
        command: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// Nothing executable matched.
    #[error("`{command}` not found in PATH ({} directories searched)", .searched.len())]
    NotFound {
        /// What the caller asked for.
        command: String,
        /// Directories that were examined, in order.
        searched: Vec<PathBuf>,
        /// A version-manager location that holds the command, when one exists.
        hint: Option<Box<ManagerHint>>,
    },
    /// A file with the requested name exists but cannot be executed.
    #[error("{}: exists but is not executable", .path.display())]
    NotExecutable {
        /// What the caller asked for.
        command: String,
        /// The existing, non-executable path.
        path: PathBuf,
    },
    /// The configured [`PathPlatform`] has no implementation yet.
    #[error("path resolution is not implemented for this platform")]
    UnsupportedPlatform,
}

impl ResolveError {
    /// The version-manager hint carried by [`ResolveError::NotFound`], if any.
    #[must_use]
    pub fn hint(&self) -> Option<&ManagerHint> {
        match self {
            Self::NotFound { hint, .. } => hint.as_deref(),
            _ => None,
        }
    }
}

/// Platform-specific `PATH` rules.
///
/// Implemented for POSIX today ([`PosixPlatform`]). Windows rules are out of scope for
/// now; [`WindowsPlatform`] is a stub that reports itself unsupported.
pub trait PathPlatform: fmt::Debug + Send + Sync {
    /// Which platform these rules describe.
    fn kind(&self) -> PlatformKind;

    /// Whether resolution may be attempted at all.
    fn is_supported(&self) -> bool {
        true
    }

    /// Directories that belong in the fallback list for this platform.
    fn default_search_dirs(&self) -> Vec<PathBuf>;

    /// Whether `path` is a regular file the current user may execute.
    fn is_executable(&self, path: &Path) -> bool;

    /// Splits a raw `PATH` value into entries, dropping empty ones.
    fn parse_path_list(&self, raw: &str) -> Vec<PathBuf>;
}

/// The platform whose path rules are in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlatformKind {
    /// macOS and Linux.
    Posix,
    /// Windows.
    Windows,
}

/// POSIX rules: `:`-separated `PATH`, executable bit decides runnability.
#[derive(Debug, Default, Clone, Copy)]
pub struct PosixPlatform;

impl PathPlatform for PosixPlatform {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Posix
    }

    fn default_search_dirs(&self) -> Vec<PathBuf> {
        default_search_dirs()
    }

    fn is_executable(&self, path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            match fs::metadata(path) {
                Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
                Err(_) => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    fn parse_path_list(&self, raw: &str) -> Vec<PathBuf> {
        raw.split(':')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect()
    }
}

/// Windows path rules (stub).
///
/// Search rules (`;` separators, `PATHEXT`, `.cmd` shims) are out of scope for TM-014.
/// Every resolver built with this platform reports [`ResolveError::UnsupportedPlatform`]
/// so a stub can never masquerade as working resolution.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsPlatform;

impl PathPlatform for WindowsPlatform {
    fn kind(&self) -> PlatformKind {
        PlatformKind::Windows
    }

    fn is_supported(&self) -> bool {
        false
    }

    fn default_search_dirs(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn is_executable(&self, _path: &Path) -> bool {
        false
    }

    fn parse_path_list(&self, _raw: &str) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Time source used for cache expiry.
///
/// Production uses [`SystemClock`]; tests use [`ManualClock`] so a 60 s cache does not
/// cost a minute of test time.
pub trait Clock: fmt::Debug + Send + Sync {
    /// Monotonic timestamp; only differences are meaningful.
    fn now(&self) -> Duration;
}

/// Monotonic wall clock starting at construction.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Starts the clock now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// Manually advanced clock for tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    elapsed_ms: std::sync::atomic::AtomicU64,
}

impl ManualClock {
    /// Starts at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.elapsed_ms
            .fetch_add(millis, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// Version-manager roots used when looking for a command missing from `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManagerRoots {
    /// `nvm` root (it holds `versions/node/<version>/bin`).
    pub nvm: Option<PathBuf>,
    /// Roots that hold `node-versions/<version>/installation/bin` (`fnm`).
    pub fnm: Vec<PathBuf>,
    /// Volta home (it holds `bin`).
    pub volta: Option<PathBuf>,
    /// `asdf` data directory (it holds `installs/nodejs/<version>/bin` and `shims`).
    pub asdf: Option<PathBuf>,
    /// `mise` data directory (it holds `installs/node/<version>/bin` and `shims`).
    pub mise: Option<PathBuf>,
}

impl ManagerRoots {
    /// Conventional roots under `home`, without consulting the environment.
    ///
    /// Deterministic by design: tests and fixtures get the same answer on every machine.
    #[must_use]
    pub fn under_home(home: &Path) -> Self {
        Self {
            nvm: Some(home.join(".nvm")),
            fnm: vec![home.join(".local/share/fnm"), home.join(".fnm")],
            volta: Some(home.join(".volta")),
            asdf: Some(home.join(".asdf")),
            mise: Some(home.join(".local/share/mise")),
        }
    }

    /// Roots from the environment (`NVM_DIR`, `FNM_DIR`, `VOLTA_HOME`, `ASDF_DATA_DIR`,
    /// `MISE_DATA_DIR`, `XDG_DATA_HOME`), falling back to [`Self::under_home`].
    #[must_use]
    pub fn discover(home: &Path) -> Self {
        let mut roots = Self::under_home(home);
        if let Some(dir) = env_path("NVM_DIR") {
            roots.nvm = Some(dir);
        }
        if let Some(dir) = env_path("FNM_DIR") {
            roots.fnm.insert(0, dir);
        }
        if let Some(dir) = env_path("VOLTA_HOME") {
            roots.volta = Some(dir);
        }
        if let Some(dir) = env_path("ASDF_DATA_DIR") {
            roots.asdf = Some(dir);
        }
        if let Some(dir) = env_path("MISE_DATA_DIR") {
            roots.mise = Some(dir);
        } else if let Some(dir) = env_path("XDG_DATA_HOME") {
            roots.mise = Some(dir.join("mise"));
        }
        roots
    }
}

/// `$SHELL -l -c 'printf %s "$PATH"'` with caching, fallback and command lookup.
#[derive(Debug)]
pub struct PathResolver {
    shell: PathBuf,
    timeout: Duration,
    cache_ttl: Duration,
    home: Option<PathBuf>,
    defaults: Vec<PathBuf>,
    manager_roots: ManagerRoots,
    clock: Arc<dyn Clock>,
    platform: Arc<dyn PathPlatform>,
    cache: Mutex<Option<CacheEntry>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    resolved: ResolvedPath,
    at: Duration,
}

impl PathResolver {
    /// A resolver for this machine: `$SHELL`, 2 s timeout, 60 s cache, POSIX rules.
    ///
    /// Home is not guessed from the environment; use [`Self::for_home`] or
    /// [`PathResolverBuilder::home`] when version-manager detection is wanted. Callers
    /// that own path policy (the app shell, the CLI) pass their `AppPaths` home.
    #[must_use]
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Like [`Self::new`], with an explicit home directory for `~` expansion and
    /// `nvm`/`fnm`/Volta/`asdf`/`mise` detection.
    #[must_use]
    pub fn for_home(home: impl Into<PathBuf>) -> Self {
        Self::builder().home(home).build()
    }

    /// Starts a customised resolver.
    #[must_use]
    pub fn builder() -> PathResolverBuilder {
        PathResolverBuilder::new()
    }

    /// The current search path, from cache when it is younger than the cache TTL.
    ///
    /// Blocks for at most [`LOGIN_SHELL_TIMEOUT`] plus a small read grace on a cold cache.
    /// Call it from a blocking context (or `spawn_blocking`), not on an async worker.
    #[must_use]
    pub fn search_path(&self) -> ResolvedPath {
        let now = self.clock.now();
        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = cache.as_ref()
                && now.saturating_sub(entry.at) < self.cache_ttl
            {
                return entry.resolved.clone();
            }
        }
        let resolved = self.discover();
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = Some(CacheEntry {
            resolved: resolved.clone(),
            at: self.clock.now(),
        });
        resolved
    }

    /// Drops the cached search path; the next call re-runs the login shell.
    pub fn invalidate(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = None;
    }

    /// Resolves `command` to an absolute executable path.
    ///
    /// A command containing `/` is treated as a path and checked directly. Otherwise the
    /// cached search path is walked in order; the first executable wins.
    ///
    /// # Errors
    ///
    /// [`ResolveError::InvalidCommand`], [`ResolveError::NotExecutable`],
    /// [`ResolveError::NotFound`] (with an optional version-manager hint) or
    /// [`ResolveError::UnsupportedPlatform`].
    pub fn which(&self, command: &str) -> Result<CommandLocation, ResolveError> {
        self.validate_command(command)?;
        if !self.platform.is_supported() {
            return Err(ResolveError::UnsupportedPlatform);
        }
        if command.contains('/') {
            return self.locate_explicit(command);
        }
        let search = self.search_path();
        let mut blocked: Option<PathBuf> = None;
        for dir in &search.dirs {
            let candidate = dir.path.join(command);
            if self.platform.is_executable(&candidate) {
                return Ok(CommandLocation {
                    path: candidate,
                    source: dir.source,
                });
            }
            if blocked.is_none() && candidate.exists() {
                blocked = Some(candidate);
            }
        }
        if let Some(path) = blocked {
            return Err(ResolveError::NotExecutable {
                command: command.to_owned(),
                path,
            });
        }
        Err(ResolveError::NotFound {
            command: command.to_owned(),
            searched: search.paths().map(Path::to_path_buf).collect(),
            hint: self.manager_hint(command).map(Box::new),
        })
    }

    /// A [`Command`] whose program is the resolved absolute path.
    ///
    /// Use this for spawns so the process that runs is the one [`Self::which`] reported,
    /// even when the command is only reachable through a version-manager directory that
    /// the app's own `PATH` does not contain.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::which`] returns.
    pub fn command(&self, command: &str) -> Result<Command, ResolveError> {
        let location = self.which(command)?;
        Ok(Command::new(location.path))
    }

    /// Looks for `command` in `nvm`, `fnm`, Volta, `asdf` and `mise` layouts.
    ///
    /// Returns `None` unless an executable actually exists at one of those locations.
    #[must_use]
    pub fn manager_hint(&self, command: &str) -> Option<ManagerHint> {
        if command.is_empty() || command.contains('/') {
            return None;
        }
        let roots = &self.manager_roots;
        if let Some(root) = &roots.nvm
            && let Some(hint) = self.versioned_hint(
                VersionManager::Nvm,
                &root.join("versions/node"),
                "bin",
                command,
            )
        {
            return Some(hint);
        }
        for root in &roots.fnm {
            if let Some(hint) = self.versioned_hint(
                VersionManager::Fnm,
                &root.join("node-versions"),
                "installation/bin",
                command,
            ) {
                return Some(hint);
            }
        }
        if let Some(root) = &roots.volta
            && let Some(hint) = self.direct_hint(VersionManager::Volta, &root.join("bin"), command)
        {
            return Some(hint);
        }
        if let Some(root) = &roots.asdf {
            if let Some(hint) = self.versioned_hint(
                VersionManager::Asdf,
                &root.join("installs/nodejs"),
                "bin",
                command,
            ) {
                return Some(hint);
            }
            if let Some(hint) = self.direct_hint(VersionManager::Asdf, &root.join("shims"), command)
            {
                return Some(hint);
            }
        }
        if let Some(root) = &roots.mise {
            if let Some(hint) = self.versioned_hint(
                VersionManager::Mise,
                &root.join("installs/node"),
                "bin",
                command,
            ) {
                return Some(hint);
            }
            if let Some(hint) = self.direct_hint(VersionManager::Mise, &root.join("shims"), command)
            {
                return Some(hint);
            }
        }
        None
    }

    fn validate_command(&self, command: &str) -> Result<(), ResolveError> {
        let reason = if command.is_empty() {
            Some("empty")
        } else if command.contains('\0') {
            Some("contains a NUL byte")
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ResolveError::InvalidCommand {
                command: command.to_owned(),
                reason,
            }),
            None => Ok(()),
        }
    }

    fn locate_explicit(&self, command: &str) -> Result<CommandLocation, ResolveError> {
        let path = absolutize(Path::new(command));
        if self.platform.is_executable(&path) {
            return Ok(CommandLocation {
                path,
                source: PathSource::Explicit,
            });
        }
        if path.exists() {
            return Err(ResolveError::NotExecutable {
                command: command.to_owned(),
                path,
            });
        }
        Err(ResolveError::NotFound {
            command: command.to_owned(),
            searched: vec![path],
            hint: None,
        })
    }

    fn discover(&self) -> ResolvedPath {
        if !self.platform.is_supported() {
            return self.fallback(Some(
                "path resolution is not implemented for this platform".to_owned(),
            ));
        }
        match self.probe_login_shell() {
            Ok(raw) => {
                let mut dirs: Vec<SearchDir> = Vec::new();
                let mut seen: HashSet<PathBuf> = HashSet::new();
                for entry in self.platform.parse_path_list(&raw) {
                    let path = self.expand_tilde(&entry);
                    if seen.insert(path.clone()) {
                        dirs.push(SearchDir {
                            path,
                            source: PathSource::LoginShell,
                        });
                    }
                }
                if dirs.is_empty() {
                    return self.fallback(Some("login shell printed no PATH".to_owned()));
                }
                for default in &self.defaults {
                    if seen.insert(default.clone()) {
                        dirs.push(SearchDir {
                            path: default.clone(),
                            source: PathSource::Defaults,
                        });
                    }
                }
                ResolvedPath {
                    dirs,
                    source: PathSource::LoginShell,
                    degraded: false,
                    detail: None,
                }
            }
            Err(failure) => self.fallback(Some(failure.detail(self.timeout))),
        }
    }

    fn fallback(&self, detail: Option<String>) -> ResolvedPath {
        ResolvedPath {
            dirs: self
                .defaults
                .iter()
                .cloned()
                .map(|path| SearchDir {
                    path,
                    source: PathSource::Defaults,
                })
                .collect(),
            source: PathSource::Defaults,
            degraded: true,
            detail,
        }
    }

    fn expand_tilde(&self, entry: &Path) -> PathBuf {
        let Some(home) = &self.home else {
            return entry.to_path_buf();
        };
        if entry == Path::new("~") {
            return home.clone();
        }
        match entry.strip_prefix("~/") {
            Ok(rest) => home.join(rest),
            Err(_) => entry.to_path_buf(),
        }
    }

    fn probe_login_shell(&self) -> Result<String, ProbeFailure> {
        let mut child = Command::new(&self.shell)
            .args(LOGIN_SHELL_ARGS)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ProbeFailure::from)?;
        let stdout = child.stdout.take();

        // Read the pipe on a helper thread so a grandchild that inherits stdout cannot
        // keep the probe alive past its timeout. Bounded to one byte over the limit so
        // "too much output" is detectable without buffering it all.
        let (sender, receiver) = mpsc::channel();
        match stdout {
            Some(mut stdout) => {
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let read = stdout
                        .by_ref()
                        .take(MAX_PROBE_OUTPUT_BYTES as u64 + 1)
                        .read_to_end(&mut buf)
                        .map(|_| buf);
                    let _ = sender.send(read);
                });
            }
            None => {
                let _ = sender.send(Ok(Vec::new()));
            }
        }

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(PROBE_POLL_INTERVAL);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProbeFailure::from(error));
                }
            }
        };

        let bytes = match receiver.recv_timeout(PROBE_READ_GRACE) {
            Ok(Ok(buf)) => buf,
            Ok(Err(error)) => return Err(ProbeFailure::from(error)),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Vec::new(),
        };

        let Some(status) = status else {
            return Err(ProbeFailure::Timeout);
        };
        if bytes.len() > MAX_PROBE_OUTPUT_BYTES {
            return Err(ProbeFailure::OutputTooLarge);
        }
        if !status.success() {
            return Err(ProbeFailure::ShellFailed(status.code()));
        }
        parse_probe_output(&bytes).ok_or(ProbeFailure::EmptyOutput)
    }

    fn versioned_hint(
        &self,
        manager: VersionManager,
        versions_root: &Path,
        leaf: &str,
        command: &str,
    ) -> Option<ManagerHint> {
        for version_dir in version_dirs(versions_root) {
            let executable = version_dir.join(leaf).join(command);
            if self.platform.is_executable(&executable) {
                return Some(ManagerHint {
                    manager,
                    message: hint_message(manager, command, &executable, Some(&version_dir)),
                    executable,
                    version_dir: Some(version_dir),
                });
            }
        }
        None
    }

    fn direct_hint(
        &self,
        manager: VersionManager,
        bin_dir: &Path,
        command: &str,
    ) -> Option<ManagerHint> {
        let executable = bin_dir.join(command);
        if self.platform.is_executable(&executable) {
            return Some(ManagerHint {
                manager,
                message: hint_message(manager, command, &executable, None),
                executable,
                version_dir: None,
            });
        }
        None
    }
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a [`PathResolver`]; unset fields use production defaults.
#[derive(Debug, Clone)]
pub struct PathResolverBuilder {
    shell: PathBuf,
    timeout: Duration,
    cache_ttl: Duration,
    home: Option<PathBuf>,
    manager_roots: Option<ManagerRoots>,
    defaults: Option<Vec<PathBuf>>,
    clock: Option<Arc<dyn Clock>>,
    platform: Option<Arc<dyn PathPlatform>>,
}

impl PathResolverBuilder {
    /// Defaults: `$SHELL` (or `/bin/sh`), 2 s timeout, 60 s cache, POSIX, real clock.
    ///
    /// No home directory is assumed; inject one with [`Self::home`] so path policy stays
    /// with `AppPaths` instead of being read from the environment here.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shell: default_shell(),
            timeout: LOGIN_SHELL_TIMEOUT,
            cache_ttl: SEARCH_PATH_CACHE_TTL,
            home: None,
            manager_roots: None,
            defaults: None,
            clock: None,
            platform: None,
        }
    }

    /// Shell used for the probe.
    #[must_use]
    pub fn shell(mut self, shell: impl Into<PathBuf>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Hard probe timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How long a discovery result is reused.
    #[must_use]
    pub fn cache_ttl(mut self, cache_ttl: Duration) -> Self {
        self.cache_ttl = cache_ttl;
        self
    }

    /// Home directory used for `~` expansion and manager detection.
    #[must_use]
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Explicit version-manager roots (avoids environment lookups).
    #[must_use]
    pub fn manager_roots(mut self, roots: ManagerRoots) -> Self {
        self.manager_roots = Some(roots);
        self
    }

    /// Replaces the well-known fallback list.
    #[must_use]
    pub fn default_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.defaults = Some(dirs);
        self
    }

    /// Replaces the clock (tests).
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Replaces the platform rules.
    #[must_use]
    pub fn platform(mut self, platform: Arc<dyn PathPlatform>) -> Self {
        self.platform = Some(platform);
        self
    }

    /// Finishes the resolver.
    #[must_use]
    pub fn build(self) -> PathResolver {
        let platform = self
            .platform
            .unwrap_or_else(|| Arc::new(PosixPlatform) as Arc<dyn PathPlatform>);
        let defaults = self
            .defaults
            .unwrap_or_else(|| platform.default_search_dirs());
        let manager_roots = self.manager_roots.unwrap_or_else(|| match &self.home {
            Some(home) => ManagerRoots::discover(home),
            None => ManagerRoots::default(),
        });
        PathResolver {
            shell: self.shell,
            timeout: self.timeout,
            cache_ttl: self.cache_ttl,
            home: self.home,
            defaults,
            manager_roots,
            clock: self
                .clock
                .unwrap_or_else(|| Arc::new(SystemClock::new()) as Arc<dyn Clock>),
            platform,
            cache: Mutex::new(None),
        }
    }
}

impl Default for PathResolverBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
enum ProbeFailure {
    Spawn(std::io::ErrorKind),
    Timeout,
    ShellFailed(Option<i32>),
    OutputTooLarge,
    EmptyOutput,
}

impl ProbeFailure {
    fn detail(self, timeout: Duration) -> String {
        match self {
            Self::Spawn(kind) => format!("login shell could not run: {kind}"),
            Self::Timeout => format!(
                "login shell did not finish within {:.1}s",
                timeout.as_secs_f64()
            ),
            Self::ShellFailed(Some(code)) => format!("login shell exited with status {code}"),
            Self::ShellFailed(None) => "login shell was terminated by a signal".to_owned(),
            Self::OutputTooLarge => {
                format!("login shell printed more than {MAX_PROBE_OUTPUT_BYTES} bytes")
            }
            Self::EmptyOutput => "login shell printed no PATH".to_owned(),
        }
    }
}

impl From<std::io::Error> for ProbeFailure {
    fn from(error: std::io::Error) -> Self {
        Self::Spawn(error.kind())
    }
}

/// Extracts the `PATH` value from probe output, skipping login banners.
///
/// `printf` emits no trailing newline, but a `.zshrc` may print several lines first. The
/// last line that looks like a `PATH` list wins; if none does, the last non-empty line is
/// used so a single-directory value still resolves.
fn parse_probe_output(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut last_non_empty = None;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains(':') && line.contains('/') {
            return Some(line.to_owned());
        }
        if last_non_empty.is_none() {
            last_non_empty = Some(line.to_owned());
        }
    }
    last_non_empty
}

/// Version directories under `root`, newest first.
fn version_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort_by(|left, right| {
        let left = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let right = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        version_cmp(left, right).reverse()
    });
    dirs
}

/// Compares version-ish names segment by segment so `v20` sorts after `v9`.
fn version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left = version_segments(left);
    let right = version_segments(right);
    for (left_segment, right_segment) in left.iter().zip(right.iter()) {
        let ordering = match (left_segment.parse::<u64>(), right_segment.parse::<u64>()) {
            (Ok(left_number), Ok(right_number)) => left_number.cmp(&right_number),
            _ => left_segment.cmp(right_segment),
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// Splits `v20.11.1` into `["v", "20", "11", "1"]` so numeric runs compare as numbers.
fn version_segments(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut previous_was_digit: Option<bool> = None;
    for (index, character) in text.char_indices() {
        if !character.is_ascii_alphanumeric() {
            if start < index {
                segments.push(&text[start..index]);
            }
            start = index + character.len_utf8();
            previous_was_digit = None;
            continue;
        }
        let is_digit = character.is_ascii_digit();
        if let Some(previous) = previous_was_digit
            && previous != is_digit
        {
            segments.push(&text[start..index]);
            start = index;
        }
        previous_was_digit = Some(is_digit);
    }
    if start < text.len() {
        segments.push(&text[start..]);
    }
    segments
}

fn hint_message(
    manager: VersionManager,
    command: &str,
    executable: &Path,
    version_dir: Option<&Path>,
) -> String {
    match version_dir {
        Some(dir) => format!(
            "`{command}` is not on PATH; {manager} provides {} (version directory {}); add that directory to PATH or use the absolute path",
            executable.display(),
            dir.display()
        ),
        None => format!(
            "`{command}` is not on PATH; {manager} shims it at {}; add that binary directory to PATH or use the absolute path",
            executable.display()
        ),
    }
}

fn default_shell() -> PathBuf {
    env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Makes a caller-supplied path absolute without resolving symlinks, so the path the
/// caller sees is the path that is executed.
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_dirs_are_in_documented_order() {
        let dirs = default_search_dirs();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/usr/sbin"),
                PathBuf::from("/sbin"),
            ]
        );
    }

    #[test]
    fn path_source_tags_are_stable() {
        assert_eq!(PathSource::LoginShell.as_str(), "login_shell");
        assert_eq!(PathSource::Defaults.as_str(), "defaults");
        assert_eq!(PathSource::Explicit.as_str(), "explicit");
    }

    #[test]
    fn probe_output_prefers_last_path_like_line() {
        assert_eq!(
            parse_probe_output(b"Welcome back\n/usr/bin:/bin\n").as_deref(),
            Some("/usr/bin:/bin")
        );
        assert_eq!(
            parse_probe_output(b"/only/dir").as_deref(),
            Some("/only/dir")
        );
        assert_eq!(parse_probe_output(b"\n \n"), None);
    }

    #[test]
    fn versions_sort_numerically_not_lexicographically() {
        assert_eq!(
            version_cmp("v20.1.0", "v9.11.1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(version_cmp("v20", "v20"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn tilde_entries_expand_against_home() {
        let resolver = PathResolver::builder()
            .home("/Users/example")
            .manager_roots(ManagerRoots::default())
            .default_dirs(Vec::new())
            .build();
        assert_eq!(
            resolver.expand_tilde(Path::new("~/bin")),
            PathBuf::from("/Users/example/bin")
        );
        assert_eq!(
            resolver.expand_tilde(Path::new("~")),
            PathBuf::from("/Users/example")
        );
        assert_eq!(
            resolver.expand_tilde(Path::new("/absolute/bin")),
            PathBuf::from("/absolute/bin")
        );
    }

    #[test]
    fn windows_platform_is_an_unsupported_stub() {
        let platform = WindowsPlatform;
        assert!(!platform.is_supported());
        assert!(platform.default_search_dirs().is_empty());
        let resolver = PathResolver::builder()
            .platform(Arc::new(WindowsPlatform))
            .build();
        match resolver.which("npx") {
            Err(ResolveError::UnsupportedPlatform) => {}
            other => panic!("expected UnsupportedPlatform, got {other:?}"),
        }
    }
}
