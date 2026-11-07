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

/// Largest journal file [`read_journal`] parses.
///
/// A journal holds paths and digests for one change set; anything larger is not a journal this
/// app wrote. The bound keeps a forged or corrupt file from turning startup into a large parse.
pub const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// Most entries [`read_journal`] accepts from one journal.
///
/// The same rationale as [`MAX_JOURNAL_BYTES`]: a startup scan must stay bounded whatever the
/// journal directory contains.
pub const MAX_JOURNAL_ENTRIES: usize = 4096;

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
    /// Parent directories that were absent before the journaled apply began.
    #[serde(default)]
    pub created_dirs: Vec<PathBuf>,
    /// Whether every op completed and the caller committed the operation.
    #[serde(default)]
    pub committed: bool,
    /// Roots the operation's writer recorded (additive optional field, backward compatible).
    ///
    /// Untrusted input: a journal can arrive from a cloned repository, so recovery never
    /// authorizes anything from this list. [`RecoveryPolicy`] carries the roots the caller
    /// itself configured.
    #[serde(default)]
    pub authorized_roots: Option<Vec<PathBuf>>,
    /// Operation backup root the apply used for delete backups and blobs (additive optional
    /// field).
    ///
    /// Equally untrusted: recovery only uses it when it names a directory the caller configured
    /// in [`RecoveryPolicy`] and otherwise parks the journal for manual review.
    #[serde(default)]
    pub backup_root: Option<PathBuf>,
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

    /// Returns the saved node that holds the original of a completed
    /// [`at_model::FileOp::Delete`].
    ///
    /// A delete is undone by renaming the saved node back, so recovery needs its location, not
    /// its bytes. The default returns `None`, which sends recovery to its own scan of
    /// `<store>/backups` for the operation's `<operation-id>-<operation-index>.deleted` entry;
    /// a caller that catalogues backups elsewhere should answer from that catalogue instead.
    fn delete_backup(
        &mut self,
        _journal: &PendingJournal,
        _op_index: usize,
        _entry: &JournalEntry,
    ) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }

    /// Records a terminal operation state. `manual_review` means recovery deliberately left the
    /// journal and destinations untouched because it could not safely decide what to restore.
    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String>;

    /// Whether `operation_id` names an operation this app recorded and that was still in flight.
    ///
    /// Asked only for a store built by [`RecoveryPolicy::requiring_recorded_operation`], which is
    /// how a project store — possibly a clone of an untrusted repository, journals and all — is
    /// told apart from debris the app itself wrote. The default answers `false`, so a caller that
    /// cannot answer parks the journal for manual review instead of replaying it.
    fn operation_in_flight(&mut self, _operation_id: &str) -> Result<bool, String> {
        Ok(false)
    }
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

/// What the caller authorizes startup recovery to act on for one store.
///
/// Journals are read back from disk and, for a project store, may have travelled through a Git
/// checkout of a repository the user cloned. A journal is therefore untrusted data: its own
/// `authorized_roots` and `backup_root` prove nothing, and only the roots and backup directories
/// the caller names here authorize anything. Anything outside them — and every journal of a
/// store built with [`Self::requiring_recorded_operation`] that does not name an operation the
/// caller recorded as in flight — is parked for manual review, typed and logged, never executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPolicy {
    store_root: PathBuf,
    authorized_roots: Vec<PathBuf>,
    backup_roots: Vec<PathBuf>,
    require_recorded_operation: bool,
}

impl RecoveryPolicy {
    /// Builds the policy for the store that owns the journal directory.
    ///
    /// Roots are canonicalized; one that is not an absolute existing directory is dropped. The
    /// store root is always authorized, so a journal with store-relative destinations stays
    /// recoverable without the caller repeating it.
    #[must_use]
    pub fn new(
        store_root: impl Into<PathBuf>,
        authorized_roots: impl IntoIterator<Item = PathBuf>,
        backup_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let store_root = store_root.into();
        let mut roots = Vec::new();
        for root in std::iter::once(store_root.clone()).chain(authorized_roots) {
            if let Some(root) = canonical_root(&root)
                && !roots.contains(&root)
            {
                roots.push(root);
            }
        }
        roots.sort();
        let mut backups = Vec::new();
        for root in backup_roots {
            if let Some(root) = canonical_root(&root)
                && !backups.contains(&root)
            {
                backups.push(root);
            }
        }
        backups.sort();
        Self {
            store_root,
            authorized_roots: roots,
            backup_roots: backups,
            require_recorded_operation: false,
        }
    }

    /// Requires every journal of this store to name an operation the caller recorded and that
    /// was still in flight.
    ///
    /// A project store can be committed to Git and cloned from an untrusted source, so a journal
    /// there proves nothing by itself; a user or Global store is written only by this app. This
    /// is the switch between the two, and it is also what keeps a forged operation id out of the
    /// caller's operation history.
    #[must_use]
    pub fn requiring_recorded_operation(mut self) -> Self {
        self.require_recorded_operation = true;
        self
    }

    /// The store root these journals belong to, as the caller named it.
    #[must_use]
    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// Whether the caller authorized `path`, after resolving every symbolic link in its existing
    /// prefix and refusing `..` segments.
    #[must_use]
    pub fn authorizes(&self, path: &Path) -> bool {
        let Some(candidate) = canonical_candidate(path) else {
            return false;
        };
        self.authorized_roots
            .iter()
            .any(|root| strictly_within(root, &candidate))
    }

    /// Whether the caller configured `path` as one of its backup directories.
    fn authorizes_backup(&self, path: &Path) -> bool {
        let Some(candidate) = canonical_candidate(path) else {
            return false;
        };
        self.backup_roots
            .iter()
            .any(|root| root == &candidate || strictly_within(root, &candidate))
    }
}

/// `root` when it is an absolute, canonicalizable existing directory.
fn canonical_root(root: &Path) -> Option<PathBuf> {
    if !root.is_absolute() || root == Path::new("/") {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    root.is_dir().then_some(root)
}

/// `path` with every existing ancestor canonicalized and the missing tail appended component by
/// component, or `None` when the path is relative, walks upward or cannot be resolved.
///
/// Resolving the deepest existing ancestor is what makes a symlinked parent part of the check:
/// the destination is validated against the directory the write would actually reach.
fn canonical_candidate(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let mut ancestor = path;
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor.file_name()?.to_os_string();
                missing.push(name);
                ancestor = ancestor.parent()?;
            }
            Err(_) => return None,
        }
    }
    let mut candidate = fs::canonicalize(ancestor).ok()?;
    for component in missing.iter().rev() {
        if component == std::ffi::OsStr::new(".") || component == std::ffi::OsStr::new("..") {
            return None;
        }
        candidate.push(component);
    }
    Some(candidate)
}

