//! Deployment primitive: materialise a canonical store item in an agent folder.
//!
//! Two modes exist. [`LinkMode::Link`] writes a symbolic link to the item, so an edit in the
//! store is visible through the agent path without another write. [`LinkMode::Copy`] writes a
//! recursive copy plus [`.agentuna-copy-manifest.json`](COPY_MANIFEST_FILE), which records each
//! file's SHA-256 and the source version that produced it; re-deploying rewrites only files
//! whose hash changed.
//!
//! Both modes stage through a sibling path named `.<name>.agentuna-tmp-<pid>` and rename it
//! over the target, so replacing a link never exposes a missing path to a concurrent reader.
//! Only a target AgenTuna owns — a recorded deployment, a link to the same source, a matching
//! copy manifest, or an empty directory — is replaced; anything else is refused with
//! [`FsError::RefusedOverwrite`] unless the caller supplies an [`OverwriteGuard`] that
//! classifies it as managed. Removal ([`remove_deployment`]) works only on recorded
//! deployments and never touches canonical store content.
//!
//! The Windows junction variant lives in TF-038; the API here stays platform-neutral.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use at_model::{ItemId, LinkMode};
use serde::{Deserialize, Serialize};

use crate::atomic::{self, WriteRequest};
use crate::concurrency::ExpectedState;
use crate::error::FsError;
use crate::guard::{
    self, DeploymentLookup, OverwriteGuard, OwnershipContext, REMEDIES, TargetKind,
};
use crate::hash::sha256_hex;

/// File name of the manifest inside every copy deployment.
pub const COPY_MANIFEST_FILE: &str = ".agentuna-copy-manifest.json";

/// Manifest format version written as `schema_version`.
pub const COPY_MANIFEST_VERSION: u32 = 1;

/// One file recorded in a [`CopyManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyManifestEntry {
    /// Path relative to the deployment root, `/`-separated so it is portable.
    pub path: String,
    /// Lowercase hex SHA-256 of the file content.
    pub sha256: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// What a copy deployment contains and which source version produced it.
///
/// Written inside the deployment directory as [`COPY_MANIFEST_FILE`]. It is the baseline a
/// re-deploy compares against, so unchanged files are not rewritten and keep their mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyManifest {
    /// Manifest format version, [`COPY_MANIFEST_VERSION`] today.
    pub schema_version: u32,
    /// Item the copy was made from, when the caller supplied an identity.
    #[serde(default)]
    pub item_id: Option<ItemId>,
    /// Mode this manifest describes; always [`LinkMode::Copy`] today.
    pub mode: LinkMode,
    /// Canonical source directory that was copied.
    pub source_path: PathBuf,
    /// SHA-256 over the sorted `path`, `hash` pairs: one value for a whole tree version.
    pub source_version: String,
    /// Directories in the copy, relative and `/`-separated, excluding the root.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Files in the copy, sorted by path.
    pub files: Vec<CopyManifestEntry>,
}

impl CopyManifest {
    /// Reads the manifest inside `dir`. `None` when the file is absent.
    ///
    /// # Errors
    /// [`FsError::InvalidManifest`] when the file is not valid JSON or not this schema.
    pub fn read(dir: &Path) -> Result<Option<Self>, FsError> {
        let path = dir.join(COPY_MANIFEST_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                let manifest: Self =
                    serde_json::from_slice(&bytes).map_err(|e| FsError::InvalidManifest {
                        path: path.clone(),
                        reason: format!("not a valid copy manifest: {e}"),
                    })?;
                if manifest.schema_version != COPY_MANIFEST_VERSION {
                    return Err(FsError::InvalidManifest {
                        path,
                        reason: format!(
                            "unsupported manifest schema_version {} (expected {COPY_MANIFEST_VERSION})",
                            manifest.schema_version
                        ),
                    });
                }
                Ok(Some(manifest))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(FsError::from_io(&path, e)),
        }
    }

