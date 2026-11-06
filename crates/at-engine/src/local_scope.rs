//! The Local scope: machine-local overrides stored inside the project store.
//!
//! Local is a real, writable fourth scope (integration decisions 3 and 4). Definitions live
//! in `<project>/.agentuna/local/` and win over Project definitions with the same logical
//! id, because [`resolve`] orders layers by
//! [`Scope::precedence`](at_model::Scope::precedence).
//! Rendering into agent-local files (`CLAUDE.local.md`, `.claude/settings.local.json`, the
//! project skills folder) is owned by the agent adapters; this module owns the store layout,
//! the availability gate and the Git bookkeeping around it.
//!
//! # Availability
//!
//! Local content is only meaningful inside a Git work tree, because "uncommitted" is the
//! property that distinguishes it from Project content. [`LocalScope::detect`] reports
//! [`LocalScopeStatus::NotAvailable`] when the project root is not inside a work tree, and
//! [`resolve_with_local`] then resolves the remaining Global/User/Project layers exactly as
//! before while still reporting the status to the caller.
//!
//! # Git bookkeeping
//!
//! Two files, both edited through the `at-fs` write pipeline:
//!
//! * the project `.gitignore` gets one `.agentuna/local/` line ([`ensure_gitignore`]);
//! * `.git/info/exclude` gets one line per deployed Local path ([`ensure_git_exclude`]).
//!
//! `.git/info/exclude` is a local ignore list only. It does **not** untrack files Git already
//! tracks and it does **not** isolate a path from any agent: an agent that reads the deployed
//! folder still sees it. Callers must refuse a Local deployment over a tracked Project path
//! and require an explicit plan instead of pretending the exclude entry makes it safe.
//! This module never edits an existing tracked file's index state and never runs `git`.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use at_fs::{ExpectedState, MAX_FILE_BYTES, WriteRequest, apply, sha256_hex};
use at_model::{Definition, ItemKind, Scope, ScopeSnapshot};
use at_store::layout::Slug;
use at_store::local_store::LocalStorePaths;
use at_store::paths::STORE_DIR_NAME;

use crate::locks::LockStore;
use crate::resolve::{ResolveInput, ResolvedSet, resolve};

/// Line added to the project `.gitignore` so Local content is never committed.
pub const LOCAL_IGNORE_LINE: &str = ".agentuna/local/";

/// Pattern compared against existing `.gitignore` entries (trailing slash optional there).
const LOCAL_IGNORE_PATTERN: &str = ".agentuna/local";

/// Why the Local scope is not usable for the current project.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalUnavailableReason {
    /// The project root is not inside a Git work tree.
    NotAGitRepository,
}

impl LocalUnavailableReason {
    /// Stable identifier for logs, diagnostics and the UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAGitRepository => "not_a_git_repository",
        }
    }
}

impl fmt::Display for LocalUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAGitRepository => f.write_str("the project is not a Git repository"),
        }
    }
}

/// Whether the Local layer participates in resolution.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalScopeStatus {
    /// The project is inside a Git work tree; Local definitions apply.
    Available,
    /// The Local layer is skipped and the status is surfaced to the caller.
    NotAvailable {
        /// Why Local content cannot be used.
        reason: LocalUnavailableReason,
    },
}

impl LocalScopeStatus {
    /// Whether Local definitions apply.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// The reason Local is unavailable, when it is.
    #[must_use]
    pub const fn reason(self) -> Option<LocalUnavailableReason> {
        match self {
            Self::Available => None,
            Self::NotAvailable { reason } => Some(reason),
        }
    }
}

/// The Git directory a project belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitRepository {
    /// Directory holding `HEAD` for this work tree (`.git` or the `gitdir:` target).
    git_dir: PathBuf,
    /// Directory shared with the main work tree, which owns `info/exclude`.
    common_dir: PathBuf,
}

/// The Local scope of one project: its store layout and its Git work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalScope {
    project_root: PathBuf,
    store: LocalStorePaths,
    repository: Option<GitRepository>,
}

impl LocalScope {
    /// Detects the Local scope of `project_root`.
    ///
    /// `store` is the project's Local store layout, built with
    /// [`LocalStorePaths::from_project`] from the canonical
    /// [`at_store::layout::StoreLayout`] or with [`LocalStorePaths::for_paths`] from
    /// [`at_store::AppPaths`]. Only the store layout chooses where Local content lives.
    ///
    /// Detection never fails: a missing work tree is a status, not an error.
    #[must_use]
    pub fn detect(project_root: impl Into<PathBuf>, store: LocalStorePaths) -> Self {
        let project_root = project_root.into();
        let repository = find_repository(&project_root);
        Self {
            project_root,
            store,
            repository,
        }
    }

