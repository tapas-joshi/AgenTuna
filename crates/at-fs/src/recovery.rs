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
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::MAX_FILE_BYTES;
use crate::atomic::{BackupPolicy, WriteMode, WriteOutcome, WriteRequest, apply};
use crate::backup::BackupMeta;
use crate::concurrency::ExpectedState;
use crate::error::FsError;
use crate::hash::sha256_hex;

use at_model::{ExpectedState as ModelExpectedState, FileOp};

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
pub const JOURNAL_VERSION: u32 = 2;

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
    /// Format version; new journals use [`JOURNAL_VERSION`], and v1 remains readable.
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
    /// Complete reviewed operation set (absent in pre-v2 recovery journals).
    #[serde(default)]
    pub change_set: Option<at_model::ChangeSet>,
    /// Indices durably marked complete after each file operation.
    #[serde(default)]
    pub completed_ops: Vec<usize>,
    /// Whether every op completed and the caller committed the operation.
    #[serde(default)]
    pub committed: bool,
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
    /// A journal's on-disk version is not readable by this implementation.
    #[error("{}: journal version {version} is unsupported", path.display())]
    JournalVersionUnsupported {
        /// Journal path.
        path: PathBuf,
        /// Unsupported wire version.
        version: u32,
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
    /// A caller-owned data or operation-history callback failed.
    #[error("recovery callback failed for operation {operation_id}: {message}")]
    Callback {
        /// Operation the callback was handling.
        operation_id: String,
        /// Callback failure detail.
        message: String,
    },
}

/// Caller-owned data and operation-history seams used by startup journal recovery.
///
/// `before_bytes` reads the operation's saved original bytes when they are needed to restore a
/// completed write or delete. `finish` updates the caller-owned operation record; implementations
/// should make repeated calls for the same operation id idempotent.
pub trait RecoveryCallbacks {
    /// Returns the original bytes for a completed operation, or `None` when the original path
    /// was absent. The caller should read its backup store, not infer bytes from the journal.
    fn before_bytes(
        &mut self,
        journal: &PendingJournal,
        op_index: usize,
        entry: &JournalEntry,
    ) -> Result<Option<Vec<u8>>, String>;

    /// Records a terminal operation state. `manual_review` means recovery deliberately left the
    /// journal and destinations untouched because it could not safely decide what to restore.
    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String>;
}

/// A journal left in place for a human because startup recovery could not safely resolve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReviewJournal {
    /// Journal file that remains available for assessment or an explicit user decision.
    pub journal_path: PathBuf,
    /// Operation id recorded in the journal.
    pub operation_id: String,
    /// Safe, content-free reason recovery stopped.
    pub reason: String,
}

/// Outcome of one startup recovery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Uncommitted journals that were fully rolled back and removed.
    pub rolled_back: Vec<PathBuf>,
    /// Committed journals cleaned up without changing their destination files.
    pub committed_cleaned: Vec<PathBuf>,
    /// Journals intentionally retained for manual assessment.
    pub manual_review: Vec<ManualReviewJournal>,
    /// Legacy v1 journals retained because they lack v2 progress data.
    pub legacy: Vec<PathBuf>,
}