    /// Writes the manifest into `dir`, atomically replacing an older one.
    ///
    /// # Errors
    /// IO errors, [`FsError::ConcurrentModification`] when another writer changed the old
    /// manifest in between.
    pub fn write_to(&self, dir: &Path) -> Result<PathBuf, FsError> {
        let path = dir.join(COPY_MANIFEST_FILE);
        let bytes = self.to_json()?.into_bytes();
        let request = match fs::read(&path) {
            Ok(previous) => {
                WriteRequest::replace(&path, bytes, ExpectedState::Hash(sha256_hex(&previous)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => WriteRequest::create(&path, bytes),
            Err(e) => return Err(FsError::from_io(&path, e)),
        };
        atomic::apply(&request)?;
        Ok(path)
    }

    /// The entry recorded for a relative path, if any.
    #[must_use]
    pub fn entry(&self, relative_path: &str) -> Option<&CopyManifestEntry> {
        self.files.iter().find(|f| f.path == relative_path)
    }

    /// Serialised form written to disk, pretty-printed so diffs stay readable.
    ///
    /// # Errors
    /// [`FsError::InvalidManifest`] if serialisation fails (it does not for this type).
    pub fn to_json(&self) -> Result<String, FsError> {
        serde_json::to_string_pretty(self).map_err(|e| FsError::InvalidManifest {
            path: PathBuf::from(COPY_MANIFEST_FILE),
            reason: format!("cannot serialise: {e}"),
        })
    }
}

/// Filesystem primitives a deployment needs.
///
/// Injected so tests can simulate a platform that refuses symbolic links or a rename across
/// volumes; production callers use [`SystemFs`].
#[doc(hidden)]
pub trait DeployFs: fmt::Debug {
    /// Creates a symbolic link at `link_path` whose contents are `link_target`.
    fn symlink(&self, link_target: &Path, link_path: &Path) -> io::Result<()>;
    /// Renames `from` to `to`, replacing a non-directory `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Whether both paths live on the same volume, so a relative link is usable.
    fn same_volume(&self, a: &Path, b: &Path) -> bool;
}

/// The real filesystem.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct SystemFs;

impl DeployFs for SystemFs {
    fn symlink(&self, link_target: &Path, link_path: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(link_target, link_path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn same_volume(&self, a: &Path, b: &Path) -> bool {
        match (fs::metadata(a), fs::metadata(b)) {
            // A failed lookup can only make the answer more conservative: an absolute link
            // works everywhere, a relative one does not.
            (Ok(a), Ok(b)) => a.dev() == b.dev(),
            _ => false,
        }
    }
}

/// Everything one deployment needs.
#[derive(Debug, Clone)]
pub struct DeployRequest {
    /// Canonical store item directory that is deployed.
    pub source: PathBuf,
    /// Path inside the agent folder that receives the link or copy.
    pub target: PathBuf,
    /// Link or copy.
    pub mode: LinkMode,
    /// Item identity recorded in a copy manifest.
    pub item_id: Option<ItemId>,
    /// With [`LinkMode::Link`], fall back to a copy when the platform refuses symlinks.
    /// Fallback never happens silently for a caller that did not ask for it.
    pub fallback_to_copy: bool,
    /// Refuse-to-overwrite guard, checked before staging and again immediately before the
    /// install rename.
    pub guard: Option<Arc<dyn OverwriteGuard>>,
}

impl DeployRequest {
    /// A deployment of `source` to `target` with no guard and no copy fallback.
    #[must_use]
    pub fn new(source: impl Into<PathBuf>, target: impl Into<PathBuf>, mode: LinkMode) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            mode,
            item_id: None,
            fallback_to_copy: false,
            guard: None,
        }
    }

    /// Records the item identity in the copy manifest.
    #[must_use]
    pub fn with_item(mut self, item_id: ItemId) -> Self {
        self.item_id = Some(item_id);
        self
    }

    /// Replaces the ownership guard.
    #[must_use]
    pub fn with_guard(mut self, guard: Arc<dyn OverwriteGuard>) -> Self {
        self.guard = Some(guard);
        self
    }

    /// Allows a [`LinkMode::Link`] deployment to become a copy when symlinks are refused.
    #[must_use]
    pub fn with_copy_fallback(mut self) -> Self {
        self.fallback_to_copy = true;
        self
    }
}

/// Where a link points and whether the stored value is relative.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkOutcome {
    /// The exact value written into the symlink.
    pub link_target: PathBuf,
    /// Whether `link_target` is relative to the link's directory.
    pub is_relative: bool,
}

/// What a copy deployment wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CopyOutcome {
    /// SHA-256 over the source tree this deployment now matches.
    pub source_version: String,
    /// Manifest that records the copy.
    pub manifest_path: PathBuf,
    /// Files created or rewritten.
    pub files_written: usize,
    /// Files left untouched because their hash was already correct.
    pub files_unchanged: usize,
    /// Files deleted because the source no longer has them.
    pub files_removed: usize,
}

/// What a successful deployment did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeployOutcome {
    /// Mode actually used; [`LinkMode::Copy`] after a fallback.
    pub mode: LinkMode,
    /// The deployment path.
    pub target: PathBuf,
    /// Present when a link was written.
    pub link: Option<LinkOutcome>,
    /// Present when a copy was written.
    pub copy: Option<CopyOutcome>,
}

