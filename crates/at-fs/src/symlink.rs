//! Symlink policy and non-following file access.

#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File, Metadata};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::MAX_FILE_BYTES;
use crate::error::FsError;

/// Records that a write went through a symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkWrite {
    /// The path the caller asked to write; still a symlink afterwards.
    pub link_path: PathBuf,
    /// The fully resolved file that actually received the bytes.
    pub resolved_path: PathBuf,
}

/// Resolves the real destination. Returns the path to operate on and any symlink traversed.
///
/// With `follow` false a symlink destination is refused. With `follow` true the link is
/// resolved once, by the OS, and all further operations use the resolved path.
pub(crate) fn resolve(
    path: &Path,
    follow: bool,
) -> Result<(PathBuf, Option<SymlinkWrite>), FsError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if !follow {
                return Err(FsError::SymlinkRefused {
                    path: path.to_path_buf(),
                });
            }
            let resolved = fs::canonicalize(path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FsError::DanglingSymlink {
                    path: path.to_path_buf(),
                },
                _ => FsError::from_io(path, e),
            })?;
            let write = SymlinkWrite {
                link_path: path.to_path_buf(),
                resolved_path: resolved.clone(),
            };
            Ok((resolved, Some(write)))
        }
        Ok(_) => Ok((path.to_path_buf(), None)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((path.to_path_buf(), None)),
        Err(e) => Err(FsError::from_io(path, e)),
    }
}

/// Opens `path` read-only without following a final symlink and without blocking on fifos.
pub(crate) fn open_nofollow(path: &Path) -> Result<File, FsError> {
    #[cfg(unix)]
    {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| FsError::from_io(path, e))
    }
    #[cfg(not(unix))]
    {
        // Windows has no `O_NOFOLLOW`. `FILE_FLAG_OPEN_REPARSE_POINT` would need an unsafe
        // `CreateFileW` call, which this workspace forbids, so the entry is opened and then
        // re-inspected: a link that appeared between the two calls is still refused, and a link
        // that is already there is refused before its target is read. The residual window is a
        // same-user race, which the single-user threat model already accepts.
        let file = File::open(path).map_err(|e| FsError::from_io(path, e))?;
        let meta = fs::symlink_metadata(path).map_err(|e| FsError::from_io(path, e))?;
        if meta.file_type().is_symlink() {
            return Err(FsError::SymlinkRefused {
                path: path.to_path_buf(),
            });
        }
        Ok(file)
    }
}

/// Reads at most [`MAX_FILE_BYTES`] from an opened regular file.
pub(crate) fn read_bounded(
    path: &Path,
    file: &mut File,
    meta: &Metadata,
) -> Result<Vec<u8>, FsError> {
    if meta.len() > MAX_FILE_BYTES {
        return Err(FsError::TooLarge {
            path: path.to_path_buf(),
            size: meta.len(),
            limit: MAX_FILE_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| FsError::from_io(path, e))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FsError::TooLarge {
            path: path.to_path_buf(),
            size: bytes.len() as u64,
            limit: MAX_FILE_BYTES,
        });
    }
    Ok(bytes)
}

/// Reads a regular file without following its final path component.
///
/// # Errors
/// Returns [`FsError::SymlinkRefused`] for a symbolic link and the usual IO or size error for
/// other invalid or unreadable targets.
pub fn read_nofollow(path: &Path) -> Result<Vec<u8>, FsError> {
    let mut file = open_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| FsError::from_io(path, error))?;
    if !metadata.is_file() {
        return Err(FsError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    read_bounded(path, &mut file, &metadata)
}

/// Reads a regular file, following a final symbolic link to its resolved target.
///
/// For agent configuration documents a dotfile manager commonly symlinks. The caller decides
/// whether following is allowed ([`at_model`]'s `follow_symlink` and the adapter's decision); the
/// read is bounded exactly like [`read_nofollow`].
///
/// # Errors
/// Returns [`FsError::DanglingSymlink`] for a link whose target is missing and the usual IO,
/// size or non-regular-file error for other invalid targets.
pub fn read_following(path: &Path) -> Result<Vec<u8>, FsError> {
    let (destination, _) = resolve(path, true)?;
    let mut file =
        File::open(&destination).map_err(|error| FsError::from_io(&destination, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| FsError::from_io(&destination, error))?;
    if !metadata.is_file() {
        return Err(FsError::NotRegularFile { path: destination });
    }
    read_bounded(&destination, &mut file, &metadata)
}