/// Recovers pending v2 ChangeSet journals in sorted order.
///
/// Each operation is preflighted before any destination is changed. Completed operations are
/// reversed in journal progress order; a journal is removed only after all its completed
/// operations have been restored and the caller-owned operation record has been updated.
/// Committed v2 journals are only cleaned up. V1 journals remain readable and are retained for
/// assessment because they do not record a ChangeSet or completed-operation progress.
///
/// The application startup call site is owned by TF-027; this crate provides the recovery
/// primitive and does not initialize or update SQLite itself.
///
/// # Errors
/// Returns [`RecoveryError`] when a journal cannot be read, a filesystem action fails, or a
/// caller callback fails. A divergence or missing original backup is returned in
/// [`RecoveryReport::manual_review`] instead and leaves that journal untouched.
pub fn recover_journals(
    journal_dir: &Path,
    store_root: &Path,
    callbacks: &mut dyn RecoveryCallbacks,
) -> Result<RecoveryReport, RecoveryError> {
    let mut report = RecoveryReport::default();
    for path in scan_journals(journal_dir)? {
        let journal = read_journal(&path)?;
        if journal.journal_version == 1 {
            report.legacy.push(path);
            continue;
        }
        if journal.committed {
            callbacks
                .finish(&journal.operation_id, true, false)
                .map_err(|message| callback_error(&journal, message))?;
            cleanup_recovered_journal(&path, &journal, store_root)?;
            report.committed_cleaned.push(path);
            continue;
        }

        let Some(change_set) = journal.change_set.as_ref() else {
            retain_for_manual_review(
                &path,
                &journal,
                "v2 journal has no complete ChangeSet",
                callbacks,
                &mut report,
            )?;
            continue;
        };
        if let Err(error) = change_set.validate() {
            retain_for_manual_review(
                &path,
                &journal,
                &format!("journal ChangeSet is invalid: {error}"),
                callbacks,
                &mut report,
            )?;
            continue;
        }
        if let Some(reason) = validate_progress(&journal, store_root) {
            retain_for_manual_review(&path, &journal, &reason, callbacks, &mut report)?;
            continue;
        }

        let mut plan = Vec::with_capacity(change_set.ops.len());
        let mut divergence = None;
        for index in 0..change_set.ops.len() {
            let Some(op) = change_set.ops.get(index) else {
                divergence = Some(format!("completed operation index {index} is out of range"));
                break;
            };
            let Some(entry) = journal.entries.get(index) else {
                divergence = Some(format!("journal entry for operation {index} is missing"));
                break;
            };
            let state = classify_recovery_op(op, entry, store_root)?;
            if matches!(state, RecoveryState::Diverged) {
                divergence = Some(format!(
                    "destination state diverged for {}",
                    entry.path.display()
                ));
                break;
            }
            plan.push(PlannedRecovery {
                index,
                state,
                before_bytes: None,
            });
        }
        if let Some(reason) = divergence {
            retain_for_manual_review(&path, &journal, &reason, callbacks, &mut report)?;
            continue;
        }

        // Resolve every required backup before touching any destination. A missing or invalid
        // backup leaves the whole journal intact rather than rolling back only a suffix.
        for recovery in &mut plan {
            if recovery.state != RecoveryState::After {
                continue;
            }
            let op = &change_set.ops[recovery.index];
            let entry = &journal.entries[recovery.index];
            if !needs_before_bytes(op, entry) {
                continue;
            }
            let bytes = callbacks
                .before_bytes(&journal, recovery.index, entry)
                .map_err(|message| callback_error(&journal, message))?;
            let Some(bytes) = bytes else {
                divergence = Some(format!(
                    "original bytes are unavailable for {}",
                    entry.path.display()
                ));
                break;
            };
            let expected = entry.before_hash.as_deref().unwrap_or_default();
            let actual = sha256_hex(&bytes);
            if actual != expected {
                divergence = Some(format!(
                    "saved original bytes do not match the recorded hash for {}",
                    entry.path.display()
                ));
                break;
            }
            recovery.before_bytes = Some(bytes);
        }
        if let Some(reason) = divergence {
            retain_for_manual_review(&path, &journal, &reason, callbacks, &mut report)?;
            continue;
        }

        for recovery in plan.iter().rev() {
            if recovery.state == RecoveryState::After {
                rollback_recovery_op(
                    &change_set.ops[recovery.index],
                    &journal.entries[recovery.index],
                    recovery.before_bytes.as_deref(),
                    store_root,
                )?;
            }
        }
        callbacks
            .finish(&journal.operation_id, false, false)
            .map_err(|message| callback_error(&journal, message))?;
        cleanup_recovered_journal(&path, &journal, store_root)?;
        report.rolled_back.push(path);
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryState {
    Before,
    After,
    Diverged,
}

struct PlannedRecovery {
    index: usize,
    state: RecoveryState,
    before_bytes: Option<Vec<u8>>,
}

enum ObservedNode {
    Missing,
    File(String),
    Directory { empty: bool },
    Symlink,
    Other,
}

fn validate_progress(journal: &PendingJournal, store_root: &Path) -> Option<String> {
    let Some(change_set) = journal.change_set.as_ref() else {
        return Some("v2 journal has no complete ChangeSet".to_owned());
    };
    if journal.entries.len() != change_set.ops.len() {
        return Some("journal entry count does not match the ChangeSet".to_owned());
    }
    let mut previous = None;
    for &index in &journal.completed_ops {
        if index >= change_set.ops.len() {
            return Some(format!("completed operation index {index} is out of range"));
        }
        if previous.is_some_and(|value| index <= value) {
            return Some("completed operation progress is not strictly increasing".to_owned());
        }
        previous = Some(index);
        let Some(op) = change_set.ops.get(index) else {
            return Some(format!("completed operation index {index} is out of range"));
        };
        let Some(entry) = journal.entries.get(index) else {
            return Some(format!("journal entry for operation {index} is missing"));
        };
        let Some(destination) = op.destination() else {
            return Some(format!("operation {index} has no recovery destination"));
        };
        if !is_safe_relative_path(destination) || store_root.join(destination) != entry.path {
            return Some(format!(
                "journal destination does not match operation {index}: {}",
                entry.path.display()
            ));
        }
        if let FileOp::Move { from, .. } = op {
            let Some(source) = resolve_store_path(store_root, from) else {
                return Some(format!(
                    "move source for operation {index} leaves the store root"
                ));
            };
            if !is_within_root(store_root, &source) {
                return Some(format!(
                    "move source for operation {index} leaves the store root"
                ));
            }
        }
    }
    None
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn resolve_store_path(root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return is_within_root(root, path).then(|| path.to_path_buf());
    }
    is_safe_relative_path(path).then(|| root.join(path))
}

fn is_within_root(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(is_safe_relative_path)
}

fn observe_node(path: &Path) -> Result<ObservedNode, RecoveryError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ObservedNode::Missing),
        Err(error) => return Err(io_error(path, error)),
    };
    if metadata.file_type().is_symlink() {
        return Ok(ObservedNode::Symlink);
    }
    if metadata.is_file() {
        let size = metadata.len();
        if size > MAX_FILE_BYTES {
            return Err(RecoveryError::TooLarge {
                path: path.to_path_buf(),
                size,
                limit: MAX_FILE_BYTES,
            });
        }
        let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
        return Ok(ObservedNode::File(sha256_hex(&bytes)));
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path).map_err(|error| io_error(path, error))?;
        return Ok(ObservedNode::Directory {
            empty: entries
                .next()
                .transpose()
                .map_err(|error| io_error(path, error))?
                .is_none(),
        });
    }
    Ok(ObservedNode::Other)
}