/// Which deployment a removal deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemovedDeployment {
    /// A symbolic link.
    Link,
    /// A copy directory; `files` is the manifest file count.
    Copy {
        /// Files recorded in the removed manifest.
        files: usize,
    },
}

/// What [`remove_deployment`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoveOutcome {
    /// The path that was deleted.
    pub target: PathBuf,
    /// The deployment shape that was deleted.
    pub removed: RemovedDeployment,
}

/// What removal is allowed to delete.
#[derive(Debug, Clone)]
pub struct RemoveRequest {
    /// Deployment path to delete.
    pub target: PathBuf,
    /// Item, agent, scope and store roots that decide ownership.
    pub ctx: OwnershipContext,
    /// Deployments registry; only a recorded path may be removed.
    pub deployments: Arc<dyn DeploymentLookup>,
}

impl RemoveRequest {
    /// A removal for `target` on behalf of `ctx`.
    #[must_use]
    pub fn new(
        target: impl Into<PathBuf>,
        ctx: OwnershipContext,
        deployments: Arc<dyn DeploymentLookup>,
    ) -> Self {
        Self {
            target: target.into(),
            ctx,
            deployments,
        }
    }
}

/// Deploys `source` to `target` with the mode in `request`.
///
/// # Errors
/// [`FsError::LinkUnsupported`] when a link was requested and the platform refuses one,
/// [`FsError::RefusedOverwrite`] when the target is not owned and no guard allows it,
/// [`FsError::CrossDeviceRename`] when the install rename crosses filesystems, and the other
/// [`FsError`] variants for IO failures.
pub fn deploy(request: &DeployRequest) -> Result<DeployOutcome, FsError> {
    deploy_with(request, &SystemFs)
}

/// [`deploy`] against an injected filesystem. For tests only.
#[doc(hidden)]
pub fn deploy_with(
    request: &DeployRequest,
    fs_impl: &dyn DeployFs,
) -> Result<DeployOutcome, FsError> {
    let source = guard::clean_absolute(&request.source)?;
    let target = guard::clean_absolute(&request.target)?;
    if source == target {
        return Err(FsError::InvalidPath {
            path: source,
            reason: "deployment source and target are the same path",
        });
    }
    let source_meta = fs::metadata(&source).map_err(|e| FsError::from_io(&source, e))?;
    if !source_meta.is_dir() {
        return Err(FsError::InvalidPath {
            path: source,
            reason: "deployment source is not a directory",
        });
    }
    let source_real = fs::canonicalize(&source).map_err(|e| FsError::from_io(&source, e))?;

    if let Some(guard) = &request.guard {
        guard.check(&target, true)?;
    }
    let snapshot = inspect(&target)?;
    if request.guard.is_none() {
        ensure_replaceable(&target, &snapshot, &source_real)?;
    }

    let parent = target
        .parent()
        .ok_or_else(|| FsError::NoParent {
            path: target.clone(),
        })?
        .to_path_buf();
    fs::create_dir_all(&parent).map_err(|e| FsError::from_io(&parent, e))?;
    let parent_real = fs::canonicalize(&parent).map_err(|e| FsError::from_io(&parent, e))?;

    match request.mode {
        LinkMode::Link => match deploy_link(
            &source_real,
            &target,
            &parent_real,
            request.guard.as_ref(),
            fs_impl,
        ) {
            Ok(outcome) => Ok(outcome),
            Err(FsError::LinkUnsupported { .. }) if request.fallback_to_copy => deploy_copy(
                request,
                &source_real,
                &target,
                &snapshot,
                request.guard.as_ref(),
                fs_impl,
            ),
            Err(e) => Err(e),
        },
        LinkMode::Copy => deploy_copy(
            request,
            &source_real,
            &target,
            &snapshot,
            request.guard.as_ref(),
            fs_impl,
        ),
    }
}

