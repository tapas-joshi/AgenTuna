//! Debris an interrupted write leaves behind, and how it is repaired at the next start.
//!
//! [`crate::apply`] stages a temp file next to its destination and removes it on every error
//! path, but a `SIGKILL` between staging and rename bypasses that cleanup. A multi-file change
//! set (TF-053) additionally writes a journal before it starts, so the next start can tell which
//! files were mid-flight. This module owns both kinds of debris:
//!
//! * [`scan_temp_files`] / [`sweep_stale_temp_files`] find and remove
//!   `.<name>.agentuna-tmp-<pid>-<counter>` files, and nothing else. A file whose name does not
//!   parse as a staged temp file is never reported and never deleted.
//! * [`read_journal`] / [`assess_journal`] re-read a pending journal and classify each recorded
//!   destination as still holding the *before* bytes, already holding the *after* bytes, or
//!   neither ([`EntryState::Diverged`], which requires a human).
//! * [`restore_file`] puts the before bytes back through [`crate::apply`], so a restore is an
//!   ordinary guarded write that backs up the current bytes first.
//! * [`discard_pending`] removes a journal and the temp files it names, and never touches a
//!   recorded destination.
//!
//! Everything here is deterministic and dependency-free: `at-fs` does not know about SQLite, so
//! the `operations` rows are written by the caller through a seam of its own.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::MAX_FILE_BYTES;
use crate::atomic::{BackupPolicy, WriteMode, WriteOutcome, WriteRequest, apply};
use crate::backup::BackupMeta;
use crate::concurrency::ExpectedState;
use crate::error::FsError;
use crate::hash::sha256_hex;

/// Infix of every temp file [`crate::apply`] stages.
///
/// A staged name is `.<original name>.agentuna-tmp-<pid>-<counter>`. The leading dot and the
/// fixed infix are what make debris recognizable without any side table.
pub const TEMP_MARKER: &str = ".agentuna-tmp-";

/// How long a journal or temp file survives without a decision.
///
/// Recovery removes what is older than this automatically; anything fresher stays until the user
/// chooses Restore or Discard.
pub const RETENTION: Duration = Duration::days(7);

/// Extension of a pending operation journal.
pub const JOURNAL_EXTENSION: &str = "journal";

/// Journal format version written by [`write_journal`] and accepted by [`read_journal`].
pub const JOURNAL_VERSION: u32 = 1;

/// Deepest directory level [`scan_temp_files`] descends below each root.
///
/// Store layouts are shallow (`skills/<slug>/SKILL.md`); the bound stops a symlinked or
/// unexpectedly large tree from turning a startup scan into a full disk walk.
pub const MAX_SCAN_DEPTH: usize = 6;

/// Directory names a scan never descends into.
const SKIPPED_DIRS: [&str; 1] = [".git"];

/// One staged temp file left in a store directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempFile {
    /// Absolute path of the staged file.
    pub path: PathBuf,
    /// Original file name the bytes were staged for, as recorded in the staged name.
    pub target: String,
    /// Last modification time, when the filesystem reports one.
    pub modified: Option<OffsetDateTime>,
    /// Whether the file is older than the retention window and may be removed unasked.
    pub stale: bool,
}

/// What [`sweep_stale_temp_files`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Temp files removed because they were older than the retention window.
    pub removed: Vec<PathBuf>,
    /// Temp files left in place because they were still fresh.
    pub retained: Vec<PathBuf>,
    /// Temp files that could not be removed. Left for the user to inspect.
    pub failed: Vec<PathBuf>,
}

/// One file a journal recorded before the write started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Destination that was about to be written.
    pub path: PathBuf,
    /// SHA-256 of the bytes before the write; `None` when the file did not exist yet.
    pub before_hash: Option<String>,
    /// SHA-256 of the bytes the write was going to install.
    pub after_hash: String,
}