/// Whether `path` is a strict descendant of `root`, comparing resolved components.
fn strictly_within(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|rest| {
        !rest.as_os_str().is_empty()
            && rest
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
    })
}

/// Recovers pending v2 ChangeSet journals in sorted order.
///
/// Each operation is preflighted before any destination is changed. Completed operations are
/// reversed in journal progress order; a journal is removed only after all its completed
/// operations have been restored and the caller-owned operation record has been updated.
/// Committed v2 journals are only cleaned up. V1 journals remain readable and are retained for
/// assessment because they do not record a ChangeSet or completed-operation progress.
///
/// Every journal is untrusted input and is validated against `policy` before anything moves: a
/// journal whose destinations, move sources or created directories escape the caller's roots, or
/// whose backup root is not one the caller configured, is parked in
/// [`RecoveryReport::manual_review`] without writing anything. Journals that cannot be read as a
/// journal this app wrote, and journals of a store built with
/// [`RecoveryPolicy::requiring_recorded_operation`] whose operation the caller did not record,
/// are parked the same way.
///
/// The application startup call site is owned by TF-027; this crate provides the recovery
/// primitive and does not initialize or update SQLite itself.
///
/// # Errors
/// Returns [`RecoveryError`] when a journal directory cannot be scanned, a filesystem action
/// fails, or a caller callback fails. A divergence, a missing original backup or an untrusted
/// journal is returned in [`RecoveryReport::manual_review`] instead and leaves that journal
/// untouched.
pub fn recover_journals(
    journal_dir: &Path,
    policy: &RecoveryPolicy,
    callbacks: &mut dyn RecoveryCallbacks,
) -> Result<RecoveryReport, RecoveryError> {
    let store_root = policy.store_root();
    let mut report = RecoveryReport::default();
    for path in scan_journals(journal_dir)? {
        let journal = match read_journal(&path) {
            Ok(journal) => journal,
            Err(
                error @ (RecoveryError::MalformedJournal { .. }
                | RecoveryError::JournalVersionUnsupported { .. }
                | RecoveryError::TooLarge { .. }),
            ) => {
                quarantine_journal(
                    &path,
                    None,
                    &format!("journal cannot be read as one this app wrote: {error}"),
                    &mut report,
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        let expected_name = journal_file_name(&journal.operation_id);
        if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
            quarantine_journal(
                &path,
                Some(&journal),
                "journal file name does not match its operation id",
                &mut report,
            );
            continue;
        }
        if journal.journal_version == 1 {
            report.legacy.push(path);
            continue;
        }
        if journal.entries.len() > MAX_JOURNAL_ENTRIES {
            quarantine_journal(
                &path,
                Some(&journal),
                "journal records more entries than any change set this app wrote",
                &mut report,
            );
            continue;
        }
        if let Some(backup_root) = journal.backup_root.as_deref()
            && !policy.authorizes_backup(backup_root)
        {
            quarantine_journal(
                &path,
                Some(&journal),
                "journal backup root is not a backup directory this app configured",
                &mut report,
            );
            continue;
        }
        if policy.require_recorded_operation {
            let recorded = callbacks
                .operation_in_flight(&journal.operation_id)
                .map_err(|message| callback_error(&journal, message))?;
            if !recorded {
                quarantine_journal(
                    &path,
                    Some(&journal),
                    "journal names an operation this app did not record as in flight",
                    &mut report,
                );
                continue;
            }
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
        if let Some(reason) = validate_progress(&journal, policy) {
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
            let replaced_later = later_completed_op_replaces(&journal, index, entry, policy);
            let state = classify_recovery_op(op, entry, policy, replaced_later)?;
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
                delete_backup: None,
            });
        }
        if let Some(reason) = divergence {
            retain_for_manual_review(&path, &journal, &reason, callbacks, &mut report)?;
            continue;
        }

        for recovery in &mut plan {
            if recovery.state != RecoveryState::After {
                continue;
            }
            let op = &change_set.ops[recovery.index];
            let entry = &journal.entries[recovery.index];
            if matches!(op, FileOp::Delete { .. }) {
                let Some(expected_hash) = entry.before_hash.as_deref() else {
                    divergence = Some(format!(
                        "original hash is unavailable for {}",
                        entry.path.display()
                    ));
                    break;
                };
                let offered = callbacks
                    .delete_backup(&journal, recovery.index, entry)
                    .map_err(|message| callback_error(&journal, message))?;
                let backup = match offered {
                    // The catalogue may name any path; only a backup directory the caller
                    // configured is used, and never a symbolic link standing in for one.
                    Some(path) if policy.authorizes_backup(&path) => {
                        let observed = observe_node(&path)?;
                        let verified = !matches!(observed, ObservedNode::Symlink(_))
                            && observed.hash().is_some_and(|hash| hash == expected_hash);
                        verified.then_some(path)
                    }
                    _ => find_delete_backup(policy, &journal, recovery.index, expected_hash)?,
                };
                let Some(backup) = backup else {
                    divergence = Some(format!(
                        "verified delete backup is unavailable for {}",
                        entry.path.display()
                    ));
                    break;
                };
                recovery.delete_backup = Some(backup);
            }
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
                    recovery.delete_backup.as_deref(),
                    policy,
                )?;
            }
        }
        for directory in &journal.created_dirs {
            // `validate_progress` already refused the journal when a directory escaped the
            // caller's roots; re-check so a bug in that path can never remove anything else.
            if !policy.authorizes(directory) {
                continue;
            }
            match fs::remove_dir(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(RecoveryError::Io {
                        path: directory.clone(),
                        message: error.to_string(),
                    });
                }
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
    /// Verified saved node that a completed `Delete` moved aside.
    delete_backup: Option<PathBuf>,
}

enum ObservedNode {
    Missing,
    File(String),
    Directory {
        empty: bool,
        hash: String,
    },
    /// A symbolic link, carrying the hash of its target text, which is how both the journal and
    /// [`crate::changeset::hash_node`] represent a link.
    Symlink(String),
    Other,
}

impl ObservedNode {
    /// The hash this node contributes to a journal entry, or `None` when it has none.
    fn hash(&self) -> Option<&str> {
        match self {
            Self::File(hash) | Self::Symlink(hash) => Some(hash),
            Self::Directory { hash, .. } => Some(hash),
            Self::Missing | Self::Other => None,
        }
    }
}

/// Whether `node` still holds exactly what the journal recorded before the operation ran.
///
/// A recorded `None` means the path was absent, so only [`ObservedNode::Missing`] matches.
fn holds_before(node: &ObservedNode, before: Option<&str>) -> bool {
    match before {
        None => matches!(node, ObservedNode::Missing),
        Some(before) => node.hash() == Some(before),
    }
}

/// Whether `node` already holds what the operation wrote.
///
/// An empty `after_hash` records that the operation kind has no single-file digest to compare.
fn holds_after(node: &ObservedNode, after: &str) -> bool {
    !after.is_empty() && node.hash() == Some(after)
}

fn validate_progress(journal: &PendingJournal, policy: &RecoveryPolicy) -> Option<String> {
    let Some(change_set) = journal.change_set.as_ref() else {
        return Some("v2 journal has no complete ChangeSet".to_owned());
    };
    if journal.entries.len() != change_set.ops.len() {
        return Some("journal entry count does not match the ChangeSet".to_owned());
    }
    for (index, (op, entry)) in change_set.ops.iter().zip(&journal.entries).enumerate() {
        let Some(destination) = op.destination() else {
            return Some(format!("operation {index} has no recovery destination"));
        };
        let Some(dest) = journal_destination(policy, destination) else {
            return Some(format!(
                "destination for operation {index} is outside authorized roots: {}",
                destination.display()
            ));
        };
        if dest != entry.path {
            return Some(format!(
                "journal destination does not match operation {index}: {}",
                entry.path.display()
            ));
        }
        if let FileOp::Move { from, .. } = op
            && journal_destination(policy, from).is_none()
        {
            return Some(format!(
                "move source for operation {index} is outside authorized roots: {}",
                from.display()
            ));
        }
    }
    for directory in &journal.created_dirs {
        if !policy.authorizes(directory) {
            return Some(format!(
                "journal created directory is outside authorized roots: {}",
                directory.display()
            ));
        }
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
    }
    None
}

/// The absolute path a journal entry must hold for `destination`.
///
/// A store-relative destination resolves against the caller's store root; an absolute one is used
/// as written. Either way the caller's policy must authorize the resolved path — the journal's own
/// `authorized_roots` are never consulted.
fn journal_destination(policy: &RecoveryPolicy, destination: &Path) -> Option<PathBuf> {
    let resolved = if destination.is_absolute() {
        destination.to_path_buf()
    } else if is_safe_relative_path(destination) {
        policy.store_root.join(destination)
    } else {
        return None;
    };
    policy.authorizes(&resolved).then_some(resolved)
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
        let target = fs::read_link(path).map_err(|error| io_error(path, error))?;
        return Ok(ObservedNode::Symlink(sha256_hex(
            target.as_os_str().as_encoded_bytes(),
        )));
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
        let empty = entries
            .next()
            .transpose()
            .map_err(|error| io_error(path, error))?
            .is_none();
        return Ok(ObservedNode::Directory {
            empty,
            hash: tree_hash(path)?,
        });
    }
    Ok(ObservedNode::Other)
}