/// Deployment entry point: links when `mode` is [`LinkMode::Link`], copies otherwise.
///
/// # Errors
/// See [`deploy`].
pub fn link_dir(source: &Path, target: &Path, mode: LinkMode) -> Result<DeployOutcome, FsError> {
    deploy(&DeployRequest::new(source, target, mode))
}

/// Deploys `source` as a recursive copy with a [`CopyManifest`].
///
/// # Errors
/// See [`deploy`].
pub fn copy_dir(source: &Path, target: &Path) -> Result<DeployOutcome, FsError> {
    link_dir(source, target, LinkMode::Copy)
}

/// Removes a deployment, leaving the canonical store untouched.
///
/// Only a path the deployments registry records for this item, agent and scope is removed,
/// and only when it is a link into a store root or a copy carrying a matching manifest.
///
/// # Errors
/// [`FsError::DeploymentNotOwned`] when the path is not a recorded deployment, lies inside a
/// store root, or is not one of the two expected shapes; [`FsError::NotFound`] when nothing
/// is there; IO errors otherwise.
pub fn remove_deployment(request: &RemoveRequest) -> Result<RemoveOutcome, FsError> {
    let target = guard::clean_absolute(&request.target)?;
    let resolved_target = resolve_for_containment(&target)?;
    let roots = resolved_roots(&request.ctx.store_roots)?;
    if roots.iter().any(|root| resolved_target.starts_with(root)) {
        return Err(FsError::DeploymentNotOwned {
            path: target,
            reason: "the path is inside an AgenTuna store, so it is canonical content",
        });
    }
    if !request.deployments.is_recorded(
        &target,
        &request.ctx.item_id,
        &request.ctx.agent_id,
        request.ctx.scope,
    ) {
        return Err(FsError::DeploymentNotOwned {
            path: target,
            reason: "no deployment is recorded for this item, agent and scope",
        });
    }

    let meta = fs::symlink_metadata(&target).map_err(|e| FsError::from_io(&target, e))?;
    if meta.file_type().is_symlink() {
        let link_target = fs::read_link(&target).map_err(|e| FsError::from_io(&target, e))?;
        let joined = if link_target.is_absolute() {
            link_target
        } else {
            target
                .parent()
                .ok_or_else(|| FsError::NoParent {
                    path: target.clone(),
                })?
                .join(link_target)
        };
        let resolved_link = guard::resolve_existing_prefix(&joined)?;
        if !roots.iter().any(|root| resolved_link.starts_with(root)) {
            return Err(FsError::DeploymentNotOwned {
                path: target,
                reason: "the symlink does not point into an AgenTuna store",
            });
        }
        fs::remove_file(&target).map_err(|e| FsError::from_io(&target, e))?;
        return Ok(RemoveOutcome {
            target,
            removed: RemovedDeployment::Link,
        });
    }
    if meta.is_dir() {
        let Some(manifest) = CopyManifest::read(&target)? else {
            return Err(FsError::DeploymentNotOwned {
                path: target,
                reason: "the directory has no copy manifest",
            });
        };
        if manifest.item_id.is_some_and(|id| id != request.ctx.item_id) {
            return Err(FsError::DeploymentNotOwned {
                path: target,
                reason: "the copy manifest belongs to a different item",
            });
        }
        let files = manifest.files.len();
        fs::remove_dir_all(&target).map_err(|e| FsError::from_io(&target, e))?;
        return Ok(RemoveOutcome {
            target,
            removed: RemovedDeployment::Copy { files },
        });
    }
    Err(FsError::DeploymentNotOwned {
        path: target,
        reason: "a deployment is a directory link or copy, not a file",
    })
}

/// What exists at the deployment path right now.
enum Snapshot {
    Absent,
    Symlink { resolved: Option<PathBuf> },
    Dir { empty: bool },
    File,
}

fn inspect(target: &Path) -> Result<Snapshot, FsError> {
    match fs::symlink_metadata(target) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Snapshot::Absent),
        Err(e) => Err(FsError::from_io(target, e)),
        Ok(meta) if meta.file_type().is_symlink() => Ok(Snapshot::Symlink {
            resolved: fs::canonicalize(target).ok(),
        }),
        Ok(meta) if meta.is_dir() => Ok(Snapshot::Dir {
            empty: fs::read_dir(target)
                .map_err(|e| FsError::from_io(target, e))?
                .next()
                .is_none(),
        }),
        Ok(_) => Ok(Snapshot::File),
    }
}

