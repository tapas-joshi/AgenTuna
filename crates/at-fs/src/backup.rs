//! Backup copy primitive: copies bytes into a caller-supplied operation folder.
//!
//! This module never chooses a `backups/` layout, never creates directories and never
//! touches the database; TC-014 owns layout and catalogue. A backup is a blob named after
//! the original file plus a `<name>.json` sidecar.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use at_model::Scope;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::hash::sha256_hex;

/// Why a backup could not be created or verified.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// The operation folder does not exist or is not a directory.
    #[error("backup folder {} is not an existing directory", .0.display())]
    FolderMissing(PathBuf),
    /// The blob or its sidecar is missing, unreadable or does not match its recorded hash.
    #[error("backup {} is corrupt: {reason}", .path.display())]
    Corrupt {
        /// Blob path.
        path: PathBuf,
        /// What was wrong.
        reason: String,
    },
    /// Permission denied, disk full, or other IO error while writing the backup copy.
    #[error("backup failed at {}: {source}", .path.display())]
    BackupFailed {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// IO failure while writing or reading the backup.
    #[error("backup IO error at {}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Caller-supplied context recorded in the sidecar.
#[derive(Debug, Clone, Default)]
pub struct BackupMeta {
    /// Scope of the backed-up item, when known.
    pub scope: Option<Scope>,
    /// Path of the original relative to its store root, when known.
    pub relpath: Option<String>,
    /// Operation owning the backup, when known.
    pub operation_id: Option<String>,
    /// Unix permission bits of the original, when known.
    pub mode: Option<u32>,
}

/// JSON sidecar written next to each backup blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupSidecar {
    /// Where the bytes came from.
    pub original_path: PathBuf,
    /// SHA-256 of the blob, lowercase hex.
    pub content_hash: String,
    /// Blob size in bytes.
    pub size: u64,
    /// When the backup was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
    /// Scope of the item, when known.
    pub scope: Option<Scope>,
    /// Path relative to the store root, when known.
    pub relpath: Option<String>,
    /// Owning operation, when this is an operation backup.
    #[serde(default)]
    pub operation_id: Option<String>,
    /// Original Unix permission bits, when known.
    #[serde(default)]
    pub mode: Option<u32>,
}

/// The files that make up one backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFiles {
    /// The copied bytes.
    pub blob: PathBuf,
    /// The metadata sidecar.
    pub sidecar: PathBuf,
}