/// A journal of an operation that did not finish.
///
/// One journal describes one operation. JSON on disk, one file per operation, named by
/// [`journal_file_name`]. TF-053 writes it before the first byte moves; this module only reads
/// (and, for tests and repair, writes) it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingJournal {
    /// Format version; [`JOURNAL_VERSION`] for anything this crate writes.
    pub journal_version: u32,
    /// The `operations` row the journal belongs to.
    pub operation_id: String,
    /// When the operation started.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// Every destination the operation was going to touch.
    pub entries: Vec<JournalEntry>,
    /// Temp files the operation staged before the crash, if it recorded them.
    #[serde(default)]
    pub temp_files: Vec<PathBuf>,
}

/// What the destination currently holds relative to a [`JournalEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryState {
    /// The bytes still match `before_hash` (or the file is still absent): the write never landed.
    Before,
    /// The bytes match `after_hash`: the write landed and can be undone by a restore.
    After,
    /// The bytes match neither hash, so a third party changed the file.
    ///
    /// Recovery reports this and writes nothing: only a human can decide which version wins.
    Diverged {
        /// SHA-256 of what is there now; `None` when nothing is.
        observed: Option<String>,
    },
}

impl EntryState {
    /// Stable machine identifier used by the Recovery sheet and by logs.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::Diverged { .. } => "diverged",
        }
    }
}

/// One journal entry together with what is actually on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryAssessment {
    /// The recorded entry.
    pub entry: JournalEntry,
    /// What the destination holds now.
    pub state: EntryState,
}

/// A re-read journal, with every entry classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalAssessment {
    /// Journal file the assessment came from.
    pub path: PathBuf,
    /// The decoded journal.
    pub journal: PendingJournal,
    /// One assessment per recorded entry, in journal order.
    pub entries: Vec<EntryAssessment>,
}

impl JournalAssessment {
    /// Whether any entry needs a human: recovery must not write anything for this journal.
    #[must_use]
    pub fn needs_manual_review(&self) -> bool {
        self.entries
            .iter()
            .any(|assessed| matches!(assessed.state, EntryState::Diverged { .. }))
    }

    /// Entries whose write landed and whose before bytes can be restored.
    #[must_use]
    pub fn restorable(&self) -> Vec<&EntryAssessment> {
        self.entries
            .iter()
            .filter(|assessed| matches!(assessed.state, EntryState::After))
            .collect()
    }
}

/// What [`discard_pending`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscardOutcome {
    /// Paths that were deleted: the journal and the temp files it named.
    pub removed: Vec<PathBuf>,
    /// Recorded temp paths that were not deleted because they are not staged temp files.
    ///
    /// A journal is data read back from disk; refusing to delete anything whose name does not
    /// parse as debris means a corrupt journal cannot ask recovery to delete a config file.
    pub skipped: Vec<PathBuf>,
}

/// A request to put the before bytes of one file back.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// Destination to restore.
    pub path: PathBuf,
    /// The bytes recorded as `before_hash`; usually read from an operation backup.
    pub before_bytes: Vec<u8>,
    /// SHA-256 the caller claims [`Self::before_bytes`] hash to.
    pub before_hash: String,
    /// SHA-256 of what is at [`Self::path`] right now, from [`EntryState::After`].
    pub expected_current_hash: String,
    /// Operation folder that receives the backup of the current bytes.
    pub operation_folder: PathBuf,
    /// Scope and relative path recorded in the backup sidecar.
    pub backup_meta: BackupMeta,
}

/// Why recovery could not inspect or repair the filesystem. Messages carry paths, never content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// A filesystem call failed.
    #[error("{}: {message}", path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Operating-system error text.
        message: String,
    },
    /// A journal file is not valid JSON, has an unknown version, or cannot be encoded.
    #[error("{}: malformed recovery journal: {reason}", path.display())]
    MalformedJournal {
        /// Journal path.
        path: PathBuf,
        /// What was wrong.
        reason: String,
    },
    /// The bytes offered for a restore do not hash to the recorded before hash.
    #[error(
        "{}: backup does not match the recorded before hash (expected {expected}, got {actual})",
        path.display()
    )]
    BackupMismatch {
        /// Destination being restored.
        path: PathBuf,
        /// Hash the journal recorded.
        expected: String,
        /// Hash of the bytes that were about to be written.
        actual: String,
    },
    /// A recorded destination is larger than [`crate::MAX_FILE_BYTES`] and is not read.
    #[error("{}: {size} bytes exceeds the {limit} byte limit", path.display())]
    TooLarge {
        /// Offending path.
        path: PathBuf,
        /// Observed size.
        size: u64,
        /// Limit that was exceeded.
        limit: u64,
    },
    /// The guarded write that performs the restore failed.
    #[error(transparent)]
    Write(#[from] FsError),
}