/// The no-guard ownership rule: only self-identifying deployments and empty directories.
fn ensure_replaceable(
    target: &Path,
    snapshot: &Snapshot,
    source_real: &Path,
) -> Result<(), FsError> {
    match snapshot {
        Snapshot::Absent => Ok(()),
        Snapshot::Dir { empty: true } => Ok(()),
        Snapshot::Dir { empty: false } => match CopyManifest::read(target)? {
            Some(manifest) if manifest.source_path == source_real => Ok(()),
            _ => Err(refused(target, TargetKind::Dir)),
        },
        Snapshot::Symlink {
            resolved: Some(resolved),
        } if resolved == source_real => Ok(()),
        Snapshot::Symlink { .. } => Err(refused(target, TargetKind::Symlink)),
        Snapshot::File => Err(refused(target, TargetKind::File)),
    }
}

fn refused(target: &Path, kind: TargetKind) -> FsError {
    let existing_hash = if kind == TargetKind::File {
        fs::read(target)
            .ok()
            .filter(|bytes| bytes.len() as u64 <= crate::MAX_FILE_BYTES)
            .map(|bytes| sha256_hex(&bytes))
    } else {
        None
    };
    FsError::RefusedOverwrite {
        path: target.to_path_buf(),
        kind,
        existing_hash,
        remedies: REMEDIES.to_vec(),
    }
}

fn deploy_link(
    source_real: &Path,
    target: &Path,
    parent_real: &Path,
    guard: Option<&Arc<dyn OverwriteGuard>>,
    fs_impl: &dyn DeployFs,
) -> Result<DeployOutcome, FsError> {
    let is_relative = fs_impl.same_volume(source_real, parent_real);
    let link_target = if is_relative {
        relative_path(parent_real, source_real).ok_or_else(|| FsError::InvalidPath {
            path: source_real.to_path_buf(),
            reason: "cannot express the store item relative to the agent folder",
        })?
    } else {
        source_real.to_path_buf()
    };
    let stage = Staging::link(target, &link_target, fs_impl)?;
    install(stage.path(), target, false, guard, fs_impl)?;
    stage.disarm();
    Ok(DeployOutcome {
        mode: LinkMode::Link,
        target: target.to_path_buf(),
        link: Some(LinkOutcome {
            link_target,
            is_relative,
        }),
        copy: None,
    })
}

fn deploy_copy(
    request: &DeployRequest,
    source_real: &Path,
    target: &Path,
    snapshot: &Snapshot,
    guard: Option<&Arc<dyn OverwriteGuard>>,
    fs_impl: &dyn DeployFs,
) -> Result<DeployOutcome, FsError> {
    let plan = scan_tree(source_real)?;
    let manifest = manifest_for(&plan, source_real, request.item_id);

    let incremental = match snapshot {
        Snapshot::Dir { empty: false } => match CopyManifest::read(target)? {
            Some(previous) if request.item_id.is_none() || previous.item_id == request.item_id => {
                Some(previous)
            }
            _ => None,
        },
        _ => None,
    };

    let stats = match incremental {
        Some(previous) => {
            let mut stats = CopyStats::default();
            update_copy(target, &plan, &previous, &mut stats)?;
            manifest.write_to(target)?;
            stats
        }
        None => {
            let stage = Staging::dir(target)?;
            materialise(&plan, stage.path())?;
            manifest.write_to(stage.path())?;
            install(stage.path(), target, true, guard, fs_impl)?;
            stage.disarm();
            CopyStats {
                written: plan.files.len(),
                unchanged: 0,
                removed: 0,
            }
        }
    };
    let outcome = CopyOutcome {
        source_version: manifest.source_version.clone(),
        manifest_path: target.join(COPY_MANIFEST_FILE),
        files_written: stats.written,
        files_unchanged: stats.unchanged,
        files_removed: stats.removed,
    };

    Ok(DeployOutcome {
        mode: LinkMode::Copy,
        target: target.to_path_buf(),
        link: None,
        copy: Some(outcome),
    })
}

