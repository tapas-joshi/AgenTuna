//! Guarded atomic write: temp file, fsync, re-check, rename, fsync directory.

use crate::failpoint::{self, FaultPoint};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use time::OffsetDateTime;

use crate::backup::{self, BackupFiles, BackupMeta};
use crate::concurrency::{ExpectedState, Observed};
use crate::error::FsError;
use crate::guard::OverwriteGuard;
use crate::hash::sha256_hex;
use crate::symlink::{self, SymlinkWrite};
use crate::{MAX_FILE_BYTES, diff};

/// Whether the destination must be new or must already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// The destination must not exist; creation is exclusive.
    Create,
    /// The destination must exist and is replaced atomically.
    Replace,
}

/// Where, if anywhere, the replaced bytes are copied first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupPolicy {
    /// No backup.
    None,
    /// Copy the current bytes into this existing, caller-owned operation folder.
    OperationFolder(PathBuf),
}

/// A single guarded file write.
#[derive(Debug, Clone)]
pub struct WriteRequest {
    /// Destination path.
    pub path: PathBuf,
    /// The complete new content.
    pub new_bytes: Vec<u8>,
    /// What the destination must look like for the write to proceed.
    pub expected: ExpectedState,
    /// Backup behaviour.
    pub backup: BackupPolicy,
    /// Follow a symlink destination. Off by default; only agent config writers enable it.
    pub follow_symlink: bool,
    /// Create or replace.
    pub mode: WriteMode,
    /// Treat an empty directory at the destination as absent and replace it.
    pub allow_empty_dir: bool,
    /// Scope and relative path recorded in the backup sidecar.
    pub backup_meta: BackupMeta,
    /// Refuse-to-overwrite guard, checked before staging and again before the rename.
    /// Deployments and copies into agent folders must set this.
    pub guard: Option<Arc<dyn OverwriteGuard>>,
}

impl WriteRequest {
    /// A replace request with safe defaults: no backup, no symlink following.
    #[must_use]
    pub fn replace(path: impl Into<PathBuf>, new_bytes: Vec<u8>, expected: ExpectedState) -> Self {
        Self {
            path: path.into(),
            new_bytes,
            expected,
            backup: BackupPolicy::None,
            follow_symlink: false,
            mode: WriteMode::Replace,
            allow_empty_dir: false,
            backup_meta: BackupMeta::default(),
            guard: None,
        }
    }

    /// A create request: expects the destination to be absent.
    #[must_use]
    pub fn create(path: impl Into<PathBuf>, new_bytes: Vec<u8>) -> Self {
        Self {
            mode: WriteMode::Create,
            ..Self::replace(path, new_bytes, ExpectedState::Absent)
        }
    }
}

/// What a successful write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Unified diff (3 lines of context). Contains file content; redact before logging.
    pub diff: String,
    /// Backup blob, when a backup was taken.
    pub backup_path: Option<PathBuf>,
    /// SHA-256 of the replaced content; `None` when the file was created.
    pub before_hash: Option<String>,
    /// SHA-256 of the new content.
    pub after_hash: String,
    /// Bytes written.
    pub bytes: u64,
    /// Present when the write went through a symlink.
    pub symlink: Option<SymlinkWrite>,
}

impl WriteOutcome {
    /// Whether the bytes were written through a symlink.
    #[must_use]
    pub fn wrote_through_symlink(&self) -> bool {
        self.symlink.is_some()
    }
}

/// Points in `apply` where tests may observe or interrupt the pipeline.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Temp file written and fsynced.
    TempWritten,
    /// Backup created and fsynced.
    BackupCreated,
    /// Immediately before the destination is re-checked and renamed.
    BeforeRename,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Consults the failpoint registry; a no-op unless the `faults`/`testkit` feature is compiled in or in test.
/// Injected `ENOSPC`/`EDQUOT` map to the same typed errors a real full disk produces.
fn fault(point: FaultPoint, path: &Path) -> Result<(), FsError> {
    failpoint::hit(point, path)
        .map_err(|failure| FsError::from_platform_io(path, failure.into_io()))
}