fn classify_recovery_op(
    op: &FileOp,
    entry: &JournalEntry,
    store_root: &Path,
) -> Result<RecoveryState, RecoveryError> {
    let node = observe_node(&entry.path)?;
    let before = entry.before_hash.as_deref();
    let result = match op {
        FileOp::Write {
            follow_symlink: false,
            ..
        } => match node {
            ObservedNode::File(hash) if before == Some(hash.as_str()) => RecoveryState::Before,
            ObservedNode::File(hash) if hash == entry.after_hash => RecoveryState::After,
            _ => RecoveryState::Diverged,
        },
        FileOp::Write {
            follow_symlink: true,
            ..
        } => RecoveryState::Diverged,
        FileOp::Delete { .. } => match node {
            ObservedNode::Missing if before.is_some() => RecoveryState::After,
            ObservedNode::File(hash) if before == Some(hash.as_str()) => RecoveryState::Before,
            _ => RecoveryState::Diverged,
        },
        FileOp::Copy { .. } => match node {
            ObservedNode::Missing if before.is_none() => RecoveryState::Before,
            ObservedNode::File(hash) if hash == entry.after_hash => RecoveryState::After,
            _ => RecoveryState::Diverged,
        },
        FileOp::Move { from, expected, .. } => {
            let Some(source) = resolve_store_path(store_root, from) else {
                return Ok(RecoveryState::Diverged);
            };
            let source = observe_node(&source)?;
            let expected_hash = expected_content_hash(expected);
            match (node, source) {
                (ObservedNode::Missing, ObservedNode::File(hash))
                    if before.is_none() && expected_hash == Some(hash.as_str()) =>
                {
                    RecoveryState::Before
                }
                (ObservedNode::File(hash), ObservedNode::Missing) if hash == entry.after_hash => {
                    RecoveryState::After
                }
                _ => RecoveryState::Diverged,
            }
        }
        FileOp::Mkdir { .. } => match node {
            ObservedNode::Missing if before.is_none() => RecoveryState::Before,
            ObservedNode::Directory { empty: true } => RecoveryState::After,
            _ => RecoveryState::Diverged,
        },
        FileOp::Symlink { target, mode, .. } => {
            if matches!(node, ObservedNode::Missing) && before.is_none() {
                RecoveryState::Before
            } else if matches!(mode, at_model::DeployMode::Link)
                && matches!(node, ObservedNode::Symlink)
            {
                let Some(source) = resolve_model_source(store_root, target) else {
                    return Ok(RecoveryState::Diverged);
                };
                match (fs::canonicalize(&entry.path), fs::canonicalize(source)) {
                    (Ok(linked), Ok(source)) if linked == source => RecoveryState::After,
                    _ => RecoveryState::Diverged,
                }
            } else {
                RecoveryState::Diverged
            }
        }
        _ => RecoveryState::Diverged,
    };
    Ok(result)
}

