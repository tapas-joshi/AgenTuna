//! Refuse-to-overwrite guard: never replace something AgenTuna does not own.
//!
//! Ownership is never inferred from a matching name. A target is managed only when it lives
//! under an AgenTuna store root, or the deployments registry records that exact path for the
//! same item, agent and scope. Everything else that already exists is refused wholesale,
//! with the remedies the UI can offer.
//!
//! The guard is unconditional: no setting or feature flag disables it. A caller that deploys
//! or copies into an agent folder passes an [`OverwriteGuard`] in its `WriteRequest`, and
//! `apply` checks it twice: before staging and again immediately before the rename.

use std::fmt::Debug;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use at_model::{AgentId, ItemId, Scope};
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

use crate::error::FsError;
use crate::hash::sha256_hex;
use crate::symlink::{open_nofollow, read_bounded};

/// What kind of thing already sits at a refused target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (never followed during classification).
    Symlink,
    /// An entry inside a shared config file; constructed by adapters, not by this module.
    Entry,
}

/// What the user can do about a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// Bring the existing item under AgenTuna management (owned by the adopt flow).
    Adopt,
    /// Rename the existing item out of the way, keeping every byte.
    MoveAside,
    /// Do nothing.
    Cancel,
}

/// The remedies offered with every refusal.
pub const REMEDIES: [Remedy; 3] = [Remedy::Adopt, Remedy::MoveAside, Remedy::Cancel];

/// Result of classifying a target path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Under a store root, or recorded as this item's deployment for this agent and scope.
    Managed,
    /// Something exists there that AgenTuna does not own.
    Unmanaged {
        /// What it is.
        kind: TargetKind,
        /// SHA-256 of a regular file's content; `None` for directories, symlinks and
        /// files too large to hash.
        existing_hash: Option<String>,
    },
    /// Nothing exists there.
    Absent,
}

/// Answers whether a deployments row links exactly this path. Implemented over `at-db` later.
pub trait DeploymentLookup: Debug + Send + Sync {
    /// True when `path` is recorded as the deployment of `item` for `agent` at `scope`.
    fn is_recorded(&self, path: &Path, item: &ItemId, agent: &AgentId, scope: Scope) -> bool;
}

/// The identity a write is made on behalf of.
#[derive(Debug, Clone)]
pub struct OwnershipContext {
    /// Item being deployed.
    pub item_id: ItemId,
    /// Agent being deployed to.
    pub agent_id: AgentId,
    /// Scope of the deployment.
    pub scope: Scope,
    /// AgenTuna store roots; anything beneath them is AgenTuna's own.
    pub store_roots: Vec<PathBuf>,
}

/// A check `apply` runs before staging and again right before the rename.
pub trait OverwriteGuard: Debug + Send + Sync {
    /// Fails with [`FsError::RefusedOverwrite`] when `path` holds something not owned.
    ///
    /// # Errors
    /// [`FsError::RefusedOverwrite`], or an IO error while inspecting the target.
    fn check(&self, path: &Path, allow_empty_dir: bool) -> Result<(), FsError>;
}

/// The standard guard: store roots plus the deployments registry.
#[derive(Debug, Clone)]
pub struct OwnershipGuard {
    ctx: OwnershipContext,
    deployments: Arc<dyn DeploymentLookup>,
}

impl OwnershipGuard {
    /// Builds a guard for one item/agent/scope.
    #[must_use]
    pub fn new(ctx: OwnershipContext, deployments: Arc<dyn DeploymentLookup>) -> Self {
        Self { ctx, deployments }
    }

    /// Wraps the guard for use in a `WriteRequest`.
    #[must_use]
    pub fn shared(self) -> Arc<dyn OverwriteGuard> {
        Arc::new(self)
    }
}

impl OverwriteGuard for OwnershipGuard {
    fn check(&self, path: &Path, allow_empty_dir: bool) -> Result<(), FsError> {
        let classification = classify_target(path, &self.ctx, self.deployments.as_ref())?;
        refuse_unless_replaceable(path, &classification, allow_empty_dir)
    }
}