fn io_error(path: &Path, error: io::Error) -> RecoveryError {
    RecoveryError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// The original file name a staged temp file was created for.
///
/// Returns `None` for anything that is not an [`crate::apply`] temp file, so a stray file with a
/// similar name in a store directory can never be mistaken for debris:
///
/// ```
/// use at_fs::recovery::temp_target_name;
///
/// assert_eq!(temp_target_name(".config.json.agentuna-tmp-4711-0"), Some("config.json"));
/// assert_eq!(temp_target_name("config.json"), None);
/// assert_eq!(temp_target_name(".agentuna-tmp-4711-0"), None);
/// ```
#[must_use]
pub fn temp_target_name(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix('.')?;
    let marker = rest.rfind(TEMP_MARKER)?;
    let (target, suffix) = rest.split_at(marker);
    if target.is_empty() {
        return None;
    }
    let (pid, counter) = suffix[TEMP_MARKER.len()..].split_once('-')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(pid) || !digits(counter) {
        return None;
    }
    Some(target)
}

/// Whether `file_name` is a temp file [`crate::apply`] may have staged.
#[must_use]
pub fn is_temp_file(file_name: &str) -> bool {
    temp_target_name(file_name).is_some()
}

/// Every staged temp file below `roots`, sorted by path.
///
/// The walk is bounded by [`MAX_SCAN_DEPTH`], skips `.git` directories, never follows a symlinked
/// directory and ignores directories it cannot read: a read-only Global store must not fail the
/// scan that also reports the writable stores.
///
/// `stale` is computed against `now - max_age`; a file whose modification time cannot be read is
/// never stale, because deleting it would be a guess.
#[must_use]
pub fn scan_temp_files(roots: &[PathBuf], now: OffsetDateTime, max_age: Duration) -> Vec<TempFile> {
    let mut found = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for root in roots {
        let mut stack = vec![(root.clone(), 0_usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    let name = entry.file_name();
                    let skipped = SKIPPED_DIRS
                        .iter()
                        .any(|candidate| name == std::ffi::OsStr::new(candidate));
                    if depth < MAX_SCAN_DEPTH && !skipped {
                        stack.push((path, depth + 1));
                    }
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let Some(target) = name.to_str().and_then(temp_target_name) else {
                    continue;
                };
                if !seen.insert(path.clone()) {
                    continue;
                }
                let modified = fs::symlink_metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(OffsetDateTime::from);
                let stale = modified.is_some_and(|modified| now - modified > max_age);
                found.push(TempFile {
                    path,
                    target: target.to_owned(),
                    modified,
                    stale,
                });
            }
        }
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found
}

/// Removes the temp files older than `max_age` and reports every file it looked at.
///
/// Unrelated files in the same directories are never considered: the scan only yields names that
/// parse as [`TEMP_MARKER`] debris.
#[must_use]
pub fn sweep_stale_temp_files(
    roots: &[PathBuf],
    now: OffsetDateTime,
    max_age: Duration,
) -> SweepReport {
    let mut report = SweepReport::default();
    for file in scan_temp_files(roots, now, max_age) {
        if !file.stale {
            report.retained.push(file.path);
            continue;
        }
        match fs::remove_file(&file.path) {
            Ok(()) => report.removed.push(file.path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => report.failed.push(file.path),
        }
    }
    report
}

/// `<operation_id>.journal`.
#[must_use]
pub fn journal_file_name(operation_id: &str) -> String {
    format!("{operation_id}.{JOURNAL_EXTENSION}")
}

/// The operation id encoded in a journal file name, or `None` for another file.
#[must_use]
pub fn journal_operation_id(path: &Path) -> Option<String> {
    if path.extension()? != JOURNAL_EXTENSION {
        return None;
    }
    path.file_stem()?.to_str().map(ToOwned::to_owned)
}

/// Every `*.journal` file in `dir`, sorted by path. A missing directory has no journals.
///
/// # Errors
/// [`RecoveryError::Io`] when the directory exists but cannot be read.
pub fn scan_journals(dir: &Path) -> Result<Vec<PathBuf>, RecoveryError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(dir, error)),
    };
    let mut journals = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if journal_operation_id(&path).is_some() && path.is_file() {
            journals.push(path);
        }
    }
    journals.sort();
    Ok(journals)
}

