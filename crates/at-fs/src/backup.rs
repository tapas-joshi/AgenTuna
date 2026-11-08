//! Backup copy primitive: copies bytes into a caller-supplied operation folder.
//!
//! This module never chooses a `backups/` layout, never creates directories and never
//! touches the database; TC-014 owns layout and catalogue. A backup is a blob named after
//! the original file plus a `<name>.json` sidecar.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
        if crate::errno::is_no_space(&source) || crate::errno::is_quota_exceeded(&source) {
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

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Name of the staged temp file used to install `dest` atomically.
///
/// The `.<name>.agentuna-tmp-<pid>-<n>` shape is the one
/// [`crate::recovery::sweep_stale_temp_files`] recognizes, so a crash cannot leave an
/// unrecognized blob behind.
fn temp_path(dest: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = dest
        .file_name()
        .map_or_else(|| "backup".into(), |name| name.to_string_lossy());
    dest.with_file_name(format!(".{name}.agentuna-tmp-{}-{n}", std::process::id()))
}

/// Writes `bytes` to `dest` through a same-directory temp file: write, fsync, rename.
///
/// The blob only ever appears at its final name after its bytes are on the device, so a
/// failure or crash in the middle leaves either nothing or a recognizable temp file, never
/// a truncated backup. The temp file is removed on every error path.
fn write_staged(dest: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    #[cfg(any(test, feature = "faults", feature = "testkit"))]
    if let Err(f) = crate::failpoint::hit(crate::failpoint::FaultPoint::BackupWrite, dest) {
        return Err(io_err(dest)(f.into_io()));
    }
    let temp = temp_path(dest);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // Owner-only staging file on Unix; on Windows the file inherits the operation folder's ACL.
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(io_err(dest))?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(io_err(dest)(error));
    }
    #[cfg(any(test, feature = "faults", feature = "testkit"))]
    if let Err(f) = crate::failpoint::hit(crate::failpoint::FaultPoint::BackupSync, dest) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(io_err(dest)(f.into_io()));
    }
    if let Err(error) = crate::durability::sync_all(&file) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(io_err(dest)(error));
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, dest) {
        let _ = fs::remove_file(&temp);
        return Err(io_err(dest)(error));
    }
    Ok(())
}

/// Whether `blob`/`sidecar` already hold exactly this copy, so re-running it is a no-op.
fn is_identical(blob: &Path, sidecar: &Path, original: &Path, hash: &str) -> bool {
    if !blob.is_file() || !sidecar.is_file() {
        return false;
    }
    let Ok(bytes) = fs::read(sidecar) else {
        return false;
    };
    matches!(
        serde_json::from_slice::<BackupSidecar>(&bytes),
        Ok(recorded) if recorded.content_hash == hash && recorded.original_path == original
    )
}

/// Copies `bytes` (the content of `original`) into `operation_dir` with a verified sidecar.
///
/// The blob is named after the original's file name and installed with temp+rename: its
/// bytes are fsynced under a `.<name>.agentuna-tmp-<pid>-<n>` staging name before the
/// rename, and only then is the sidecar written the same way. Re-running the same copy
/// (same original, same bytes) is idempotent and leaves one blob plus one sidecar; a
/// different copy whose file name collides gets a numeric suffix so both are preserved.
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
    let hash = sha256_hex(bytes);
    let sidecar_body = BackupSidecar {
        original_path: original.to_path_buf(),
        content_hash: hash.clone(),
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
        if is_identical(&blob, &sidecar, original, &hash) {
            return Ok(BackupFiles { blob, sidecar });
        }
        if blob.exists() || sidecar.exists() {
            continue;
        }
        write_staged(&blob, bytes)?;
        if let Err(e) = write_staged(&sidecar, &json) {
            let _ = fs::remove_file(&blob);
            return Err(e);
        }
        if let Err(e) = File::open(operation_dir).and_then(|d| crate::durability::sync_all(&d)) {
            let _ = fs::remove_file(&blob);
            let _ = fs::remove_file(&sidecar);
            return Err(io_err(operation_dir)(e));
        }
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
