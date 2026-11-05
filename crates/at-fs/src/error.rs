//! Typed errors for filesystem operations. Messages carry paths and hashes, never content.

use std::io;
use std::path::{Path, PathBuf};

use crate::backup::BackupError;

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