/// Creates the staging sibling, installs it, and cleans up on any failure.
fn install(
    stage: &Path,
    target: &Path,
    stage_is_dir: bool,
    guard: Option<&Arc<dyn OverwriteGuard>>,
    fs_impl: &dyn DeployFs,
) -> Result<(), FsError> {
    if let Some(guard) = guard {
        guard.check(target, true)?;
    }
    match inspect(target)? {
        Snapshot::Absent => rename(stage, target, fs_impl),
        Snapshot::Symlink { .. } => {
            if stage_is_dir {
                // A directory cannot be renamed over a symlink; drop the link first.
                fs::remove_file(target).map_err(|e| FsError::from_io(target, e))?;
            }
            rename(stage, target, fs_impl)
        }
        Snapshot::Dir { .. } => {
            if !stage_is_dir {
                return swap_aside(stage, target, fs_impl);
            }
            match rename(stage, target, fs_impl) {
                Ok(()) => Ok(()),
                Err(e) if is_dir_not_empty(&e) => swap_aside(stage, target, fs_impl),
                Err(e) => Err(e),
            }
        }
        Snapshot::File => {
            if stage_is_dir {
                fs::remove_file(target).map_err(|e| FsError::from_io(target, e))?;
            }
            rename(stage, target, fs_impl)
        }
    }
}

fn is_dir_not_empty(error: &FsError) -> bool {
    matches!(
        error,
        FsError::Io { source, .. }
            if matches!(source.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST))
    )
}

/// Two-step replacement for a directory target: move it aside, install, delete the old tree.
fn swap_aside(stage: &Path, target: &Path, fs_impl: &dyn DeployFs) -> Result<(), FsError> {
    let aside = sibling_path(target, ".old")?;
    rename(target, &aside, fs_impl)?;
    match rename(stage, target, fs_impl) {
        Ok(()) => {
            remove_path(&aside)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::rename(&aside, target);
            Err(e)
        }
    }
}

fn rename(from: &Path, to: &Path, fs_impl: &dyn DeployFs) -> Result<(), FsError> {
    fs_impl.rename(from, to).map_err(|e| {
        if e.raw_os_error() == Some(libc::EXDEV) {
            FsError::CrossDeviceRename {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
            }
        } else {
            FsError::from_io(to, e)
        }
    })
}

fn is_link_unsupported(error: &io::Error) -> bool {
    // macOS gives ENOTSUP and EOPNOTSUPP distinct values; on Linux they are equal, so compare
    // rather than repeat the constant inside one pattern.
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    code == libc::EPERM || code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS
}

/// A staged link or directory tree, deleted unless it was installed.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn link(target: &Path, link_target: &Path, fs_impl: &dyn DeployFs) -> Result<Self, FsError> {
        let path = sibling_path(target, "")?;
        fs_impl.symlink(link_target, &path).map_err(|e| {
            if is_link_unsupported(&e) {
                FsError::LinkUnsupported {
                    path: target.to_path_buf(),
                    source: e,
                }
            } else {
                FsError::from_io(&path, e)
            }
        })?;
        Ok(Self { path })
    }

    fn dir(target: &Path) -> Result<Self, FsError> {
        let path = sibling_path(target, "")?;
        fs::create_dir(&path).map_err(|e| FsError::from_io(&path, e))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(mut self) {
        self.path = PathBuf::new();
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = remove_path(&self.path);
        }
    }
}

/// `.<name>.agentuna-tmp-<pid><suffix>` next to `target`, with a counter on collision.
fn sibling_path(target: &Path, suffix: &str) -> Result<PathBuf, FsError> {
    let parent = target.parent().ok_or_else(|| FsError::NoParent {
        path: target.to_path_buf(),
    })?;
    let name = target
        .file_name()
        .ok_or_else(|| FsError::NoParent {
            path: target.to_path_buf(),
        })?
        .to_string_lossy()
        .into_owned();
    let pid = std::process::id();
    for n in 0u32..1000 {
        let counter = if n == 0 {
            String::new()
        } else {
            format!("-{n}")
        };
        let candidate = parent.join(format!(".{name}.agentuna-tmp-{pid}{counter}{suffix}"));
        if fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(FsError::AlreadyExists {
        path: parent.join(format!(".{name}.agentuna-tmp-{pid}{suffix}")),
    })
}

fn remove_path(path: &Path) -> Result<(), FsError> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(FsError::from_io(path, e)),
        Ok(meta) if meta.is_dir() => {
            fs::remove_dir_all(path).map_err(|e| FsError::from_io(path, e))
        }
        Ok(_) => fs::remove_file(path).map_err(|e| FsError::from_io(path, e)),
    }
}