fn expected_content_hash(expected: &ModelExpectedState) -> Option<&str> {
    match expected {
        ModelExpectedState::Hash { hash } | ModelExpectedState::HashAndMtime { hash, .. } => {
            Some(hash.as_str())
        }
        ModelExpectedState::Absent => None,
        _ => None,
    }
}

fn resolve_model_source(root: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else if is_safe_relative_path(path) {
        Some(root.join(path))
    } else {
        None
    }
}

fn needs_before_bytes(op: &FileOp, entry: &JournalEntry) -> bool {
    entry.before_hash.is_some() && matches!(op, FileOp::Write { .. } | FileOp::Delete { .. })
}

fn rollback_recovery_op(
    op: &FileOp,
    entry: &JournalEntry,
    before_bytes: Option<&[u8]>,
    store_root: &Path,
) -> Result<(), RecoveryError> {
    match op {
        FileOp::Write { .. } if entry.before_hash.is_none() => {
            fs::remove_file(&entry.path).map_err(|error| io_error(&entry.path, error))
        }
        FileOp::Write { .. } => {
            let bytes = before_bytes.ok_or_else(|| RecoveryError::MalformedJournal {
                path: entry.path.clone(),
                reason: "recovery backup bytes were not loaded".to_owned(),
            })?;
            let write = WriteRequest {
                path: entry.path.clone(),
                new_bytes: bytes.to_vec(),
                expected: ExpectedState::Hash(entry.after_hash.clone()),
                backup: BackupPolicy::None,
                follow_symlink: false,
                mode: WriteMode::Replace,
                allow_empty_dir: false,
                backup_meta: BackupMeta::default(),
                guard: None,
            };
            apply(&write).map(|_| ()).map_err(RecoveryError::from)
        }
        FileOp::Delete { .. } => {
            let bytes = before_bytes.ok_or_else(|| RecoveryError::MalformedJournal {
                path: entry.path.clone(),
                reason: "recovery backup bytes were not loaded".to_owned(),
            })?;
            let write = WriteRequest {
                path: entry.path.clone(),
                new_bytes: bytes.to_vec(),
                expected: ExpectedState::Absent,
                backup: BackupPolicy::None,
                follow_symlink: false,
                mode: WriteMode::Create,
                allow_empty_dir: false,
                backup_meta: BackupMeta::default(),
                guard: None,
            };
            apply(&write).map(|_| ()).map_err(RecoveryError::from)
        }
        FileOp::Copy { .. } | FileOp::Symlink { .. } => {
            fs::remove_file(&entry.path).map_err(|error| io_error(&entry.path, error))
        }
        FileOp::Mkdir { .. } => {
            fs::remove_dir(&entry.path).map_err(|error| io_error(&entry.path, error))
        }
        FileOp::Move { from, .. } => {
            let source = resolve_store_path(store_root, from).ok_or_else(|| {
                RecoveryError::MalformedJournal {
                    path: entry.path.clone(),
                    reason: "move source leaves the store root".to_owned(),
                }
            })?;
            if !matches!(observe_node(&source)?, ObservedNode::Missing) {
                return Err(io_error(
                    &source,
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "move source changed during recovery",
                    ),
                ));
            }
            fs::rename(&entry.path, &source).map_err(|error| io_error(&source, error))
        }
        _ => Err(RecoveryError::MalformedJournal {
            path: entry.path.clone(),
            reason: "completed operation kind cannot be safely recovered".to_owned(),
        }),
    }
}