    /// The project root this Local scope belongs to.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The Local store layout.
    #[must_use]
    pub fn store(&self) -> &LocalStorePaths {
        &self.store
    }

    /// The work tree's Git directory, when the project is inside a Git repository.
    #[must_use]
    pub fn git_dir(&self) -> Option<&Path> {
        self.repository.as_ref().map(|repo| repo.git_dir.as_path())
    }

    /// The `info/exclude` file that applies to this work tree.
    #[must_use]
    pub fn exclude_path(&self) -> Option<PathBuf> {
        self.repository
            .as_ref()
            .map(|repo| repo.common_dir.join("info").join("exclude"))
    }

    /// Whether Local definitions apply to this project.
    #[must_use]
    pub fn status(&self) -> LocalScopeStatus {
        match self.repository {
            Some(_) => LocalScopeStatus::Available,
            None => LocalScopeStatus::NotAvailable {
                reason: LocalUnavailableReason::NotAGitRepository,
            },
        }
    }

    /// The store path of a Local item.
    ///
    /// The result is asserted to stay inside [`LocalStorePaths::root`], so a Local write can
    /// never be redirected to the User or Global store by a hostile slug.
    ///
    /// # Errors
    /// [`LocalScopeError::InvalidSlug`] when [`Slug::parse`] rejects the slug or the resulting
    /// path would leave the Local store.
    pub fn item_path(&self, kind: ItemKind, slug: &str) -> Result<PathBuf, LocalScopeError> {
        let slug = Slug::parse(slug).map_err(|source| LocalScopeError::InvalidSlug {
            slug: slug.to_owned(),
            kind,
            source,
        })?;
        let path = self.store.item_path(kind, &slug);
        if !self.store.contains(&path) {
            return Err(LocalScopeError::OutsideLocalStore {
                path,
                root: self.store.root().to_path_buf(),
            });
        }
        Ok(path)
    }

    /// Appends the `.agentuna/local/` line to this project's `.gitignore`.
    ///
    /// # Errors
    /// [`LocalScopeError`] when the file cannot be read or written safely.
    pub fn ensure_project_gitignore(&self) -> Result<GitignoreChange, LocalScopeError> {
        ensure_gitignore(&self.project_root.join(".gitignore"))
    }

    /// Adds one `.git/info/exclude` entry per deployed path.
    ///
    /// Each path must be inside the project root; it is recorded as a root-anchored pattern
    /// (`/<relative path>`). Entries already present are not repeated.
    ///
    /// # Errors
    /// [`LocalScopeError::LocalNotAvailable`] outside a Git work tree,
    /// [`LocalScopeError::OutsideProject`] for a path outside the project root, and write
    /// errors from [`at_fs::apply`].
    pub fn register_deployments(
        &self,
        deployed: &[PathBuf],
    ) -> Result<ExcludeChange, LocalScopeError> {
        let exclude = self
            .exclude_path()
            .ok_or(LocalScopeError::LocalNotAvailable {
                reason: LocalUnavailableReason::NotAGitRepository,
            })?;
        let mut patterns = Vec::with_capacity(deployed.len());
        for path in deployed {
            patterns.push(deployment_pattern(&self.project_root, path)?);
        }
        let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
        ensure_git_exclude(&exclude, &patterns)
    }
}

/// Local definitions plus the scope they belong to.
///
/// The constructor refuses definitions whose path is outside the Local store: a definition
/// loaded from the User or Global store can never be presented as Local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLayer {
    scope: LocalScope,
    definitions: Vec<Definition>,
}

impl LocalLayer {
    /// Binds `definitions` to `scope`.
    ///
    /// # Errors
    /// [`LocalScopeError::OutsideLocalStore`] when any definition points outside
    /// [`LocalStorePaths::root`].
    pub fn new(scope: LocalScope, definitions: Vec<Definition>) -> Result<Self, LocalScopeError> {
        for definition in &definitions {
            if !scope.store.contains(&definition.path) {
                return Err(LocalScopeError::OutsideLocalStore {
                    path: definition.path.clone(),
                    root: scope.store.root().to_path_buf(),
                });
            }
        }
        Ok(Self { scope, definitions })
    }

    /// An empty Local layer for `scope`.
    #[must_use]
    pub fn empty(scope: LocalScope) -> Self {
        Self {
            scope,
            definitions: Vec::new(),
        }
    }