/// `/`-separated relative path from `from` (a directory) to `to`; both absolute and normal.
fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from: Vec<Component<'_>> = from.components().collect();
    let to_parts: Vec<Component<'_>> = to.components().collect();
    if from.first() != to_parts.first() {
        return None;
    }
    let common = from
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for component in &to_parts[common..] {
        out.push(component.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

/// Resolves every component of `path` except the last; the last is kept as written, so a
/// deployment symlink itself is never mistaken for the store content it points at.
fn resolve_for_containment(path: &Path) -> Result<PathBuf, FsError> {
    let parent = path.parent().ok_or_else(|| FsError::NoParent {
        path: path.to_path_buf(),
    })?;
    let mut out = guard::resolve_existing_prefix(parent)?;
    if let Some(name) = path.file_name() {
        out.push(name);
    }
    Ok(out)
}

fn resolved_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, FsError> {
    roots
        .iter()
        .map(|root| guard::resolve_existing_prefix(&guard::clean_absolute(root)?))
        .collect()
}

/// One file found in the source tree.
struct PlannedFile {
    relative: String,
    source: PathBuf,
    sha256: String,
    bytes: u64,
}

/// The full source tree, sorted so a manifest is deterministic.
struct TreePlan {
    dirs: Vec<String>,
    files: Vec<PlannedFile>,
}

fn scan_tree(root: &Path) -> Result<TreePlan, FsError> {
    let mut plan = TreePlan {
        dirs: Vec::new(),
        files: Vec::new(),
    };
    walk(root, root, "", &mut plan)?;
    plan.dirs.sort();
    plan.files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(plan)
}

fn walk(root: &Path, dir: &Path, prefix: &str, plan: &mut TreePlan) -> Result<(), FsError> {
    let entries = fs::read_dir(dir).map_err(|e| FsError::from_io(dir, e))?;
    let mut children = Vec::new();
    for entry in entries {
        children.push(entry.map_err(|e| FsError::from_io(dir, e))?);
    }
    children.sort_by_key(fs::DirEntry::file_name);
    for entry in children {
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| FsError::InvalidPath {
                path: path.clone(),
                reason: "file name is not valid UTF-8",
            })?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(&path).map_err(|e| FsError::from_io(&path, e))?;
        if meta.file_type().is_symlink() {
            let resolved = fs::canonicalize(&path).map_err(|e| FsError::from_io(&path, e))?;
            if !resolved.starts_with(root) {
                return Err(FsError::InvalidPath {
                    path,
                    reason: "source symlink leaves the deployed directory",
                });
            }
            let resolved_meta =
                fs::metadata(&resolved).map_err(|e| FsError::from_io(&resolved, e))?;
            if !resolved_meta.is_file() {
                return Err(FsError::InvalidPath {
                    path,
                    reason: "a copy deployment cannot reproduce a symlinked directory",
                });
            }
            plan.files
                .push(planned_file(&relative, &resolved, &resolved_meta)?);
        } else if meta.is_dir() {
            plan.dirs.push(relative.clone());
            walk(root, &path, &relative, plan)?;
        } else if meta.is_file() {
            plan.files.push(planned_file(&relative, &path, &meta)?);
        } else {
            return Err(FsError::NotRegularFile { path });
        }
    }
    Ok(())
}

fn planned_file(
    relative: &str,
    source: &Path,
    meta: &fs::Metadata,
) -> Result<PlannedFile, FsError> {
    if meta.len() > crate::MAX_FILE_BYTES {
        return Err(FsError::TooLarge {
            path: source.to_path_buf(),
            size: meta.len(),
            limit: crate::MAX_FILE_BYTES,
        });
    }
    let bytes = fs::read(source).map_err(|e| FsError::from_io(source, e))?;
    Ok(PlannedFile {
        relative: relative.to_owned(),
        source: source.to_path_buf(),
        sha256: sha256_hex(&bytes),
        bytes: bytes.len() as u64,
    })
}

fn source_version(plan: &TreePlan) -> String {
    let mut buffer = String::new();
    for file in &plan.files {
        buffer.push_str(&file.relative);
        buffer.push('\0');
        buffer.push_str(&file.sha256);
        buffer.push('\0');
        buffer.push_str(&file.bytes.to_string());
        buffer.push('\n');
    }
    sha256_hex(buffer.as_bytes())
}

fn manifest_for(plan: &TreePlan, source_real: &Path, item_id: Option<ItemId>) -> CopyManifest {
    CopyManifest {
        schema_version: COPY_MANIFEST_VERSION,
        item_id,
        mode: LinkMode::Copy,
        source_path: source_real.to_path_buf(),
        source_version: source_version(plan),
        dirs: plan.dirs.clone(),
        files: plan
            .files
            .iter()
            .map(|file| CopyManifestEntry {
                path: file.relative.clone(),
                sha256: file.sha256.clone(),
                bytes: file.bytes,
            })
            .collect(),
    }
}

/// Writes the whole plan into an empty staging directory.
fn materialise(plan: &TreePlan, stage: &Path) -> Result<(), FsError> {
    for dir in &plan.dirs {
        let path = stage.join(dir);
        fs::create_dir_all(&path).map_err(|e| FsError::from_io(&path, e))?;
    }
    for file in &plan.files {
        let path = stage.join(&file.relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| FsError::from_io(parent, e))?;
        }
        let bytes = fs::read(&file.source).map_err(|e| FsError::from_io(&file.source, e))?;
        fs::write(&path, &bytes).map_err(|e| FsError::from_io(&path, e))?;
        copy_permissions(&file.source, &path)?;
    }
    Ok(())
}

