//! The project above the working directory: the documented default of `--project`.
//!
//! TU-026 declares `--project` as selecting "the project above the working directory"
//! when it is absent, so this module implements that default. The walk starts at the
//! process working directory and climbs to the nearest ancestor that either
//!
//! - is registered with AgenTuna, or
//! - holds a `.git` entry (a directory, or the file a Git worktree or submodule uses).
//!
//! The walk is bounded by [`MAX_PROJECT_WALK`] levels. It only reads: discovery never
//! registers a project, never creates a project store and never writes the index, so a
//! read-only invocation stays read-only. When nothing matches, the caller behaves as if
//! `--project` had not been passed.

use std::path::{Path, PathBuf};

use at_store::{AppPaths, PathEnvironment};

/// Directory levels the walk examines above the working directory.
///
/// Deep enough for any realistic checkout, shallow enough that a pathological tree or a
/// filesystem hiccup cannot stall a command.
pub const MAX_PROJECT_WALK: usize = 32;

/// The project the working directory belongs to, when one can be discovered.
///
/// Returns `None` when the working directory cannot be read or no ancestor matches.
#[must_use]
pub fn project_above_cwd() -> Option<PathBuf> {
    let start = std::env::current_dir().ok()?;
    let start = canonical(&start);
    discover(&start, &registered_roots())
}

/// The nearest ancestor of `start` (inclusive) that is registered or is a Git root.
///
/// `registered` holds canonical project roots; it may be empty.
#[must_use]
pub fn discover(start: &Path, registered: &[PathBuf]) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..MAX_PROJECT_WALK {
        let directory = current?;
        if is_registered(directory, registered) || has_git_entry(directory) {
            return Some(directory.to_path_buf());
        }
        current = directory.parent();
    }
    None
}

/// Project roots recorded in the index, canonicalized, or an empty list.
///
/// A missing or unreadable index means nothing is registered yet: that is not an error
/// for discovery, only a reason to fall back to the Git-root rule. The index is opened
/// through [`at_db::ReadOnlyDatabase`], which never creates or migrates the file.
fn registered_roots() -> Vec<PathBuf> {
    let Ok(paths) = AppPaths::resolve(&PathEnvironment::from_process()) else {
        return Vec::new();
    };
    if !paths.db_path().exists() {
        return Vec::new();
    }
    let Ok(database) = at_db::ReadOnlyDatabase::open(paths.db_path()) else {
        return Vec::new();
    };
    let Ok(projects) = at_db::projects::list_projects(database.connection()) else {
        return Vec::new();
    };
    projects
        .iter()
        .map(|project| canonical(Path::new(&project.path)))
        .collect()
}

/// Whether `directory` is one of the registered project roots.
fn is_registered(directory: &Path, registered: &[PathBuf]) -> bool {
    if registered.is_empty() {
        return false;
    }
    let candidate = canonical(directory);
    registered.contains(&candidate)
}

/// Whether `directory` holds a `.git` entry.
///
/// A normal checkout records `.git` as a directory, while a Git worktree or submodule
/// records it as a regular file; `metadata` accepts both and follows a `.git` symlink to
/// its target, so no write-capable call is needed here.
fn has_git_entry(directory: &Path) -> bool {
    std::fs::metadata(directory.join(".git")).is_ok()
}

/// The physical form of `path`, or the path itself when it cannot be resolved.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