    /// The Local scope these definitions were read from.
    #[must_use]
    pub fn scope(&self) -> &LocalScope {
        &self.scope
    }

    /// Whether the layer participates in resolution.
    #[must_use]
    pub fn status(&self) -> LocalScopeStatus {
        self.scope.status()
    }

    /// The Local definitions.
    #[must_use]
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The Local scope layer, or `None` when the project is not a Git repository.
    #[must_use]
    pub fn snapshot(&self) -> Option<ScopeSnapshot> {
        self.status().is_available().then(|| ScopeSnapshot {
            scope: Scope::Local,
            definitions: self.definitions.clone(),
        })
    }
}

/// The resolved set plus the Local layer status that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalResolution {
    /// Winner per logical id, including Local winners when the layer is available.
    pub resolved: ResolvedSet,
    /// Whether Local took part; reported even when the layer was skipped.
    pub local: LocalScopeStatus,
}

/// Resolves Global/User/Project layers together with the Local layer.
///
/// `base` carries the other scopes in any order. Any Local snapshot inside `base` is ignored:
/// Local definitions are supplied only through `local`, which is the single place the
/// availability gate can be applied. When the project is not a Git repository the Local
/// layer is skipped and [`LocalResolution::local`] says so; the remaining layers resolve
/// exactly as they would without Local.
///
/// This variant enforces no Global locks. A caller that has a lock source must use
/// [`resolve_with_local_and_locks`] instead: a Local override may never win over a
/// Global-locked definition.
#[must_use]
pub fn resolve_with_local(base: &[ScopeSnapshot], local: &LocalLayer) -> LocalResolution {
    resolve_layers(base, local, None)
}

/// Resolves Global/User/Project layers together with the Local layer under Global locks.
///
/// Behaves like [`resolve_with_local`] and additionally attaches `locks` to the resolution,
/// so a definition pinned by a lock wins its logical id even when a Local definition with
/// the same identity exists. Every Local definition that loses to the lock is reported as
/// blocked ([`crate::resolve::Shadowed::blocked`]) rather than merely shadowed.
#[must_use]
pub fn resolve_with_local_and_locks(
    base: &[ScopeSnapshot],
    local: &LocalLayer,
    locks: &dyn LockStore,
) -> LocalResolution {
    resolve_layers(base, local, Some(locks))
}

fn resolve_layers(
    base: &[ScopeSnapshot],
    local: &LocalLayer,
    locks: Option<&dyn LockStore>,
) -> LocalResolution {
    let mut layers: Vec<ScopeSnapshot> = base
        .iter()
        .filter(|snapshot| snapshot.scope != Scope::Local)
        .cloned()
        .collect();
    if let Some(snapshot) = local.snapshot() {
        layers.push(snapshot);
    }
    let input = match locks {
        Some(locks) => ResolveInput::new(&layers).with_locks(locks),
        None => ResolveInput::new(&layers),
    };
    LocalResolution {
        resolved: resolve(input),
        local: local.status(),
    }
}

/// What [`ensure_gitignore`] did.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitignoreChange {
    /// The `.gitignore` file did not exist and was created with the Local line.
    Created,
    /// The Local line was appended to an existing file.
    Appended,
    /// An equivalent ignore pattern was already present; nothing was written.
    Unchanged,
}

