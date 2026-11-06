//! Typed errors for filesystem operations. Messages carry paths and hashes, never content.

use std::io;
use std::path::{Path, PathBuf};

use crate::backup::BackupError;
use crate::guard::{Remedy, TargetKind};

/// Why a filesystem operation did not happen (or did not complete).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsError {
    /// The destination changed since the caller looked. Nothing was written.
    #[error("{path}: concurrent modification (expected {expected}, found {actual})", path = .path.display())]
    ConcurrentModification {
        /// Destination path.
        path: PathBuf,
        /// What the caller expected (`absent` or a hash).
        expected: String,
        /// What is there now (`absent`, `directory` or a hash).
        actual: String,
    },
    /// The destination is a symlink and `follow_symlink` is false.
    #[error("{}: destination is a symlink and following symlinks is not allowed", .path.display())]
    SymlinkRefused {
        /// Destination path.
        path: PathBuf,
    },
    /// The destination is a symlink whose target does not exist.
    #[error("{}: symlink target does not exist", .path.display())]
    DanglingSymlink {
        /// Destination path.
        path: PathBuf,
    },
    /// The operating system denied access.
    #[error("{}: permission denied", .path.display())]
    PermissionDenied {
        /// Path that could not be accessed.
        path: PathBuf,
    },
    /// `WriteMode::Create` but something already exists at the destination.
    #[error("{}: already exists", .path.display())]
    AlreadyExists {
        /// Destination path.
        path: PathBuf,
    },
    /// `WriteMode::Replace` but nothing exists at the destination, or the parent is missing.
    #[error("{}: not found", .path.display())]
    NotFound {
        /// Missing path.
        path: PathBuf,
    },
    /// The destination is a directory (or a non-empty one) and may not be replaced.
    #[error("{}: destination is a directory", .path.display())]
    DestinationIsDirectory {
        /// Destination path.
        path: PathBuf,
    },
    /// The destination is not a regular file (device, socket, fifo).
    #[error("{}: not a regular file", .path.display())]
    NotRegularFile {
        /// Destination path.
        path: PathBuf,
    },
    /// The file or the new content exceeds [`crate::MAX_FILE_BYTES`].
    #[error("{}: {size} bytes exceeds the {limit} byte limit", .path.display())]
    TooLarge {
        /// Path involved.
        path: PathBuf,
        /// Observed or requested size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// The destination has no parent directory to stage a temp file in.
    #[error("{}: path has no parent directory", .path.display())]
    NoParent {
        /// Destination path.
        path: PathBuf,
    },
    /// Something AgenTuna does not own already exists at the target. Nothing was written.
    #[error("{}: {kind:?} already exists and is not managed by AgenTuna", .path.display())]
    RefusedOverwrite {
        /// Target path.
        path: PathBuf,
        /// What is there.
        kind: TargetKind,
        /// SHA-256 of the existing file, when it is a hashable regular file.
        existing_hash: Option<String>,
        /// What the user can do next.
        remedies: Vec<Remedy>,
    },
    /// The path is not usable (relative, contains `..`, or an ancestor is not a directory).
    #[error("{}: {reason}", .path.display())]
    InvalidPath {
        /// Offending path.
        path: PathBuf,
        /// Why.
        reason: &'static str,
    },
    /// The platform refused to create a symbolic link (`EPERM`, `ENOTSUP`, `ENOSYS`).
    #[error("{}: the platform does not allow symbolic links here: {source}", .path.display())]
    LinkUnsupported {
        /// The deployment path a link was requested for.
        path: PathBuf,
        /// The error the operating system returned.
        #[source]
        source: io::Error,
    },
    /// A rename would have to cross filesystems (`EXDEV`).
    #[error(
        "{}: cannot rename across filesystems to {}",
        .from.display(),
        .to.display()
    )]
    CrossDeviceRename {
        /// Rename source.
        from: PathBuf,
        /// Rename destination.
        to: PathBuf,
    },
    /// The copy manifest is unreadable or does not describe this deployment.
    #[error("{}: {reason}", .path.display())]
    InvalidManifest {
        /// Manifest path.
        path: PathBuf,
        /// Why it cannot be used.
        reason: String,
    },
    /// Removal was refused because the path is not an AgenTuna-owned deployment.
    #[error("{}: refusing to remove the deployment: {reason}", .path.display())]
    DeploymentNotOwned {
        /// Path removal was asked to delete.
        path: PathBuf,
        /// Why the path is not removable.
        reason: &'static str,
    },
    /// Backup creation or verification failed.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// Any other IO failure.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The volume cannot hold the write (`ENOSPC`). Nothing was written.
    #[error("{}: the disk is full", .path.display())]
    NoSpaceLeft {
        /// Path involved.
        path: PathBuf,
    },
    /// The volume or user quota is exhausted (`EDQUOT`). Nothing was written.
    #[error("{}: the disk quota is exhausted", .path.display())]
    QuotaExceeded {
        /// Path involved.
        path: PathBuf,
    },
    /// A link would have to cross filesystems and the policy does not allow that.
    #[error(
        "{}: cannot link across volumes to {}",
        .from.display(),
        .to.display()
    )]
    CrossVolume {
        /// Canonical store source.
        from: PathBuf,
        /// Deployment path that would receive the link.
        to: PathBuf,
    },
    /// The deployment path is a symlink pointing somewhere other than the store item.
    #[error("{}: existing symlink points to {}", .path.display(), .target.display())]
    ForeignLink {
        /// The deployment path.
        path: PathBuf,
        /// What the existing link resolves to.
        target: PathBuf,
    },
    /// The requested operation does not exist on this platform.
    #[error("{}: {reason}", .path.display())]
    UnsupportedPlatform {
        /// Path the operation was requested for.
        path: PathBuf,
        /// Why the platform cannot perform it.
        reason: &'static str,
    },
    /// A cross-volume copy did not reproduce the source byte for byte.
    #[error("{}: copy verification failed: {reason}", .path.display())]
    CopyVerificationFailed {
        /// Path that was copied.
        path: PathBuf,
        /// Why the verification failed.
        reason: String,
    },
}