fn tree_hash(path: &Path) -> Result<String, RecoveryError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| io_error(path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(path, error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut bytes = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        bytes.extend_from_slice(name.as_encoded_bytes());
        bytes.push(0);
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child).map_err(|error| io_error(&child, error))?;
        let digest = if metadata.is_dir() && !metadata.file_type().is_symlink() {
            tree_hash(&child)?
        } else if metadata.file_type().is_symlink() {
            sha256_hex(
                fs::read_link(&child)
                    .map_err(|error| io_error(&child, error))?
                    .as_os_str()
                    .as_encoded_bytes(),
            )
        } else if metadata.is_file() {
            let contents = fs::read(&child).map_err(|error| io_error(&child, error))?;
            sha256_hex(&contents)
        } else {
            return Err(RecoveryError::MalformedJournal {
                path: child,
                reason: "unsupported directory entry".to_owned(),
            });
        };
        bytes.extend_from_slice(digest.as_bytes());
        bytes.push(0xff);
    }
    Ok(sha256_hex(&bytes))
}

/// Whether a completed operation after `index` writes the same destination as `index`.
///
/// A `Delete` followed by a deployment of the same path is the migration shape: the original
/// node is moved to a backup and a link is created in its place. When that later operation
/// completed, whatever sits at the path now belongs to it, so the delete's own effect (the
/// original node is gone) still holds and recovery must undo the later operation first.
fn later_completed_op_replaces(
    journal: &PendingJournal,
    index: usize,
    entry: &JournalEntry,
    policy: &RecoveryPolicy,
) -> bool {
    let Some(change_set) = journal.change_set.as_ref() else {
        return false;
    };
    journal.completed_ops.iter().copied().any(|later| {
        later > index
            && change_set
                .ops
                .get(later)
                .and_then(FileOp::destination)
                .and_then(|destination| journal_destination(policy, destination))
                .as_deref()
                == Some(entry.path.as_path())
    })
}