impl GitignoreChange {
    /// Whether this call wrote the file.
    #[must_use]
    pub const fn wrote(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// Ensures `gitignore` ignores `.agentuna/local/`.
///
/// A missing file is created with exactly one [`LOCAL_IGNORE_LINE`] line. An existing file is
/// left untouched when any entry already covers the directory — `.agentuna/local/`,
/// `.agentuna/local`, `/.agentuna/local`, `**/.agentuna/local`, `.agentuna/local/**`,
/// `.agentuna/local/*`, or a broader `.agentuna/` entry. Otherwise the line is appended once,
/// preserving the file's line-ending style and creating the separating newline when the file
/// did not end with one.
///
/// The write goes through [`at_fs::apply`], which refuses symlinks and detects a file that
/// changed since it was read.
///
/// # Errors
/// [`LocalScopeError`] for I/O failures, non-UTF-8 content, a symlink or a changed file.
pub fn ensure_gitignore(gitignore: &Path) -> Result<GitignoreChange, LocalScopeError> {
    let Some(existing) = read_optional(gitignore)? else {
        let line = format!("{LOCAL_IGNORE_LINE}\n");
        write_text(gitignore, None, line)?;
        return Ok(GitignoreChange::Created);
    };
    let text = core::str::from_utf8(&existing).map_err(|_| LocalScopeError::NotUtf8 {
        path: gitignore.to_path_buf(),
    })?;
    if text.lines().any(ignores_local_store) {
        return Ok(GitignoreChange::Unchanged);
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut updated = String::with_capacity(text.len() + LOCAL_IGNORE_LINE.len() + 2);
    updated.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        updated.push_str(newline);
    }
    updated.push_str(LOCAL_IGNORE_LINE);
    updated.push_str(newline);
    write_text(gitignore, Some(&existing), updated)?;
    Ok(GitignoreChange::Appended)
}

/// What [`ensure_git_exclude`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeChange {
    /// Whether the exclude file had to be created.
    pub created: bool,
    /// Patterns appended by this call, in request order and without duplicates.
    pub added: Vec<String>,
    /// Requested patterns that were already present (or repeated in the request).
    pub already_present: Vec<String>,
}

impl ExcludeChange {
    /// Whether this call wrote the file.
    #[must_use]
    pub fn wrote(&self) -> bool {
        self.created || !self.added.is_empty()
    }
}

/// Adds `patterns` to a `.git/info/exclude` file, each exactly once.
///
/// Patterns are normalized to a root-anchored form (`/relative/path`) and compared against
/// existing entries ignoring a leading slash, a trailing slash, a trailing `/**` or `/*`, and
/// a leading `**/`, so `/a/b`, `a/b/` and `/a/b/**` are the same entry. Empty, negated,
/// absolute and traversal patterns are rejected rather than guessed at. Comments are skipped.
///
/// The file is written once through [`at_fs::apply`], with the expected hash of the bytes that
/// were read. The file is created when absent and `patterns` is not empty; an empty request
/// touches nothing.
///
/// # Errors
/// [`LocalScopeError`] for I/O failures, non-UTF-8 content, a symlink, an invalid pattern or
/// a changed file.
pub fn ensure_git_exclude(
    exclude: &Path,
    patterns: &[&str],
) -> Result<ExcludeChange, LocalScopeError> {
    let mut requested: Vec<String> = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let normalized = normalize_requested_pattern(pattern)?;
        if normalized.is_empty() {
            continue;
        }
        if requested.iter().any(|known| known == &normalized) {
            continue;
        }
        requested.push(normalized);
    }
    if requested.is_empty() {
        return Ok(ExcludeChange::default());
    }

    let Some(existing) = read_optional(exclude)? else {
        let text = join_lines(&requested);
        write_text(exclude, None, text)?;
        return Ok(ExcludeChange {
            created: true,
            added: requested,
            already_present: Vec::new(),
        });
    };
    let text = core::str::from_utf8(&existing).map_err(|_| LocalScopeError::NotUtf8 {
        path: exclude.to_path_buf(),
    })?;
    let present = effective_existing(text);
    let mut added = Vec::new();
    let mut already_present = Vec::new();
    for pattern in requested {
        if present.iter().any(|known| known == &pattern) {
            already_present.push(pattern);
        } else {
            added.push(pattern);
        }
    }
    if added.is_empty() {
        return Ok(ExcludeChange {
            created: false,
            added,
            already_present,
        });
    }
    let mut updated = String::with_capacity(
        text.len() + added.iter().map(String::len).sum::<usize>() + added.len() + 1,
    );
    updated.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&join_lines(&added));
    write_text(exclude, Some(&existing), updated)?;
    Ok(ExcludeChange {
        created: false,
        added,
        already_present,
    })
}