/// Removes the temp file unless it was installed.
struct TempGuard {
    path: PathBuf,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Everything found at the destination, plus the bytes when it is a file.
struct Current {
    observed: Observed,
    bytes: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
    /// An empty directory sits at the destination and may be replaced.
    empty_dir: bool,
}

fn read_current(path: &Path, allow_empty_dir: bool) -> Result<Current, FsError> {
    let absent = Current {
        observed: Observed::Absent,
        bytes: None,
        permissions: None,
        empty_dir: false,
    };
    let lstat = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(absent),
        Err(e) => return Err(FsError::from_io(path, e)),
    };
    if lstat.file_type().is_symlink() {
        return Err(FsError::SymlinkRefused {
            path: path.to_path_buf(),
        });
    }
    if lstat.is_dir() {
        let empty = fs::read_dir(path)
            .map_err(|e| FsError::from_io(path, e))?
            .next()
            .is_none();
        return Ok(if empty && allow_empty_dir {
            Current {
                empty_dir: true,
                ..absent
            }
        } else {
            Current {
                observed: Observed::Directory,
                ..absent
            }
        });
    }
    if !lstat.is_file() {
        return Err(FsError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    let mut file = symlink::open_nofollow(path)?;
    let meta = file.metadata().map_err(|e| FsError::from_io(path, e))?;
    if !meta.is_file() {
        return Err(FsError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    let bytes = symlink::read_bounded(path, &mut file, &meta)?;
    let mtime = meta.modified().map_err(|e| FsError::from_io(path, e))?;
    Ok(Current {
        observed: Observed::File {
            hash: sha256_hex(&bytes),
            mtime,
        },
        bytes: Some(bytes),
        permissions: Some(meta.permissions()),
        empty_dir: false,
    })
}

fn create_temp(parent: &Path, name: &str) -> Result<(File, TempGuard), FsError> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = parent.join(format!(".{name}.agentuna-tmp-{}-{n}", std::process::id()));
    fault(FaultPoint::TempCreate, &path)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| FsError::from_platform_io(parent, e))?;
    Ok((file, TempGuard { path }))
}

fn concurrent(path: &Path, expected: &str, actual: &Observed) -> FsError {
    FsError::ConcurrentModification {
        path: path.to_path_buf(),
        expected: expected.to_owned(),
        actual: actual.describe(),
    }
}

fn sync_dir(dir: &Path) -> Result<(), FsError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| FsError::from_io(dir, e))
}

/// Applies `request`. See the module docs for the exact order of operations.
///
/// Nothing is written unless the destination matches `request.expected`. On any failure the
/// temp file and any backup taken by this call are removed and the original is untouched.
///
/// # Errors
/// See [`FsError`].
pub fn apply(request: &WriteRequest) -> Result<WriteOutcome, FsError> {
    apply_with_hook(request, &mut |_| {})
}