fn classify_recovery_op(
    op: &FileOp,
    entry: &JournalEntry,
    policy: &RecoveryPolicy,
    replaced_later: bool,
) -> Result<RecoveryState, RecoveryError> {
    let store_root = policy.store_root();
    let node = observe_node(&entry.path)?;
    let before = entry.before_hash.as_deref();
    let after = entry.after_hash.as_str();
    let unchanged = holds_before(&node, before);
    let result = match op {
        FileOp::Write {
            follow_symlink: false,
            ..
        } => {
            if unchanged {
                RecoveryState::Before
            } else if matches!(node, ObservedNode::File(_)) && holds_after(&node, after) {
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        // A write through a symbolic link lands on the resolved target, and the journal stores
        // the target's digest, so recovery resolves the link exactly like the writer did. A
        // dangling or removed link cannot be classified and stays for a human.
        FileOp::Write {
            follow_symlink: true,
            ..
        } => match crate::symlink::resolve(&entry.path, true) {
            Ok((target, _)) => {
                let node = observe_node(&target)?;
                if holds_before(&node, before) {
                    RecoveryState::Before
                } else if matches!(node, ObservedNode::File(_)) && holds_after(&node, after) {
                    RecoveryState::After
                } else {
                    RecoveryState::Diverged
                }
            }
            Err(_) => RecoveryState::Diverged,
        },
        FileOp::Delete { .. } => {
            if unchanged {
                RecoveryState::Before
            } else if before.is_none() {
                RecoveryState::Diverged
            } else if matches!(node, ObservedNode::Missing) || replaced_later {
                // Nothing is there, or a completed later operation re-created the path: either
                // way the original node is gone and its saved copy restores it.
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        FileOp::Copy { source, .. } => {
            if unchanged {
                RecoveryState::Before
            } else if (matches!(node, ObservedNode::Directory { .. })
                && copy_manifest_matches(&entry.path, source, store_root)?)
                || (matches!(node, ObservedNode::File(_)) && holds_after(&node, after))
            {
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        FileOp::CopyTree { .. } => {
            if unchanged {
                RecoveryState::Before
            } else if holds_after(&node, after) {
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        FileOp::Move { from, expected, .. } => {
            let Some(source) = journal_destination(policy, from) else {
                return Ok(RecoveryState::Diverged);
            };
            let source = observe_node(&source)?;
            let expected_hash = expected_content_hash(expected);
            match (&node, &source) {
                (ObservedNode::Missing, observed)
                    if before.is_none() && expected_hash == observed.hash() =>
                {
                    RecoveryState::Before
                }
                (observed, ObservedNode::Missing) if expected_hash == observed.hash() => {
                    RecoveryState::After
                }
                _ => RecoveryState::Diverged,
            }
        }
        FileOp::Mkdir { .. } => {
            if unchanged {
                RecoveryState::Before
            } else if before.is_none()
                && matches!(node, ObservedNode::Directory { empty: true, .. })
            {
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        FileOp::Symlink { target, mode, .. } => {
            if matches!(node, ObservedNode::Missing) || unchanged {
                // Either the operation never ran, or the `Delete` that clears the original path
                // ran and this link was never written. Neither leaves anything to undo here.
                RecoveryState::Before
            } else if matches!(mode, at_model::DeployMode::Link)
                && matches!(node, ObservedNode::Symlink(_))
            {
                let Some(source) = resolve_model_source(store_root, target) else {
                    return Ok(RecoveryState::Diverged);
                };
                match (fs::canonicalize(&entry.path), fs::canonicalize(source)) {
                    (Ok(linked), Ok(source)) if linked == source => RecoveryState::After,
                    _ => RecoveryState::Diverged,
                }
            } else if matches!(node, ObservedNode::Directory { .. })
                && copy_manifest_matches(&entry.path, target, store_root)?
            {
                // `DeployMode::Link` falls back to a manifest-bearing copy, so both modes can
                // leave a directory here.
                RecoveryState::After
            } else {
                RecoveryState::Diverged
            }
        }
        _ => RecoveryState::Diverged,
    };
    Ok(result)
}
/// A directory copy is removable only when its self-identifying deploy manifest proves the
/// journaled source and the entire current tree still match the manifest.
fn copy_manifest_matches(
    destination: &Path,
    source: &Path,
    store_root: &Path,
) -> Result<bool, RecoveryError> {
    use crate::deploy::{COPY_MANIFEST_FILE, CopyManifest};

    let Some(manifest) =
        CopyManifest::read(destination).map_err(|error| RecoveryError::Callback {
            operation_id: "copy".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(false);
    };
    let source_path = resolve_store_path(store_root, source)
        .or_else(|| source.is_absolute().then(|| source.to_path_buf()));
    let Some(source_path) = source_path else {
        return Ok(false);
    };
    let expected_source = fs::canonicalize(&source_path).ok();
    let recorded_source = fs::canonicalize(&manifest.source_path).ok();
    if expected_source.is_none() || expected_source != recorded_source {
        return Ok(false);
    }
    let mut expected = BTreeSet::new();
    expected.insert(COPY_MANIFEST_FILE.to_owned());
    for directory in &manifest.dirs {
        let relative = Path::new(directory);
        if !is_safe_relative_path(relative) {
            return Ok(false);
        }
        let path = destination.join(relative);
        if !matches!(observe_node(&path)?, ObservedNode::Directory { .. }) {
            return Ok(false);
        }
        expected.insert(directory.clone());
    }
    for file in &manifest.files {
        let relative = Path::new(&file.path);
        if !is_safe_relative_path(relative) {
            return Ok(false);
        }
        let path = destination.join(relative);
        let Ok(bytes) = fs::read(&path) else {
            return Ok(false);
        };
        if sha256_hex(&bytes) != file.sha256 || bytes.len() as u64 != file.bytes {
            return Ok(false);
        }
        expected.insert(file.path.clone());
    }
    let mut actual = BTreeSet::new();
    collect_relative_entries(destination, destination, &mut actual)?;
    Ok(actual == expected)
}

fn collect_relative_entries(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeSet<String>,
) -> Result<(), RecoveryError> {
    for entry in fs::read_dir(directory).map_err(|error| io_error(directory, error))? {
        let entry = entry.map_err(|error| io_error(directory, error))?;
        let path = entry.path();
        let relative =
            path.strip_prefix(root)
                .map_err(|error| RecoveryError::MalformedJournal {
                    path: path.clone(),
                    reason: error.to_string(),
                })?;
        let Some(relative) = relative.to_str() else {
            return Err(RecoveryError::MalformedJournal {
                path,
                reason: "copy manifest path is not UTF-8".to_owned(),
            });
        };
        if !is_safe_relative_path(Path::new(relative)) {
            return Err(RecoveryError::MalformedJournal {
                path,
                reason: "unsafe copy manifest path".to_owned(),
            });
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(RecoveryError::MalformedJournal {
                path,
                reason: "copy deployment contains a symlink".to_owned(),
            });
        }
        entries.insert(relative.replace(std::path::MAIN_SEPARATOR, "/"));
        if metadata.is_dir() {
            collect_relative_entries(root, &entry.path(), entries)?;
        } else if !metadata.is_file() {
            return Err(RecoveryError::MalformedJournal {
                path,
                reason: "copy deployment contains a non-file".to_owned(),
            });
        }
    }
    Ok(())
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
    entry.before_hash.is_some() && matches!(op, FileOp::Write { .. })
}

fn find_delete_backup(
    policy: &RecoveryPolicy,
    journal: &PendingJournal,
    operation_index: usize,
    expected_hash: &str,
) -> Result<Option<PathBuf>, RecoveryError> {
    let roots = policy.backup_roots.clone();
    let wanted_name = format!("{}-{operation_index}.deleted", journal.operation_id);
    let legacy_name = format!("{}.deleted", journal.operation_id);
    let mut matches = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let mut pending = vec![(root, 0usize)];
        while let Some((directory, depth)) = pending.pop() {
            if depth > 8 {
                continue;
            }
            for entry in fs::read_dir(&directory).map_err(|error| io_error(&directory, error))? {
                let entry = entry.map_err(|error| io_error(&directory, error))?;
                let path = entry.path();
                let metadata =
                    fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
                if metadata.file_type().is_symlink() {
                    continue;
                }
                let file_name = entry.file_name();
                if file_name == std::ffi::OsStr::new(&wanted_name)
                    || file_name == std::ffi::OsStr::new(&legacy_name)
                {
                    let observed = observe_node(&path)?;
                    if matches!(&observed, ObservedNode::File(hash) if hash == expected_hash)
                        || matches!(&observed, ObservedNode::Directory { hash, .. } if hash == expected_hash)
                    {
                        matches.push(path);
                    }
                } else if metadata.is_dir() {
                    pending.push((path, depth + 1));
                }
            }
        }
    }
    matches.sort();
    matches.dedup();
    Ok((matches.len() == 1).then(|| matches.remove(0)))
}

fn rollback_recovery_op(
    op: &FileOp,
    entry: &JournalEntry,
    before_bytes: Option<&[u8]>,
    delete_backup: Option<&Path>,
    policy: &RecoveryPolicy,
) -> Result<(), RecoveryError> {
    let store_root = policy.store_root();
    match op {
        FileOp::Write { follow_symlink, .. } if entry.before_hash.is_none() => {
            let target = crate::symlink::resolve(&entry.path, *follow_symlink)
                .map(|(target, _)| target)
                .map_err(RecoveryError::from)?;
            fs::remove_file(&target).map_err(|error| io_error(&target, error))
        }
        FileOp::Write { follow_symlink, .. } => {
            let bytes = before_bytes.ok_or_else(|| RecoveryError::MalformedJournal {
                path: entry.path.clone(),
                reason: "recovery backup bytes were not loaded".to_owned(),
            })?;
            // The journal digest describes the resolved target, so restore through the link
            // instead of refusing it and parking the journal.
            let target = crate::symlink::resolve(&entry.path, *follow_symlink)
                .map(|(target, _)| target)
                .map_err(RecoveryError::from)?;
            let write = WriteRequest {
                path: target,
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
            let backup = delete_backup.ok_or_else(|| RecoveryError::MalformedJournal {
                path: entry.path.clone(),
                reason: "verified delete backup was not resolved".to_owned(),
            })?;
            if !matches!(observe_node(&entry.path)?, ObservedNode::Missing) {
                return Err(io_error(
                    &entry.path,
                    io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "delete target changed during recovery",
                    ),
                ));
            }
            fs::rename(backup, &entry.path).map_err(|error| io_error(&entry.path, error))
        }
        FileOp::Copy { source, .. } => {
            let metadata =
                fs::symlink_metadata(&entry.path).map_err(|error| io_error(&entry.path, error))?;
            if metadata.is_dir() {
                #[cfg(any(test, feature = "faults", feature = "testkit"))]
                if let Err(failure) = crate::failpoint::hit(
                    crate::failpoint::FaultPoint::ChangeSetOperation,
                    &entry.path,
                ) {
                    return Err(io_error(&entry.path, failure.into_io()));
                }
                if !copy_manifest_matches(&entry.path, source, store_root)? {
                    return Err(RecoveryError::MalformedJournal {
                        path: entry.path.clone(),
                        reason: "copy deployment changed during recovery".to_owned(),
                    });
                }
                fs::remove_dir_all(&entry.path).map_err(|error| io_error(&entry.path, error))
            } else {
                fs::remove_file(&entry.path).map_err(|error| io_error(&entry.path, error))
            }
        }
        FileOp::CopyTree { .. } => {
            let metadata =
                fs::symlink_metadata(&entry.path).map_err(|error| io_error(&entry.path, error))?;
            if metadata.is_dir() {
                fs::remove_dir_all(&entry.path).map_err(|error| io_error(&entry.path, error))
            } else {
                fs::remove_file(&entry.path).map_err(|error| io_error(&entry.path, error))
            }
        }
        FileOp::Symlink { .. } => {
            let metadata =
                fs::symlink_metadata(&entry.path).map_err(|error| io_error(&entry.path, error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                // `DeployMode::Link` falls back to a manifest-bearing copy directory.
                fs::remove_dir_all(&entry.path).map_err(|error| io_error(&entry.path, error))
            } else {
                fs::remove_file(&entry.path).map_err(|error| io_error(&entry.path, error))
            }
        }
        FileOp::Mkdir { .. } => {
            fs::remove_dir(&entry.path).map_err(|error| io_error(&entry.path, error))
        }
        FileOp::Move { from, .. } => {
            let source = journal_destination(policy, from).ok_or_else(|| {
                RecoveryError::MalformedJournal {
                    path: entry.path.clone(),
                    reason: "move source is not a usable path".to_owned(),
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

/// Parks a journal that is not provably one this app wrote.
///
/// Nothing is touched: no destination, no backup and no operation row. Calling `finish` here
/// would itself be a write keyed by an attacker-chosen operation id, so the reason travels in
/// the report and the log instead.
fn quarantine_journal(
    path: &Path,
    journal: Option<&PendingJournal>,
    reason: &str,
    report: &mut RecoveryReport,
) {
    let operation_id = journal.map_or_else(
        || journal_operation_id(path).unwrap_or_default(),
        |journal| journal.operation_id.clone(),
    );
    tracing::warn!(
        journal = %path.display(),
        operation_id = %operation_id,
        reason,
        "startup recovery parked an untrusted journal for manual review"
    );
    report.manual_review.push(ManualReviewJournal {
        journal_path: path.to_path_buf(),
        operation_id,
        reason: reason.to_owned(),
    });
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

/// Every `*.journal` entry in `dir`, sorted by path. A missing directory has no journals.
///
/// Symbolic links are listed but never followed: [`read_journal`] rejects them, so a store that
/// arrived through Git can neither redirect the scan nor hide a link from the recovery sheet.
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
        if journal_operation_id(&path).is_none() {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                journals.push(path);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&path, error)),
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
    crate::durability::sync_all(&file).map_err(|error| io_error(&path, error))?;
    Ok(path)
}

/// Reads and validates one journal file.
///
/// The file must be a regular file no larger than [`MAX_JOURNAL_BYTES`] with no more than
/// [`MAX_JOURNAL_ENTRIES`] entries, and a symbolic link is never followed: a journal is untrusted
/// input, and a link would let a store that arrived through Git point the read anywhere on disk.
///
/// # Errors
/// [`RecoveryError::Io`] when the file cannot be read, [`RecoveryError::MalformedJournal`] when it
/// is not a regular file or bounded journal, is not JSON, or names more entries than any change
/// set this app writes, and [`RecoveryError::JournalVersionUnsupported`] for an unknown version.
pub fn read_journal(path: &Path) -> Result<PendingJournal, RecoveryError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: "journal is a symbolic link or not a regular file".to_owned(),
        });
    }
    if metadata.len() > MAX_JOURNAL_BYTES {
        return Err(RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: format!(
                "journal is {} bytes, above the {MAX_JOURNAL_BYTES} byte limit",
                metadata.len()
            ),
        });
    }
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: "journal grew past the size limit while it was read".to_owned(),
        });
    }
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
    if journal.entries.len() > MAX_JOURNAL_ENTRIES {
        return Err(RecoveryError::MalformedJournal {
            path: path.to_path_buf(),
            reason: format!(
                "journal records {} entries, above the {MAX_JOURNAL_ENTRIES} entry limit",
                journal.entries.len()
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
#[cfg(test)]
mod startup_recovery_tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use std::sync::Arc;

    use crate::failpoint::{Fault, FaultPoint, TestInjector, with_injector};
    use at_model::{ChangeSet, ContentHash, ExpectedState as ModelExpectedState, FileOp};
    use time::OffsetDateTime;

    use super::{
        JOURNAL_VERSION, JournalEntry, PendingJournal, RecoveryCallbacks, RecoveryPolicy,
        read_journal, recover_journals, write_journal,
    };

    #[derive(Default)]
    struct Callbacks {
        before: BTreeMap<usize, Vec<u8>>,
        finished: Vec<(String, bool, bool)>,
        in_flight: Vec<String>,
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

        fn operation_in_flight(&mut self, operation_id: &str) -> Result<bool, String> {
            Ok(self.in_flight.iter().any(|known| known == operation_id))
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
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: None,
        };
        write_journal(journal_dir, &journal).expect("write journal");
        journal
    }

    /// Policy for a store this app owns: the store root authorized and the store's own backup
    /// directory configured, which is what the startup caller supplies.
    fn policy_for(root: &Path) -> RecoveryPolicy {
        RecoveryPolicy::new(root, [root.to_path_buf()], [root.join("backups")])
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

        let report =
            recover_journals(&journal_dir, &policy_for(&root), &mut callbacks).expect("recover");
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

        let repeated =
            recover_journals(&journal_dir, &policy_for(&root), &mut callbacks).expect("repeat");
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

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recover");
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

        let report =
            recover_journals(&journal_dir, &policy_for(&root), &mut callbacks).expect("recover");
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
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: None,
        };
        journal.journal_version = 1;
        let path = write_journal(&journal_dir, &journal).expect("write legacy journal");

        let report = recover_journals(
            &journal_dir,
            &policy_for(sandbox.path()),
            &mut Callbacks::default(),
        )
        .expect("recover");
        assert_eq!(report.legacy, vec![path.clone()]);
        assert_eq!(read_journal(&path).expect("read legacy"), journal);
        assert!(path.exists());
    }
    #[test]
    fn recovery_removes_verified_directory_copy_after_progress_was_journaled() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = root.join(".state/journal");
        let source = root.join("source");
        let destination = root.join("copy");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("item.txt"), b"copied content").expect("source content");
        crate::deploy::copy_dir(&source, &destination).expect("copy deployment");
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "op-copy".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: vec![JournalEntry {
                path: destination.clone(),
                before_hash: None,
                after_hash: String::new(),
            }],
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-copy".to_owned(),
                preview_id: "preview-copy".to_owned(),
                operation_kind: at_model::OperationKind::parse("deploy")
                    .expect("known operation kind"),
                scope: None,
                project_id: None,
                summary: "copy recovery test".to_owned(),
                ops: vec![FileOp::Copy {
                    source: PathBuf::from("source"),
                    dest: PathBuf::from("copy"),
                    expected: ModelExpectedState::Absent,
                }],
                created_at_unix_seconds: 0,
            }),
            completed_ops: vec![0],
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: None,
        };
        write_journal(&journal_dir, &journal).expect("write in-flight journal");

        let injector = Arc::new(TestInjector::new());
        injector.arm(FaultPoint::ChangeSetOperation, Fault::Fail(libc::EIO));
        let failed = with_injector(injector, || {
            recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
        });
        assert!(failed.is_err());
        assert!(destination.exists(), "failed recovery preserves copy tree");
        assert!(
            journal_dir.join("op-copy.journal").exists(),
            "failed recovery preserves journal for retry"
        );

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recovery retry");

        assert_eq!(report.rolled_back.len(), 1);
        assert!(!destination.exists());
        assert!(source.join("item.txt").exists());
    }

    #[test]
    fn recovery_keeps_directory_copy_with_modified_manifest_content_for_review() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = root.join(".state/journal");
        let source = root.join("source");
        let destination = root.join("copy");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(source.join("item.txt"), b"original").expect("source content");
        crate::deploy::copy_dir(&source, &destination).expect("copy deployment");
        fs::write(destination.join("item.txt"), b"external edit").expect("external edit");
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "op-copy-edited".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: vec![JournalEntry {
                path: destination.clone(),
                before_hash: None,
                after_hash: String::new(),
            }],
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-copy-edited".to_owned(),
                preview_id: "preview-copy-edited".to_owned(),
                operation_kind: at_model::OperationKind::parse("deploy")
                    .expect("known operation kind"),
                scope: None,
                project_id: None,
                summary: "copy recovery test".to_owned(),
                ops: vec![FileOp::Copy {
                    source: PathBuf::from("source"),
                    dest: PathBuf::from("copy"),
                    expected: ModelExpectedState::Absent,
                }],
                created_at_unix_seconds: 0,
            }),
            completed_ops: vec![0],
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: None,
        };
        let journal_path = write_journal(&journal_dir, &journal).expect("write journal");

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recover edited copy");

        assert_eq!(report.manual_review.len(), 1);
        assert!(journal_path.exists());
        assert_eq!(
            fs::read(destination.join("item.txt")).expect("preserve edit"),
            b"external edit"
        );
    }
    #[test]
    fn sibling_delete_recovery_restores_each_indexed_tree_backup() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = root.join(".state/journal");
        let names = ["first", "second", "third"];
        let mut entries = Vec::new();
        let mut operations = Vec::new();
        let mut backups = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let destination = root.join("native").join(name);
            fs::create_dir_all(destination.join("nested")).expect("source tree");
            fs::write(
                destination.join("nested/item.txt"),
                format!("original tree {name}"),
            )
            .expect("source content");
            let before_hash = super::tree_hash(&destination).expect("tree hash");
            let backup = root.join(format!(
                "backups/op-dir-delete/scope/native/op-dir-delete-{index}.deleted"
            ));
            fs::create_dir_all(backup.parent().expect("backup parent")).expect("backup directory");
            fs::rename(&destination, &backup).expect("simulate completed delete");
            entries.push(JournalEntry {
                path: destination,
                before_hash: Some(before_hash.clone()),
                after_hash: String::new(),
            });
            operations.push(FileOp::Delete {
                path: PathBuf::from("native").join(name),
                expected: ModelExpectedState::Hash {
                    hash: ContentHash::parse(&before_hash).expect("valid tree hash"),
                },
            });
            backups.push(backup);
        }
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "op-dir-delete".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries,
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-dir-delete".to_owned(),
                preview_id: "preview-dir-delete".to_owned(),
                operation_kind: at_model::OperationKind::parse("delete").expect("known kind"),
                scope: None,
                project_id: None,
                summary: "sibling directory delete recovery".to_owned(),
                ops: operations,
                created_at_unix_seconds: 0,
            }),
            completed_ops: (0..names.len()).collect(),
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: None,
        };
        write_journal(&journal_dir, &journal).expect("write pending journal");

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recover sibling deletes");

        assert!(report.manual_review.is_empty(), "{report:?}");
        for (name, backup) in names.iter().zip(&backups) {
            let destination = root.join("native").join(name);
            assert_eq!(
                fs::read(destination.join("nested/item.txt")).expect("restored file"),
                format!("original tree {name}").as_bytes()
            );
            assert!(!backup.exists());
        }
    }

    #[test]
    fn forged_journal_entry_outside_authorized_roots_is_refused_and_writes_nothing() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = sandbox.path().join("journal");
        let authorized_dir = sandbox.path().join("authorized");
        let outside_dir = sandbox.path().join("outside");
        fs::create_dir_all(&root).expect("store");
        fs::create_dir_all(&authorized_dir).expect("authorized");
        fs::create_dir_all(&outside_dir).expect("outside");

        let outside_file = outside_dir.join("pwned.txt");
        let forged_op = FileOp::Write {
            path: outside_file.clone(),
            bytes: b"forged content".to_vec(),
            expected: ModelExpectedState::Absent,
            follow_symlink: false,
            backup: false,
        };

        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "op-forged".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: vec![JournalEntry {
                path: outside_file.clone(),
                before_hash: None,
                after_hash: crate::sha256_hex(b"forged content"),
            }],
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-forged".to_owned(),
                preview_id: "preview-forged".to_owned(),
                operation_kind: at_model::OperationKind::parse("edit_skill").expect("kind"),
                scope: None,
                project_id: None,
                summary: "forged test".to_owned(),
                ops: vec![forged_op],
                created_at_unix_seconds: 0,
            }),
            completed_ops: vec![0],
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: Some(vec![authorized_dir.clone()]),
            backup_root: None,
        };

        let path = write_journal(&journal_dir, &journal).expect("write forged journal");
        let mut callbacks = Callbacks::default();
        // The journal claims its own root; the caller authorized a different one, so the
        // destination is refused however the journal describes itself.
        let policy = RecoveryPolicy::new(
            &root,
            [root.clone(), authorized_dir.clone()],
            [root.join("backups")],
        );
        let report = recover_journals(&journal_dir, &policy, &mut callbacks).expect("recover");
        assert_eq!(report.manual_review.len(), 1);
        assert_eq!(report.manual_review[0].journal_path, path);
        assert!(
            report.manual_review[0]
                .reason
                .contains("outside authorized roots")
        );
        assert!(
            !outside_file.exists(),
            "nothing must be written outside authorized roots"
        );
    }

    /// A journal from a cloned repository: `.agentuna/.state/journal/*.journal` travels through
    /// Git, so the file is attacker-authored data. Its own `authorized_roots` are self-attested
    /// and prove nothing; only the caller's roots may authorize anything.
    fn forged_delete_journal(
        journal_dir: &Path,
        backups: &Path,
        target: &Path,
        payload: &[u8],
        authorized_roots: Option<Vec<PathBuf>>,
    ) -> PendingJournal {
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "forged-delete".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: vec![JournalEntry {
                path: target.to_path_buf(),
                before_hash: Some(crate::sha256_hex(payload)),
                after_hash: String::new(),
            }],
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-forged-delete".to_owned(),
                preview_id: "preview-forged-delete".to_owned(),
                operation_kind: at_model::OperationKind::parse("edit_skill").expect("known kind"),
                scope: None,
                project_id: None,
                summary: "forged delete".to_owned(),
                ops: vec![FileOp::Delete {
                    path: target.to_path_buf(),
                    expected: ModelExpectedState::Hash {
                        hash: ContentHash::parse(&crate::sha256_hex(payload)).expect("valid hash"),
                    },
                }],
                created_at_unix_seconds: 0,
            }),
            completed_ops: vec![0],
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots,
            backup_root: Some(backups.to_path_buf()),
        };
        write_journal(journal_dir, &journal).expect("write forged journal");
        journal
    }

    #[test]
    fn forged_journal_without_self_attested_roots_cannot_write_outside_the_store() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repo = sandbox.path().join("cloned-repo");
        let root = repo.join(at_store::paths::STORE_DIR_NAME);
        let journal_dir = root.join(".state").join("journal");
        let launch_agents = sandbox.path().join("LaunchAgents");
        let backups = repo.join("attacker-backups");
        fs::create_dir_all(&journal_dir).expect("journal dir");
        fs::create_dir_all(&launch_agents).expect("launch agents");
        fs::create_dir_all(&backups).expect("attacker backups");
        let payload = b"<?xml version=\"1.0\"?><plist><dict/></plist>";
        let target = launch_agents.join("com.attacker.evil.plist");
        fs::write(backups.join("forged-delete-0.deleted"), payload).expect("attacker blob");

        forged_delete_journal(&journal_dir, &backups, &target, payload, None);

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recovery runs");
        assert!(
            !target.exists(),
            "a journal from a cloned repository must never write outside caller roots"
        );
        assert_eq!(report.manual_review.len(), 1, "{report:?}");
        assert!(report.manual_review[0].journal_path.exists());
    }

    #[test]
    fn forged_journal_self_attested_root_slash_is_ignored() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repo = sandbox.path().join("cloned-repo");
        let root = repo.join(at_store::paths::STORE_DIR_NAME);
        let journal_dir = root.join(".state").join("journal");
        let launch_agents = sandbox.path().join("LaunchAgents");
        let backups = repo.join("attacker-backups");
        fs::create_dir_all(&journal_dir).expect("journal dir");
        fs::create_dir_all(&launch_agents).expect("launch agents");
        fs::create_dir_all(&backups).expect("attacker backups");
        let payload = b"<?xml version=\"1.0\"?><plist><dict/></plist>";
        let target = launch_agents.join("com.attacker.evil.plist");
        fs::write(backups.join("forged-delete-0.deleted"), payload).expect("attacker blob");

        forged_delete_journal(
            &journal_dir,
            &backups,
            &target,
            payload,
            Some(vec![PathBuf::from("/")]),
        );

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recovery runs");
        assert!(
            !target.exists(),
            "a self-attested `/` root must not authorize an attacker destination"
        );
        assert_eq!(report.manual_review.len(), 1, "{report:?}");
    }

    #[test]
    fn symlinked_journal_file_is_refused_before_it_is_executed() {
        use std::os::unix::fs::symlink;

        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = sandbox.path().join("journal");
        fs::create_dir_all(&root).expect("store");
        fs::create_dir_all(&journal_dir).expect("journal dir");
        fs::write(root.join("config.txt"), b"after").expect("target");
        let mut journal = write_journal_for(
            &root,
            &journal_dir,
            "op-symlink",
            &[("config.txt", b"before", b"after")],
            vec![0],
        );
        journal.operation_id = "op-symlink".to_owned();
        let payload = sandbox.path().join("attacker.journal");
        fs::write(
            &payload,
            serde_json::to_vec_pretty(&journal).expect("encode"),
        )
        .expect("payload");
        let link = journal_dir.join("op-symlink.journal");
        fs::remove_file(&link).expect("remove real journal");
        symlink(&payload, &link).expect("symlinked journal");

        let mut callbacks = Callbacks::default();
        callbacks.before.insert(0, b"before".to_vec());
        let report = recover_journals(&journal_dir, &policy_for(&root), &mut callbacks)
            .expect("recovery runs");

        assert_eq!(
            fs::read(root.join("config.txt")).expect("read target"),
            b"after",
            "a symlinked journal must never be executed"
        );
        assert_eq!(report.manual_review.len(), 1, "{report:?}");
        assert!(link.exists(), "the symlinked journal is left for a human");
    }

    #[test]
    fn project_store_journal_needs_an_operation_the_app_recorded() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox
            .path()
            .join("cloned-repo")
            .join(at_store::paths::STORE_DIR_NAME);
        let journal_dir = root.join(".state").join("journal");
        fs::create_dir_all(&journal_dir).expect("journal dir");
        fs::write(root.join("config.txt"), b"after").expect("target");
        write_journal_for(
            &root,
            &journal_dir,
            "op-project",
            &[("config.txt", b"before", b"after")],
            vec![0],
        );
        let policy = RecoveryPolicy::new(&root, [root.clone()], [root.join("backups")])
            .requiring_recorded_operation();
        let mut callbacks = Callbacks::default();
        callbacks.before.insert(0, b"before".to_vec());

        let report = recover_journals(&journal_dir, &policy, &mut callbacks).expect("recover");
        assert_eq!(
            fs::read(root.join("config.txt")).expect("read target"),
            b"after",
            "a project store journal is only replayed for an operation this app recorded"
        );
        assert_eq!(report.manual_review.len(), 1, "{report:?}");
        assert!(
            callbacks.finished.is_empty(),
            "an unrecorded operation id must not reach the operation history"
        );

        // The same journal is ordinary debris once the operation is on record.
        callbacks.in_flight.push("op-project".to_owned());
        let report = recover_journals(&journal_dir, &policy, &mut callbacks).expect("recover");
        assert_eq!(report.rolled_back.len(), 1, "{report:?}");
        assert_eq!(
            fs::read(root.join("config.txt")).expect("read restored"),
            b"before"
        );
        assert_eq!(
            callbacks.finished,
            [("op-project".to_owned(), false, false)]
        );
    }

    #[test]
    fn symlinked_delete_blob_is_never_renamed_into_place() {
        use std::os::unix::fs::symlink;

        let sandbox = tempfile::tempdir().expect("sandbox");
        let root = sandbox.path().join("store");
        let journal_dir = root.join(".state").join("journal");
        let backups = root.join("backups");
        fs::create_dir_all(&journal_dir).expect("journal dir");
        fs::create_dir_all(&backups).expect("backups");
        let payload = b"<?xml version=\"1.0\"?><plist/>";
        let source = sandbox.path().join("attacker.plist");
        fs::write(&source, payload).expect("payload");
        symlink(&source, backups.join("forged-delete-0.deleted")).expect("symlinked blob");
        let target = root.join("native").join("skill");
        let journal = PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: "forged-delete".to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries: vec![JournalEntry {
                path: target.clone(),
                before_hash: Some(crate::sha256_hex(payload)),
                after_hash: String::new(),
            }],
            temp_files: Vec::new(),
            change_set: Some(ChangeSet {
                id: "set-forged-delete".to_owned(),
                preview_id: "preview-forged-delete".to_owned(),
                operation_kind: at_model::OperationKind::parse("delete").expect("known kind"),
                scope: None,
                project_id: None,
                summary: "symlinked blob".to_owned(),
                ops: vec![FileOp::Delete {
                    path: target.clone(),
                    expected: ModelExpectedState::Hash {
                        hash: ContentHash::parse(&crate::sha256_hex(payload)).expect("valid hash"),
                    },
                }],
                created_at_unix_seconds: 0,
            }),
            completed_ops: vec![0],
            created_dirs: Vec::new(),
            committed: false,
            authorized_roots: None,
            backup_root: Some(backups),
        };
        write_journal(&journal_dir, &journal).expect("write journal");

        let report = recover_journals(&journal_dir, &policy_for(&root), &mut Callbacks::default())
            .expect("recovery runs");
        assert!(
            !target.exists(),
            "a symbolic link is never restored as if it were the saved original"
        );
        assert_eq!(report.manual_review.len(), 1, "{report:?}");
    }
}