/// Turns a classification into a refusal. Empty directories pass only with `allow_empty_dir`.
fn refuse_unless_replaceable(
    path: &Path,
    classification: &Classification,
    allow_empty_dir: bool,
) -> Result<(), FsError> {
    let Classification::Unmanaged {
        kind,
        existing_hash,
    } = classification
    else {
        return Ok(());
    };
    if *kind == TargetKind::Dir && allow_empty_dir && is_empty_dir(path)? {
        return Ok(());
    }
    Err(FsError::RefusedOverwrite {
        path: path.to_path_buf(),
        kind: *kind,
        existing_hash: existing_hash.clone(),
        remedies: REMEDIES.to_vec(),
    })
}

fn is_empty_dir(path: &Path) -> Result<bool, FsError> {
    Ok(fs::read_dir(path)
        .map_err(|e| FsError::from_io(path, e))?
        .next()
        .is_none())
}

/// Lexically cleans an absolute path: drops `.`, rejects `..` and relative paths.
pub(crate) fn clean_absolute(path: &Path) -> Result<PathBuf, FsError> {
    if !path.is_absolute() {
        return Err(FsError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path must be absolute",
        });
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(FsError::InvalidPath {
                    path: path.to_path_buf(),
                    reason: "path must not contain `..`",
                });
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// Resolves symlinks in the longest existing prefix and appends the rest unchanged.
/// This makes a store-root comparison immune to `/var` vs `/private/var` and to a symlinked
/// ancestor that points outside the store.
pub(crate) fn resolve_existing_prefix(path: &Path) -> Result<PathBuf, FsError> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        match fs::canonicalize(&probe) {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Ok(resolved);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(std::ffi::OsStr::to_owned) else {
                    return Ok(path.to_path_buf());
                };
                missing.push(name);
                if !probe.pop() {
                    return Ok(path.to_path_buf());
                }
            }
            Err(e) => return Err(FsError::from_io(&probe, e)),
        }
    }
}

/// Classifies `path` for a deployment or copy on behalf of `ctx`. Never follows a final symlink.
///
/// # Errors
/// [`FsError::InvalidPath`] for relative paths or `..`; IO errors while inspecting.
pub fn classify_target(
    path: &Path,
    ctx: &OwnershipContext,
    deployments: &dyn DeploymentLookup,
) -> Result<Classification, FsError> {
    let clean = clean_absolute(path)?;
    let meta = match fs::symlink_metadata(&clean) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Classification::Absent),
        Err(e) => return Err(FsError::from_io(&clean, e)),
    };

    let resolved = resolve_existing_prefix(&clean)?;
    for root in &ctx.store_roots {
        let root = resolve_existing_prefix(&clean_absolute(root)?)?;
        if resolved.starts_with(&root) {
            return Ok(Classification::Managed);
        }
    }
    if deployments.is_recorded(&clean, &ctx.item_id, &ctx.agent_id, ctx.scope) {
        return Ok(Classification::Managed);
    }

    let (kind, existing_hash) = if meta.file_type().is_symlink() {
        (TargetKind::Symlink, None)
    } else if meta.is_dir() {
        (TargetKind::Dir, None)
    } else {
        let hash = open_nofollow(&clean).ok().and_then(|mut file| {
            let m = file.metadata().ok()?;
            read_bounded(&clean, &mut file, &m)
                .ok()
                .map(|b| sha256_hex(&b))
        });
        (TargetKind::File, hash)
    };
    Ok(Classification::Unmanaged {
        kind,
        existing_hash,
    })
}