/// Like [`apply`], calling `hook` at each [`Stage`]. For tests only.
#[doc(hidden)]
pub fn apply_with_hook(
    request: &WriteRequest,
    hook: &mut dyn FnMut(Stage),
) -> Result<WriteOutcome, FsError> {
    let new_len = request.new_bytes.len() as u64;
    if new_len > MAX_FILE_BYTES {
        return Err(FsError::TooLarge {
            path: request.path.clone(),
            size: new_len,
            limit: MAX_FILE_BYTES,
        });
    }
    let (dest, symlink) = symlink::resolve(&request.path, request.follow_symlink)?;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| FsError::NoParent { path: dest.clone() })?;
    let name = dest
        .file_name()
        .ok_or_else(|| FsError::NoParent { path: dest.clone() })?
        .to_string_lossy()
        .into_owned();

    if let Some(guard) = &request.guard {
        guard.check(&dest, request.allow_empty_dir)?;
    }
    let current = read_current(&dest, request.allow_empty_dir)?;
    match (request.mode, &current.observed) {
        (_, Observed::Directory) => return Err(FsError::DestinationIsDirectory { path: dest }),
        (WriteMode::Create, Observed::File { .. }) => {
            return Err(FsError::AlreadyExists { path: dest });
        }
        (WriteMode::Replace, Observed::Absent) if !current.empty_dir => {
            return Err(FsError::NotFound { path: dest });
        }
        _ => {}
    }
    if !request.expected.matches(&current.observed) {
        return Err(concurrent(
            &dest,
            &request.expected.describe(),
            &current.observed,
        ));
    }

    // Stage the new content first: this proves the directory is writable before any backup exists.
    let (mut temp_file, temp) = create_temp(parent, &name)?;
    fault(FaultPoint::TempWrite, &temp.path)?;
    temp_file
        .write_all(&request.new_bytes)
        .map_err(|e| FsError::from_platform_io(&temp.path, e))?;
    if let Some(permissions) = &current.permissions {
        temp_file
            .set_permissions(permissions.clone())
            .map_err(|e| FsError::from_platform_io(&temp.path, e))?;
    }
    fault(FaultPoint::TempSync, &temp.path)?;
    temp_file
        .sync_all()
        .map_err(|e| FsError::from_platform_io(&temp.path, e))?;
    drop(temp_file);
    hook(Stage::TempWritten);

    let before_hash = match &current.observed {
        Observed::File { hash, .. } => Some(hash.clone()),
        _ => None,
    };
    let diff = diff::unified(
        &dest.to_string_lossy(),
        current.bytes.as_deref(),
        &request.new_bytes,
    );

    let mut backup_files: Option<BackupFiles> = None;
    if let (BackupPolicy::OperationFolder(dir), Some(old_bytes)) = (&request.backup, &current.bytes)
    {
        let files = backup::copy_into(
            dir,
            &dest,
            old_bytes,
            &request.backup_meta,
            OffsetDateTime::now_utc(),
        )?;
        backup_files = Some(files);
    }
    hook(Stage::BackupCreated);

    let result = install(&dest, &temp, request, &current, hook);
    match result {
        // The rename happened. A failed directory fsync is reported, but the backup is kept
        // because the new bytes are already in place.
        Ok(()) => {
            fault(FaultPoint::DirSync, parent)?;
            sync_dir(parent).map(|()| WriteOutcome {
                diff,
                backup_path: backup_files.map(|f| f.blob),
                before_hash,
                after_hash: sha256_hex(&request.new_bytes),
                bytes: new_len,
                symlink,
            })
        }
        Err(e) => {
            if let Some(files) = &backup_files {
                files.discard();
            }
            Err(e)
        }
    }
}

/// Re-checks the destination and moves the temp file into place.
fn install(
    dest: &Path,
    temp: &TempGuard,
    request: &WriteRequest,
    before: &Current,
    hook: &mut dyn FnMut(Stage),
) -> Result<(), FsError> {
    hook(Stage::BeforeRename);
    fault(FaultPoint::BeforeRename, dest)?;
    if let Some(guard) = &request.guard {
        guard.check(dest, request.allow_empty_dir)?;
    }
    let now = read_current(dest, request.allow_empty_dir)?;
    if now.observed != before.observed || now.empty_dir != before.empty_dir {
        return Err(concurrent(
            dest,
            &request.expected.describe(),
            &now.observed,
        ));
    }
    if before.empty_dir {
        fs::remove_dir(dest).map_err(|e| FsError::from_platform_io(dest, e))?;
    }
    fault(FaultPoint::Rename, dest)?;
    match request.mode {
        // hard_link fails if the destination appeared meanwhile, so creation is exclusive.
        WriteMode::Create => fs::hard_link(&temp.path, dest).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => FsError::ConcurrentModification {
                path: dest.to_path_buf(),
                expected: "absent".to_owned(),
                actual: "present".to_owned(),
            },
            _ => FsError::from_platform_io(dest, e),
        })?,
        WriteMode::Replace => {
            fs::rename(&temp.path, dest).map_err(|e| FsError::from_platform_io(dest, e))?
        }
    }
    Ok(())
}

/// Re-exported for tests that need to compute modes.
#[doc(hidden)]
#[must_use]
pub fn mode_bits(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}