/// Writes `journal` to `dir` as one JSON file, fsynced.
///
/// TF-053 calls this before the first byte of a multi-file change set moves; recovery calls it in
/// tests. Writing the same operation twice replaces the file, so a retried operation keeps one
/// journal.
///
/// # Errors
/// [`RecoveryError::Io`] when the directory or file cannot be written,
/// [`RecoveryError::MalformedJournal`] when the journal cannot be encoded.
pub fn write_journal(dir: &Path, journal: &PendingJournal) -> Result<PathBuf, RecoveryError> {
    fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
    let path = dir.join(journal_file_name(&journal.operation_id));
    let bytes =
        serde_json::to_vec_pretty(journal).map_err(|error| RecoveryError::MalformedJournal {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    let mut file = fs::File::create(&path).map_err(|error| io_error(&path, error))?;
    file.write_all(&bytes)
        .map_err(|error| io_error(&path, error))?;
    file.sync_all().map_err(|error| io_error(&path, error))?;
    Ok(path)
}

/// Reads and validates one journal file.
///
/// # Errors
/// [`RecoveryError::Io`] when the file cannot be read, [`RecoveryError::MalformedJournal`] when it
/// is not JSON or carries a version this binary does not understand.
pub fn read_journal(path: &Path) -> Result<PendingJournal, RecoveryError> {
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    let journal: PendingJournal =
        serde_json::from_slice(&bytes).map_err(|error| RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    if journal.journal_version != JOURNAL_VERSION {
        return Err(RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: format!(
                "journal version {} is not supported by this build (expected {JOURNAL_VERSION})",
                journal.journal_version
            ),
        });
    }
    Ok(journal)
}

/// Hashes `path` and says whether it still holds the before bytes, the after bytes, or neither.
///
/// # Errors
/// [`RecoveryError::Io`] when the destination cannot be inspected,
/// [`RecoveryError::TooLarge`] when it exceeds [`crate::MAX_FILE_BYTES`].
pub fn assess_entry(entry: &JournalEntry) -> Result<EntryState, RecoveryError> {
    let observed = match fs::symlink_metadata(&entry.path) {
        Ok(metadata) if metadata.is_file() => {
            let size = metadata.len();
            if size > MAX_FILE_BYTES {
                return Err(RecoveryError::TooLarge {
                    path: entry.path.clone(),
                    size,
                    limit: MAX_FILE_BYTES,
                });
            }
            let bytes = fs::read(&entry.path).map_err(|error| io_error(&entry.path, error))?;
            Some(sha256_hex(&bytes))
        }
        // A directory or symlink where a file was expected is not a version of that file.
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error(&entry.path, error)),
    };
    Ok(match observed {
        Some(hash) if entry.before_hash.as_deref() == Some(hash.as_str()) => EntryState::Before,
        Some(hash) if hash == entry.after_hash => EntryState::After,
        None if entry.before_hash.is_none() => EntryState::Before,
        observed => EntryState::Diverged { observed },
    })
}

/// Reads one journal and classifies every entry it records.
///
/// # Errors
/// Any error from [`read_journal`] or [`assess_entry`].
pub fn assess_journal(path: &Path) -> Result<JournalAssessment, RecoveryError> {
    let journal = read_journal(path)?;
    let mut entries = Vec::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        entries.push(EntryAssessment {
            entry: entry.clone(),
            state: assess_entry(entry)?,
        });
    }
    Ok(JournalAssessment {
        path: path.to_path_buf(),
        journal,
        entries,
    })
}