impl FsError {
    /// One actionable sentence for a UI toast or CLI hint; `None` when retrying cannot help.
    ///
    /// The text states what to do and whether anything was written. It never contains a secret.
    #[must_use]
    pub fn remediation(&self) -> Option<&'static str> {
        match self {
            Self::PermissionDenied { .. } => Some(
                "Grant write access to this folder or choose a location you own; nothing was written.",
            ),
            Self::NoSpaceLeft { .. } => {
                Some("Free disk space on this volume, then retry; nothing was written.")
            }
            Self::QuotaExceeded { .. } => {
                Some("Raise the disk quota for this volume, then retry; nothing was written.")
            }
            Self::CrossVolume { .. } => Some(
                "Enable copy fallback for this agent, or keep the store and agent folder on one volume.",
            ),
            Self::ForeignLink { .. } => Some(
                "Move the existing link aside, adopt the item it points at, or cancel; nothing was written.",
            ),
            Self::RefusedOverwrite { .. } => {
                Some("Adopt the existing item, move it aside, or cancel; nothing was written.")
            }
            Self::LinkUnsupported { .. } => {
                Some("Choose copy mode for this agent; nothing was written.")
            }
            Self::UnsupportedPlatform { .. } => {
                Some("This action is unavailable on this operating system.")
            }
            Self::ConcurrentModification { .. } => {
                Some("Re-read the file and review the new change before writing again.")
            }
            Self::DeploymentNotOwned { .. } => {
                Some("Only an AgenTuna-owned deployment can be removed; nothing was deleted.")
            }
            Self::Backup(_) => Some(
                "The original was not replaced because its backup failed; check disk space and permissions.",
            ),
            Self::CopyVerificationFailed { .. } => Some(
                "The copy did not match the source, so the original was left untouched; retry.",
            ),
            Self::SymlinkRefused { .. } => {
                Some("Follow the symlink explicitly or choose another path.")
            }
            Self::DanglingSymlink { .. } => {
                Some("Remove the dangling link or point it at an existing item.")
            }
            Self::InvalidPath { .. } => Some("Use an absolute path without `..` components."),
            Self::TooLarge { .. } => {
                Some("Choose a smaller file; AgenTuna refuses files above its size limit.")
            }
            Self::AlreadyExists { .. } => Some("Choose another name or remove the existing entry."),
            Self::NotFound { .. } => Some("Refresh the view; the path no longer exists."),
            Self::DestinationIsDirectory { .. } => {
                Some("Choose a file path, or remove the directory first.")
            }
            Self::NotRegularFile { .. } => {
                Some("Only regular files can be written; choose another path.")
            }
            Self::NoParent { .. } => Some("Use a path that has a parent directory."),
            Self::InvalidManifest { .. } => {
                Some("Re-deploy this item; the copy manifest is unusable and removal is refused.")
            }
            Self::CrossDeviceRename { .. } => {
                Some("Keep the store and the target on one volume, or use copy mode.")
            }
            Self::Io { .. } => Some("Check the path and permissions, then retry."),
        }
    }

    /// Maps an IO error on `path` to the most specific variant.
    pub(crate) fn from_platform_io(path: &Path, source: io::Error) -> Self {
        #[cfg(unix)]
        {
            let Some(code) = source.raw_os_error() else {
                return Self::from_io(path, source);
            };
            if code == libc::ENOSPC {
                return Self::NoSpaceLeft {
                    path: path.to_path_buf(),
                };
            }
            if code == libc::EDQUOT {
                return Self::QuotaExceeded {
                    path: path.to_path_buf(),
                };
            }
        }
        Self::from_io(path, source)
    }
}

impl FsError {
    /// Maps an IO error on `path` to the most specific variant.
    pub(crate) fn from_io(path: &Path, source: io::Error) -> Self {
        match source.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.to_path_buf(),
            },
            io::ErrorKind::NotFound => Self::NotFound {
                path: path.to_path_buf(),
            },
            io::ErrorKind::AlreadyExists => Self::AlreadyExists {
                path: path.to_path_buf(),
            },
            _ if source.raw_os_error() == Some(libc::ELOOP) => Self::SymlinkRefused {
                path: path.to_path_buf(),
            },
            _ => Self::Io {
                path: path.to_path_buf(),
                source,
            },
        }
    }
}