#[derive(Default)]
struct CopyStats {
    written: usize,
    unchanged: usize,
    removed: usize,
}

/// Rewrites only files whose recorded hash or on-disk content differs from the plan.
fn update_copy(
    target: &Path,
    plan: &TreePlan,
    previous: &CopyManifest,
    stats: &mut CopyStats,
) -> Result<(), FsError> {
    for dir in &plan.dirs {
        let path = target.join(dir);
        if fs::symlink_metadata(&path).is_err() {
            fs::create_dir_all(&path).map_err(|e| FsError::from_io(&path, e))?;
        }
    }
    let recorded: BTreeMap<&str, &CopyManifestEntry> = previous
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();

    for file in &plan.files {
        let path = target.join(&file.relative);
        let on_disk = fs::read(&path).ok().map(|bytes| sha256_hex(&bytes));
        let recorded_same = recorded
            .get(file.relative.as_str())
            .is_some_and(|entry| entry.sha256 == file.sha256);
        if recorded_same && on_disk.as_deref() == Some(file.sha256.as_str()) {
            stats.unchanged += 1;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| FsError::from_io(parent, e))?;
        }
        let bytes = fs::read(&file.source).map_err(|e| FsError::from_io(&file.source, e))?;
        let request = match on_disk {
            Some(hash) => WriteRequest::replace(&path, bytes, ExpectedState::Hash(hash)),
            None => WriteRequest::create(&path, bytes),
        };
        atomic::apply(&request)?;
        copy_permissions(&file.source, &path)?;
        stats.written += 1;
    }

    let planned: BTreeMap<&str, ()> = plan
        .files
        .iter()
        .map(|file| (file.relative.as_str(), ()))
        .collect();
    for entry in &previous.files {
        if planned.contains_key(entry.path.as_str()) {
            continue;
        }
        let path = target.join(&entry.path);
        match fs::remove_file(&path) {
            Ok(()) => stats.removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(FsError::from_io(&path, e)),
        }
    }

    let planned_dirs: BTreeMap<&str, ()> = plan.dirs.iter().map(|dir| (dir.as_str(), ())).collect();
    let mut stale: Vec<&String> = previous
        .dirs
        .iter()
        .filter(|dir| !planned_dirs.contains_key(dir.as_str()))
        .collect();
    stale.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
    for dir in stale {
        let path = target.join(dir);
        match fs::remove_dir(&path) {
            Ok(()) => {}
            // A directory that vanished or still holds a user file is not ours to delete.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(e) => return Err(FsError::from_io(&path, e)),
        }
    }
    Ok(())
}

fn copy_permissions(from: &Path, to: &Path) -> Result<(), FsError> {
    let meta = fs::metadata(from).map_err(|e| FsError::from_io(from, e))?;
    fs::set_permissions(
        to,
        fs::Permissions::from_mode(meta.permissions().mode() & 0o7777),
    )
    .map_err(|e| FsError::from_io(to, e))
}