fn remove_if_present(path: &Path, removed: &mut Vec<PathBuf>) -> Result<(), RecoveryError> {
    match fs::remove_file(path) {
        Ok(()) => {
            removed.push(path.to_path_buf());
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, error)),
    }
}

/// Removes a journal and the temp files it names, and never a recorded destination.
///
/// Recorded temp paths are re-validated with [`is_temp_file`] before deletion; anything else goes
/// to [`DiscardOutcome::skipped`] so a corrupt journal cannot turn discard into a delete of user
/// content. The original bytes are left exactly as they are.
///
/// # Errors
/// [`RecoveryError::Io`] when a removable path exists but cannot be deleted.
pub fn discard_pending(
    journal_path: &Path,
    journal: &PendingJournal,
) -> Result<DiscardOutcome, RecoveryError> {
    let mut outcome = DiscardOutcome::default();
    remove_if_present(journal_path, &mut outcome.removed)?;
    for temp in &journal.temp_files {
        let is_debris = temp
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_temp_file);
        if !is_debris {
            outcome.skipped.push(temp.clone());
            continue;
        }
        remove_if_present(temp, &mut outcome.removed)?;
    }
    Ok(outcome)
}

/// Removes journals older than `max_age`, plus the temp files they name.
///
/// Used when the user never answered the Recovery sheet: after the retention window the debris is
/// removed without a decision. A journal that cannot be decoded is removed on its own.
///
/// # Errors
/// [`RecoveryError::Io`] when a path exists but cannot be deleted.
pub fn purge_expired_journals(
    dir: &Path,
    now: OffsetDateTime,
    max_age: Duration,
) -> Result<Vec<PathBuf>, RecoveryError> {
    let mut removed = Vec::new();
    for path in scan_journals(dir)? {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now - OffsetDateTime::from(modified) <= max_age {
            continue;
        }
        match read_journal(&path) {
            Ok(journal) => {
                let outcome = discard_pending(&path, &journal)?;
                removed.extend(outcome.removed);
            }
            Err(_) => remove_if_present(&path, &mut removed)?,
        }
    }
    Ok(removed)
}

/// Puts the before bytes of one file back through [`crate::apply`].
///
/// The bytes are verified against [`RestoreRequest::before_hash`] first, so a truncated or wrong
/// backup never reaches the destination. The write is an ordinary guarded `Replace`: the current
/// bytes are copied into [`RestoreRequest::operation_folder`] (created when absent) and the
/// destination is re-checked against [`RestoreRequest::expected_current_hash`] immediately before
/// the rename. A caller that recorded [`EntryState::After`] has the expected hash; a diverged
/// destination is therefore reported as [`FsError::ConcurrentModification`] rather than lost.
///
/// # Errors
/// [`RecoveryError::BackupMismatch`] when the offered bytes are not the recorded ones, or any
/// [`crate::FsError`] from the write.
pub fn restore_file(request: &RestoreRequest) -> Result<WriteOutcome, RecoveryError> {
    let actual = sha256_hex(&request.before_bytes);
    if actual != request.before_hash {
        return Err(RecoveryError::BackupMismatch {
            path: request.path.clone(),
            expected: request.before_hash.clone(),
            actual,
        });
    }
    fs::create_dir_all(&request.operation_folder)
        .map_err(|error| io_error(&request.operation_folder, error))?;
    let write = WriteRequest {
        path: request.path.clone(),
        new_bytes: request.before_bytes.clone(),
        expected: ExpectedState::Hash(request.expected_current_hash.clone()),
        backup: BackupPolicy::OperationFolder(request.operation_folder.clone()),
        follow_symlink: false,
        mode: WriteMode::Replace,
        allow_empty_dir: false,
        backup_meta: request.backup_meta.clone(),
        guard: None,
    };
    apply(&write).map_err(RecoveryError::from)
}