fn cleanup_recovered_journal(
    path: &Path,
    journal: &PendingJournal,
    store_root: &Path,
) -> Result<(), RecoveryError> {
    let mut safe_journal = journal.clone();
    safe_journal
        .temp_files
        .retain(|temp| is_within_root(store_root, temp));
    discard_pending(path, &safe_journal)?;
    Ok(())
}

fn retain_for_manual_review(
    path: &Path,
    journal: &PendingJournal,
    reason: &str,
    callbacks: &mut dyn RecoveryCallbacks,
    report: &mut RecoveryReport,
) -> Result<(), RecoveryError> {
    callbacks
        .finish(&journal.operation_id, false, true)
        .map_err(|message| callback_error(journal, message))?;
    report.manual_review.push(ManualReviewJournal {
        journal_path: path.to_path_buf(),
        operation_id: journal.operation_id.clone(),
        reason: reason.to_owned(),
    });
    Ok(())
}

fn callback_error(journal: &PendingJournal, message: String) -> RecoveryError {
    RecoveryError::Callback {
        operation_id: journal.operation_id.clone(),
        message,
    }
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
    if !matches!(journal.journal_version, 1 | JOURNAL_VERSION) {
        return Err(RecoveryError::JournalVersionUnsupported {
            path: path.to_path_buf(),
            version: journal.journal_version,
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
#[cfg(test)]
mod startup_recovery_tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use at_model::{ChangeSet, ContentHash, ExpectedState as ModelExpectedState, FileOp};
    use time::OffsetDateTime;

    use super::{
        JOURNAL_VERSION, JournalEntry, PendingJournal, RecoveryCallbacks, read_journal,
        recover_journals, write_journal,
    };

    #[derive(Default)]
    struct Callbacks {
        before: BTreeMap<usize, Vec<u8>>,
        finished: Vec<(String, bool, bool)>,
    }

    impl RecoveryCallbacks for Callbacks {
        fn before_bytes(
            &mut self,
            _journal: &PendingJournal,
            index: usize,
            _entry: &JournalEntry,
        ) -> Result<Option<Vec<u8>>, String> {
            Ok(self.before.get(&index).cloned())
        }

        fn finish(
            &mut self,
            operation_id: &str,
            succeeded: bool,
            manual_review: bool,
        ) -> Result<(), String> {
            self.finished
                .push((operation_id.to_owned(), succeeded, manual_review));
            Ok(())
        }
    }

    fn write_op(path: &Path, before: &[u8], after: &[u8]) -> FileOp {
        FileOp::Write {
            path: path.to_path_buf(),
            bytes: after.to_vec(),
            expected: ModelExpectedState::Hash {
                hash: ContentHash::parse(&crate::sha256_hex(before)).expect("valid hash"),
            },
            follow_symlink: false,
            backup: true,
        }
    }

    fn write_journal_for(
        root: &Path,
        journal_dir: &Path,
        id: &str,
        files: &[(&str, &[u8], &[u8])],
        completed_ops: Vec<usize>,
    ) -> PendingJournal {
        let operations = files
            .iter()
            .map(|(name, before, after)| write_op(Path::new(name), before, after))
            .collect();
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: id.to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: files
                .iter()
                .map(|(name, before, after)| JournalEntry {
                    path: root.join(name),
                    before_hash: Some(crate::sha256_hex(before)),
                    after_hash: crate::sha256_hex(after),
                })
                .collect(),
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: format!("set-{id}"),
                preview_id: format!("preview-{id}"),
                operation_kind: at_model::OperationKind::parse("edit_skill")
                    .expect("known operation kind"),
                scope: None,
                project_id: None,
                summary: "recovery test".to_owned(),
                ops: operations,
                created_at_unix_seconds: 0,
            }),
            completed_ops,
            committed: false,
        };
        write_journal(journal_dir, &journal).expect("write journal");
        journal
    }

    #[test]
    fn recovery_rolls_back_completed_and_landed_unprogressed_ops_idempotently() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = sandbox.path().join("journal");
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join("done.txt"), b"after one").expect("completed target");
        fs::write(root.join("pending.txt"), b"after two").expect("uncompleted target");
        let mut journal = write_journal_for(
            &root,
            &journal_dir,
            "op-progress",
            &[
                ("done.txt", b"before one", b"after one"),
                ("pending.txt", b"before two", b"after two"),
            ],
            vec![0],
        );
        journal.entries[1].before_hash = Some(crate::sha256_hex(b"before two"));
        let journal_path = write_journal(&journal_dir, &journal).expect("write progress");
        let mut callbacks = Callbacks::default();
        callbacks.before.insert(0, b"before one".to_vec());
        callbacks.before.insert(1, b"before two".to_vec());

        let report = recover_journals(&journal_dir, &root, &mut callbacks).expect("recover");
        assert_eq!(report.rolled_back, vec![journal_path.clone()]);
        assert!(report.manual_review.is_empty());
        assert_eq!(
            fs::read(root.join("done.txt")).expect("read restored"),
            b"before one"
        );
        assert_eq!(
            fs::read(root.join("pending.txt")).expect("read restored"),
            b"before two"
        );
        assert!(!journal_path.exists());

        let repeated = recover_journals(&journal_dir, &root, &mut callbacks).expect("repeat");
        assert!(repeated.rolled_back.is_empty());
        assert_eq!(
            fs::read(root.join("done.txt")).expect("read again"),
            b"before one"
        );
        assert_eq!(
            fs::read(root.join("pending.txt")).expect("read again"),
            b"before two"
        );
    }

    #[test]
    fn committed_journal_is_cleaned_without_rolling_back_files() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = sandbox.path().join("journal");
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join("done.txt"), b"after").expect("committed target");
        let mut journal = write_journal_for(
            &root,
            &journal_dir,
            "op-committed",
            &[("done.txt", b"before", b"after")],
            vec![0],
        );
        journal.committed = true;
        let path = write_journal(&journal_dir, &journal).expect("write committed journal");

        let report =
            recover_journals(&journal_dir, &root, &mut Callbacks::default()).expect("recover");
        assert_eq!(report.committed_cleaned, vec![path.clone()]);
        assert!(report.rolled_back.is_empty());
        assert_eq!(
            fs::read(root.join("done.txt")).expect("read target"),
            b"after"
        );
        assert!(!path.exists());
    }

    #[test]
    fn divergence_preserves_all_targets_and_the_journal_for_review() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = sandbox.path().join("journal");
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join("first.txt"), b"after one").expect("first target");
        fs::write(root.join("second.txt"), b"external edit").expect("diverged target");
        write_journal_for(
            &root,
            &journal_dir,
            "op-diverged",
            &[
                ("first.txt", b"before one", b"after one"),
                ("second.txt", b"before two", b"after two"),
            ],
            vec![0, 1],
        );
        let path = journal_dir.join("op-diverged.journal");
        let mut callbacks = Callbacks::default();
        callbacks.before.insert(0, b"before one".to_vec());
        callbacks.before.insert(1, b"before two".to_vec());

        let report = recover_journals(&journal_dir, &root, &mut callbacks).expect("recover");
        assert_eq!(report.manual_review.len(), 1);
        assert_eq!(report.manual_review[0].journal_path, path);
        assert!(path.exists());
        assert_eq!(
            fs::read(root.join("first.txt")).expect("first unchanged"),
            b"after one"
        );
        assert_eq!(
            fs::read(root.join("second.txt")).expect("external edit unchanged"),
            b"external edit"
        );
    }

    #[test]
    fn legacy_v1_journal_remains_readable_and_is_not_recovered_automatically() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let journal_dir = sandbox.path().join("journal");
        let mut journal = PendingJournal {
            journal_version: 1,
            operation_id: "op-legacy".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: Vec::new(),
            temp_files: Vec::new(),
            change_set: None,
            completed_ops: Vec::new(),
            committed: false,
        };
        journal.journal_version = 1;
        let path = write_journal(&journal_dir, &journal).expect("write legacy journal");

        let report = recover_journals(&journal_dir, sandbox.path(), &mut Callbacks::default())
            .expect("recover");
        assert_eq!(report.legacy, vec![path.clone()]);
        assert_eq!(read_journal(&path).expect("read legacy"), journal);
        assert!(path.exists());
    }
}