/// Creates missing parent directories, but only when the target itself is absent.
///
/// An existing target of any kind is left alone and reported as `Ok(false)` so the caller's
/// guard can refuse it. Every existing ancestor must be a directory.
///
/// # Errors
/// IO errors, or [`FsError::InvalidPath`] when an ancestor exists and is not a directory.
pub fn create_parents_if_target_absent(path: &Path) -> Result<bool, FsError> {
    let clean = clean_absolute(path)?;
    if fs::symlink_metadata(&clean).is_ok() {
        return Ok(false);
    }
    let Some(parent) = clean.parent() else {
        return Ok(true);
    };
    match fs::metadata(parent) {
        Ok(m) if m.is_dir() => return Ok(true),
        Ok(_) => {
            return Err(FsError::InvalidPath {
                path: parent.to_path_buf(),
                reason: "ancestor is not a directory",
            });
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(FsError::from_io(parent, e)),
    }
    fs::create_dir_all(parent).map_err(|e| FsError::from_io(parent, e))?;
    Ok(true)
}

/// Creates `path` and any parent directories if they do not exist, returning an IO-typed [`FsError`] on failure.
///
/// # Errors
/// Returns [`FsError`] if an ancestor is not a directory or if creating the directory fails.
pub fn ensure_dir(path: &Path) -> Result<(), FsError> {
    ensure_dir_with_mode(path, None)
}

/// Creates `path` and any parent directories if they do not exist, optionally setting the directory's
/// permission mode on Unix platforms.
///
/// # Errors
/// Returns [`FsError`] if creating directories or setting permissions fails.
pub fn ensure_dir_with_mode(path: &Path, mode: Option<u32>) -> Result<(), FsError> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            if let Some(m_bits) = mode {
                set_directory_mode(path, m_bits)?;
            }
            Ok(())
        }
        Ok(_) => Err(FsError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path exists and is not a directory",
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|err| FsError::from_io(path, err))?;
            if let Some(m_bits) = mode {
                set_directory_mode(path, m_bits)?;
            }
            Ok(())
        }
        Err(e) => Err(FsError::from_io(path, e)),
    }
}

/// Sets permission mode on `path`, mapping OS errors into [`FsError`].
///
/// # Errors
/// Returns [`FsError`] if setting permissions fails.
pub fn set_permissions(path: &Path, mode: u32) -> Result<(), FsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
            .map_err(|e| FsError::from_io(path, e))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Sets permission mode on a directory when supported by the platform (e.g. Unix 0o700 for private store dirs).
///
/// # Errors
/// Returns [`FsError`] if inspecting metadata or setting permissions fails.
pub fn set_directory_mode(path: &Path, mode: u32) -> Result<(), FsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).map_err(|e| FsError::from_io(path, e))?;
        if metadata.permissions().mode() & 0o777 != mode & 0o777 {
            set_permissions(path, mode)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// Where a refused target went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveAsideOutcome {
    /// The path that was moved.
    pub original: PathBuf,
    /// Its new name, in the same parent directory.
    pub moved_to: PathBuf,
}

const BACKUP_STAMP: &[FormatItem<'static>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");

/// Renames `path` to `<name>.agentuna-backup-<UTC timestamp>` inside the same parent.
///
/// A rename within one directory is atomic and same-filesystem, so no byte is copied or
/// deleted. The caller records the operation (TF-009) and can undo by renaming back.
/// A name collision gets a numeric suffix; an existing sibling is never overwritten.
///
/// # Errors
/// [`FsError::NotFound`] when `path` does not exist; IO errors otherwise.
pub fn move_aside(path: &Path, now: OffsetDateTime) -> Result<MoveAsideOutcome, FsError> {
    let clean = clean_absolute(path)?;
    fs::symlink_metadata(&clean).map_err(|e| FsError::from_io(&clean, e))?;
    let name = clean
        .file_name()
        .ok_or_else(|| FsError::NoParent {
            path: clean.clone(),
        })?
        .to_string_lossy()
        .into_owned();
    let parent = clean.parent().ok_or_else(|| FsError::NoParent {
        path: clean.clone(),
    })?;
    let stamp = now
        .to_offset(time::UtcOffset::UTC)
        .format(BACKUP_STAMP)
        .map_err(|_| FsError::InvalidPath {
            path: clean.clone(),
            reason: "timestamp not formattable",
        })?;

    for attempt in 0u32..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!(".{attempt}")
        };
        let target = parent.join(format!("{name}.agentuna-backup-{stamp}{suffix}"));
        if fs::symlink_metadata(&target).is_ok() {
            continue;
        }
        fs::rename(&clean, &target).map_err(|e| FsError::from_io(&clean, e))?;
        return Ok(MoveAsideOutcome {
            original: clean,
            moved_to: target,
        });
    }
    Err(FsError::AlreadyExists {
        path: parent.join(format!("{name}.agentuna-backup-{stamp}")),
    })
}