/// The `.git/info/exclude` pattern for a deployed path: `/` plus its project-relative form.
///
/// # Errors
/// [`LocalScopeError::OutsideProject`] when `deployed` is not inside `project_root` or has
/// no components, [`LocalScopeError::NonUtf8Segment`] for a non-UTF-8 component, and
/// [`LocalScopeError::InvalidExcludePattern`] for a component Git patterns cannot express
/// without escaping.
pub fn deployment_pattern(project_root: &Path, deployed: &Path) -> Result<String, LocalScopeError> {
    let relative =
        deployed
            .strip_prefix(project_root)
            .map_err(|_| LocalScopeError::OutsideProject {
                path: deployed.to_path_buf(),
                root: project_root.to_path_buf(),
            })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| LocalScopeError::NonUtf8Segment {
                        path: deployed.to_path_buf(),
                    })?;
                if part.chars().any(|c| c.is_control() || c.is_whitespace())
                    || part.is_empty()
                    || part.contains('\\')
                {
                    return Err(LocalScopeError::InvalidExcludePattern {
                        pattern: part.to_owned(),
                    });
                }
                parts.push(part);
            }
            Component::CurDir => {}
            _ => {
                return Err(LocalScopeError::OutsideProject {
                    path: deployed.to_path_buf(),
                    root: project_root.to_path_buf(),
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(LocalScopeError::OutsideProject {
            path: deployed.to_path_buf(),
            root: project_root.to_path_buf(),
        });
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Why a Local scope operation did not complete.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LocalScopeError {
    /// Reading or writing a file failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The path exists but is not a regular file (symlinks included).
    #[error("{}: not a regular file", .path.display())]
    NotAFile {
        /// Path involved.
        path: PathBuf,
    },
    /// The path is a symlink; Local bookkeeping files are never followed.
    #[error("{}: refusing to follow a symlink", .path.display())]
    SymlinkRefused {
        /// Path involved.
        path: PathBuf,
    },
    /// The file is larger than [`at_fs::MAX_FILE_BYTES`].
    #[error("{}: {size} bytes exceeds the {limit} byte limit", .path.display())]
    TooLarge {
        /// Path involved.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The file is not UTF-8 text.
    #[error("{}: not valid UTF-8", .path.display())]
    NotUtf8 {
        /// Path involved.
        path: PathBuf,
    },
    /// A slug cannot name a Local item.
    #[error("`{slug}` is not a usable Local {kind} slug: {source}")]
    InvalidSlug {
        /// Rejected slug.
        slug: String,
        /// Item kind it was used for.
        kind: ItemKind,
        /// The store slug rule that rejected it.
        #[source]
        source: at_store::layout::SlugError,
    },
    /// A definition was loaded from outside the Local store.
    #[error("{}: outside the Local store {}", .path.display(), .root.display())]
    OutsideLocalStore {
        /// Definition path.
        path: PathBuf,
        /// Local store root.
        root: PathBuf,
    },
    /// A deployed path is not inside the project root.
    #[error("{}: outside the project root {}", .path.display(), .root.display())]
    OutsideProject {
        /// Deployed path.
        path: PathBuf,
        /// Project root.
        root: PathBuf,
    },
    /// A path component cannot be written as a Git exclude pattern.
    #[error("`{pattern}` cannot be written as a Git exclude pattern")]
    InvalidExcludePattern {
        /// Rejected component.
        pattern: String,
    },
    /// A path component is not valid UTF-8.
    #[error("{}: path component is not valid UTF-8", .path.display())]
    NonUtf8Segment {
        /// Path involved.
        path: PathBuf,
    },
    /// The Local layer cannot be used for this project.
    #[error("the Local scope is not available: {reason}")]
    LocalNotAvailable {
        /// Why Local content cannot be used.
        reason: LocalUnavailableReason,
    },
    /// The guarded write pipeline refused or failed the write.
    #[error(transparent)]
    Write(#[from] at_fs::FsError),
}

/// Whether `line` is a `.gitignore` entry that already ignores `.agentuna/local/`.
fn ignores_local_store(line: &str) -> bool {
    let line = line.trim_end();
    if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
        return false;
    }
    let mut pattern = line;
    while let Some(stripped) = pattern.strip_suffix('/') {
        pattern = stripped;
    }
    if let Some(stripped) = pattern.strip_suffix("/**") {
        pattern = stripped;
    } else if let Some(stripped) = pattern.strip_suffix("/*") {
        pattern = stripped;
    }
    loop {
        if let Some(stripped) = pattern.strip_prefix('/') {
            pattern = stripped;
        } else if let Some(stripped) = pattern.strip_prefix("**/") {
            pattern = stripped;
        } else {
            break;
        }
    }
    pattern == LOCAL_IGNORE_PATTERN || pattern == STORE_DIR_NAME
}

/// Normalizes a requested exclude pattern to `/relative/form`.
fn normalize_requested_pattern(raw: &str) -> Result<String, LocalScopeError> {
    let trimmed = raw.trim();
    let invalid = || LocalScopeError::InvalidExcludePattern {
        pattern: raw.to_owned(),
    };
    if trimmed.is_empty()
        || trimmed.starts_with('!')
        || trimmed.starts_with('#')
        || trimmed.contains('\\')
        || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid());
    }
    let mut body = trimmed;
    while let Some(stripped) = body.strip_prefix('/') {
        body = stripped;
    }
    while let Some(stripped) = body.strip_suffix('/') {
        body = stripped;
    }
    if body.is_empty() {
        return Err(invalid());
    }
    for component in Path::new(body).components() {
        match component {
            Component::Normal(part) if part.to_str().is_some() => {}
            _ => return Err(invalid()),
        }
    }
    Ok(format!("/{body}"))
}

/// The comparable form of one exclude pattern (`/relative/form`), `None` when it is empty.
fn comparable_pattern(pattern: &str) -> Option<String> {
    let mut body = pattern.trim();
    while let Some(stripped) = body.strip_prefix('/') {
        body = stripped;
    }
    while let Some(stripped) = body.strip_suffix('/') {
        body = stripped;
    }
    if let Some(stripped) = body.strip_suffix("/**") {
        body = stripped;
    } else if let Some(stripped) = body.strip_suffix("/*") {
        body = stripped;
    }
    if let Some(stripped) = body.strip_prefix("**/") {
        body = stripped;
    }
    (!body.is_empty()).then(|| format!("/{body}"))
}

/// Patterns the exclude file currently ignores, honoring later negations.
fn effective_existing(text: &str) -> Vec<String> {
    let mut present: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(negated) = trimmed.strip_prefix('!') {
            if let Some(pattern) = comparable_pattern(negated) {
                present.retain(|known| known != &pattern);
            }
        } else if let Some(pattern) = comparable_pattern(trimmed)
            && !present.contains(&pattern)
        {
            present.push(pattern);
        }
    }
    present
}

fn join_lines(lines: &[String]) -> String {
    let mut out = String::with_capacity(lines.iter().map(String::len).sum::<usize>() + lines.len());
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, LocalScopeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LocalScopeError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(LocalScopeError::SymlinkRefused {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(LocalScopeError::NotAFile {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(LocalScopeError::TooLarge {
            path: path.to_path_buf(),
            size: metadata.len(),
            limit: MAX_FILE_BYTES,
        });
    }
    fs::read(path)
        .map(Some)
        .map_err(|source| LocalScopeError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn write_text(path: &Path, existing: Option<&[u8]>, text: String) -> Result<(), LocalScopeError> {
    let request = match existing {
        None => WriteRequest::create(path, text.into_bytes()),
        Some(bytes) => WriteRequest::replace(
            path,
            text.into_bytes(),
            ExpectedState::Hash(sha256_hex(bytes)),
        ),
    };
    apply(&request)?;
    Ok(())
}

/// Finds the Git work tree containing `project_root`, searching its ancestors.
fn find_repository(project_root: &Path) -> Option<GitRepository> {
    for directory in project_root.ancestors() {
        let dot_git = directory.join(".git");
        let metadata = match fs::metadata(&dot_git) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            let common_dir = resolve_common_dir(&dot_git);
            return Some(GitRepository {
                git_dir: dot_git,
                common_dir,
            });
        }
        if metadata.is_file()
            && let Some(git_dir) = read_git_file(&dot_git)
        {
            let common_dir = resolve_common_dir(&git_dir);
            return Some(GitRepository {
                git_dir,
                common_dir,
            });
        }
    }
    None
}

/// Reads a `.git` file (linked worktree or submodule) and returns the Git directory it names.
fn read_git_file(dot_git: &Path) -> Option<PathBuf> {
    const MAX_GIT_FILE_BYTES: u64 = 4096;
    let metadata = fs::metadata(dot_git).ok()?;
    if metadata.len() > MAX_GIT_FILE_BYTES {
        return None;
    }
    let contents = fs::read_to_string(dot_git).ok()?;
    let target = contents.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = PathBuf::from(target);
    Some(if target.is_absolute() {
        target
    } else {
        dot_git.parent()?.join(target)
    })
}

/// The directory shared with the main work tree (which owns `info/exclude`).
fn resolve_common_dir(git_dir: &Path) -> PathBuf {
    let commondir = git_dir.join("commondir");
    let Ok(contents) = fs::read_to_string(&commondir) else {
        return git_dir.to_path_buf();
    };
    let target = contents.trim();
    if target.is_empty() {
        return git_dir.to_path_buf();
    }
    let target = PathBuf::from(target);
    let joined = if target.is_absolute() {
        target
    } else {
        git_dir.join(target)
    };
    normalize_lexically(&joined).unwrap_or(joined)
}

/// Resolves `.` and `..` lexically, without touching the filesystem.
fn normalize_lexically(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}