impl BackupFiles {
    /// Best-effort removal, used to roll back a backup whose write never happened.
    pub(crate) fn discard(&self) {
        let _ = fs::remove_file(&self.blob);
        let _ = fs::remove_file(&self.sidecar);
    }
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> BackupError + '_ {
    move |source| {
        #[cfg(unix)]
        if let Some(code) = source.raw_os_error()
            && (code == libc::ENOSPC || code == libc::EDQUOT)
        {
            return BackupError::BackupFailed {
                path: path.to_path_buf(),
                source,
            };
        }
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            return BackupError::BackupFailed {
                path: path.to_path_buf(),
                source,
            };
        }
        BackupError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Writes `bytes` to a fresh file at `dest` (exclusive create), fsynced.
fn write_new(dest: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    #[cfg(any(test, feature = "faults", feature = "testkit"))]
    if let Err(f) = crate::failpoint::hit(crate::failpoint::FaultPoint::BackupWrite, dest) {
        return Err(io_err(dest)(f.into_io()));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(io_err(dest))?;
    file.write_all(bytes).map_err(io_err(dest))?;
    #[cfg(any(test, feature = "faults", feature = "testkit"))]
    if let Err(f) = crate::failpoint::hit(crate::failpoint::FaultPoint::BackupSync, dest) {
        return Err(io_err(dest)(f.into_io()));
    }
    file.sync_all().map_err(io_err(dest))
}

/// Copies `bytes` (the content of `original`) into `operation_dir` with a verified sidecar.
///
/// The blob is named after the original's file name; a name collision inside the folder
/// gets a numeric suffix. Both files are written with exclusive create and fsynced, then
/// the folder is fsynced.
///
/// # Errors
/// [`BackupError::FolderMissing`] when `operation_dir` is not a directory; IO errors otherwise.
pub fn copy_into(
    operation_dir: &Path,
    original: &Path,
    bytes: &[u8],
    meta: &BackupMeta,
    captured_at: OffsetDateTime,
) -> Result<BackupFiles, BackupError> {
    if !operation_dir.is_dir() {
        return Err(BackupError::FolderMissing(operation_dir.to_path_buf()));
    }
    let base = original
        .file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
    let sidecar_body = BackupSidecar {
        original_path: original.to_path_buf(),
        content_hash: sha256_hex(bytes),
        size: bytes.len() as u64,
        captured_at,
        scope: meta.scope,
        relpath: meta.relpath.clone(),
        operation_id: meta.operation_id.clone(),
        mode: meta.mode,
    };
    let json = serde_json::to_vec_pretty(&sidecar_body).map_err(|e| BackupError::Corrupt {
        path: original.to_path_buf(),
        reason: e.to_string(),
    })?;

    for attempt in 0u32..1000 {
        let name = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}.{attempt}")
        };
        let blob = operation_dir.join(&name);
        let sidecar = operation_dir.join(format!("{name}.json"));
        if blob.exists() || sidecar.exists() {
            continue;
        }
        write_new(&blob, bytes)?;
        if let Err(e) = write_new(&sidecar, &json) {
            let _ = fs::remove_file(&blob);
            return Err(e);
        }
        File::open(operation_dir)
            .and_then(|d| d.sync_all())
            .map_err(io_err(operation_dir))?;
        return Ok(BackupFiles { blob, sidecar });
    }
    Err(BackupError::Corrupt {
        path: operation_dir.join(&base),
        reason: "too many name collisions".to_owned(),
    })
}

/// Re-reads blob and sidecar and checks them against `expected_hash`.
///
/// # Errors
/// [`BackupError::Corrupt`] on a missing sidecar, unreadable blob, or any hash/size mismatch.
pub fn verify(blob: &Path, expected_hash: &str) -> Result<BackupSidecar, BackupError> {
    let corrupt = |reason: &str| BackupError::Corrupt {
        path: blob.to_path_buf(),
        reason: reason.to_owned(),
    };
    let name = blob
        .file_name()
        .ok_or_else(|| corrupt("no file name"))?
        .to_string_lossy()
        .into_owned();
    let sidecar_path = blob.with_file_name(format!("{name}.json"));
    let sidecar_bytes =
        fs::read(&sidecar_path).map_err(|_| corrupt("sidecar missing or unreadable"))?;
    let sidecar: BackupSidecar =
        serde_json::from_slice(&sidecar_bytes).map_err(|_| corrupt("sidecar is not valid JSON"))?;
    let data = fs::read(blob).map_err(|_| corrupt("blob missing or unreadable"))?;
    if sha256_hex(&data) != sidecar.content_hash {
        return Err(corrupt("blob does not match sidecar hash"));
    }
    if sidecar.content_hash != expected_hash {
        return Err(corrupt("hash differs from the expected hash"));
    }
    if sidecar.size != data.len() as u64 {
        return Err(corrupt("size differs from sidecar"));
    }
    Ok(sidecar)
}

/// Builds the operation folder `backups/<utc-timestamp>-<operation-id>`.
///
/// # Errors
/// [`BackupError::Corrupt`] when `operation_id` is empty or contains path syntax.
pub fn operation_backup_dir(
    backups_root: &Path,
    created_at: OffsetDateTime,
    operation_id: &str,
) -> Result<PathBuf, BackupError> {
    if operation_id.is_empty()
        || !operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(BackupError::Corrupt {
            path: backups_root.to_path_buf(),
            reason: "invalid operation id".to_owned(),
        });
    }
    let timestamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        created_at.year(),
        u8::from(created_at.month()),
        created_at.day(),
        created_at.hour(),
        created_at.minute(),
        created_at.second(),
    );
    Ok(backups_root.join(format!("{timestamp}-{operation_id}")))
}
