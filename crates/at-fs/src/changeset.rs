//! Journaled application of reviewed multi-file ChangeSets.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use at_model::{ChangeSet, FileOp};
use fs4::FileExt;
use time::OffsetDateTime;

use crate::atomic::{BackupPolicy, WriteRequest, apply as apply_write};
use crate::backup::BackupMeta;
use crate::concurrency::ExpectedState;
use crate::hash::sha256_hex;
use crate::recovery::{JOURNAL_VERSION, JournalEntry, PendingJournal, write_journal};
use crate::{FsError, MAX_FILE_BYTES};

/// Database callbacks for operation lifecycle and file history.
pub trait OperationRecorder {
    /// Opens a running operation.
    fn begin(&mut self, change_set: &ChangeSet) -> Result<String, String>;
    /// Records one completed operation file and optional backup catalogue entry.
    fn file_completed(
        &mut self,
        operation_id: &str,
        path: &Path,
        before_hash: Option<&str>,
        after_hash: Option<&str>,
        backup_path: Option<&Path>,
    ) -> Result<(), String>;
    /// Finishes the operation; a failed rollback requires manual review.
    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String>;
}

/// Concrete operation recorder backed by at-db repositories.
pub struct DbOperationRecorder<'a> {
    connection: &'a rusqlite::Connection,
    backup_retention_days: u32,
}

impl<'a> DbOperationRecorder<'a> {
    /// Builds a recorder, clamping retention to the contract's 30-day minimum.
    #[must_use]
    pub const fn new(connection: &'a rusqlite::Connection, backup_retention_days: u32) -> Self {
        Self {
            connection,
            backup_retention_days: if backup_retention_days < 30 {
                30
            } else {
                backup_retention_days
            },
        }
    }
}

impl OperationRecorder for DbOperationRecorder<'_> {
    fn begin(&mut self, set: &ChangeSet) -> Result<String, String> {
        use at_db::ops::{NewOperation, OperationActor, OperationKind, begin};
        use std::str::FromStr;
        let kind = OperationKind::from_str(set.operation_kind.as_str())
            .map_err(|error| error.to_string())?;
        let mut operation = NewOperation::new(kind, OperationActor::Ui).with_summary(&set.summary);
        operation.scope = set.scope;
        begin(self.connection, &operation)
            .map(|id| id.to_string())
            .map_err(|error| error.to_string())
    }

    fn file_completed(
        &mut self,
        id: &str,
        path: &Path,
        before: Option<&str>,
        after: Option<&str>,
        backup_path: Option<&Path>,
    ) -> Result<(), String> {
        use at_db::backups::{NewBackup, record_backup};
        use at_db::ops::{FileAction, OperationId, TouchedFile, attach_file};
        use std::str::FromStr;
        let operation = OperationId::from_str(id).map_err(|error| error.to_string())?;
        let action = match (before, after) {
            (None, Some(_)) => FileAction::Created,
            (Some(_), None) => FileAction::Deleted,
            (Some(_), Some(_)) => FileAction::Modified,
            (None, None) => return Err("operation file has no before or after hash".to_owned()),
        };
        let path_text = path.to_string_lossy().into_owned();
        let backup_id = if let Some(blob) = backup_path {
            let size = fs::symlink_metadata(blob)
                .map_err(|error| error.to_string())?
                .len();
            let expires = (OffsetDateTime::now_utc()
                + time::Duration::days(i64::from(self.backup_retention_days)))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| error.to_string())?;
            let blob_text = blob.to_string_lossy().into_owned();
            let mut row = NewBackup::new(&path_text, &blob_text)
                .with_operation(id)
                .with_content(before, i64::try_from(size).ok());
            row.expires_at = Some(&expires);
            Some(record_backup(self.connection, &row).map_err(|error| error.to_string())?)
        } else {
            None
        };
        let mut row = TouchedFile::new(&path_text, action).with_hashes(before, after);
        if let Some(backup_id) = backup_id.as_deref() {
            row = row.with_backup(backup_id);
        }
        attach_file(self.connection, &operation, &row).map_err(|error| error.to_string())
    }

    fn finish(&mut self, id: &str, succeeded: bool, manual_review: bool) -> Result<(), String> {
        use at_db::ops::{OperationId, OperationStatus, finish};
        use std::str::FromStr;
        let operation = OperationId::from_str(id).map_err(|error| error.to_string())?;
        let status = if succeeded {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        };
        let error_kind = manual_review.then_some("manual_review_required");
        if finish(self.connection, &operation, status, error_kind)
            .map_err(|error| error.to_string())?
        {
            Ok(())
        } else {
            Err("operation row was not running".to_owned())
        }
    }
}

/// Parameters for applying a ChangeSet.
pub struct ApplyContext<'a> {
    /// Active store root for resolving relative paths.
    pub store_root: &'a Path,
    /// Backups directory; apply creates a timestamped operation subfolder beneath it.
    pub backup_folder: &'a Path,
    /// Operation-history repository.
    pub recorder: &'a mut dyn OperationRecorder,
}

/// Explicitly authorized external roots for native agent configuration writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyRoots {
    roots: Vec<PathBuf>,
}

impl ApplyRoots {
    /// Canonicalizes a non-empty list of existing directory roots.
    ///
    /// `/` is never accepted because it would authorize the entire filesystem.
    ///
    /// # Errors
    /// Returns [`ChangeSetApplyError::InvalidAuthorizedRoot`] for a non-directory,
    /// non-absolute, root-directory or uncanonicalizable path.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self, ChangeSetApplyError> {
        let mut canonical = Vec::new();
        for root in roots {
            if !root.is_absolute() || root == Path::new("/") {
                return Err(ChangeSetApplyError::InvalidAuthorizedRoot(root));
            }
            let root = fs::canonicalize(&root)
                .map_err(|_| ChangeSetApplyError::InvalidAuthorizedRoot(root))?;
            if root == Path::new("/") || !root.is_dir() {
                return Err(ChangeSetApplyError::InvalidAuthorizedRoot(root));
            }
            if !canonical.contains(&root) {
                canonical.push(root);
            }
        }
        if canonical.is_empty() {
            return Err(ChangeSetApplyError::InvalidAuthorizedRoot(PathBuf::new()));
        }
        canonical.sort();
        Ok(Self { roots: canonical })
    }

    fn root_for(&self, path: &Path) -> Result<(&Path, PathBuf), ChangeSetApplyError> {
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::CurDir | Component::ParentDir | Component::Prefix(_)
                )
            })
        {
            return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                path.to_path_buf(),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ChangeSetApplyError::OutsideAuthorizedRoots(path.to_path_buf()))?;
        let file_name = path
            .file_name()
            .filter(|_| matches!(path.components().next_back(), Some(Component::Normal(_))))
            .ok_or_else(|| ChangeSetApplyError::OutsideAuthorizedRoots(path.to_path_buf()))?;
        let mut ancestor = parent;
        let mut missing = Vec::new();
        loop {
            match fs::symlink_metadata(ancestor) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let Some(name) = ancestor.file_name() else {
                        return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                            path.to_path_buf(),
                        ));
                    };
                    missing.push(name.to_os_string());
                    ancestor = ancestor.parent().ok_or_else(|| {
                        ChangeSetApplyError::OutsideAuthorizedRoots(path.to_path_buf())
                    })?;
                }
                Err(_) => {
                    return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                        path.to_path_buf(),
                    ));
                }
            }
        }
        let ancestor_real = fs::canonicalize(ancestor)
            .map_err(|_| ChangeSetApplyError::OutsideAuthorizedRoots(path.to_path_buf()))?;
        let mut canonical_path = ancestor_real.clone();
        for component in missing.iter().rev() {
            if component == OsStr::new(".") || component == OsStr::new("..") {
                return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                    path.to_path_buf(),
                ));
            }
            canonical_path.push(component);
        }
        canonical_path.push(file_name);
        let mut matched: Option<(&Path, PathBuf, usize)> = None;
        for root in &self.roots {
            if let Ok(relative) = canonical_path.strip_prefix(root)
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                let root_depth = root.components().count();
                if matched
                    .as_ref()
                    .is_none_or(|(_, _, matched_depth)| root_depth > *matched_depth)
                {
                    matched = Some((root.as_path(), relative.to_path_buf(), root_depth));
                }
            }
        }
        let (root, relative, _) = matched
            .ok_or_else(|| ChangeSetApplyError::OutsideAuthorizedRoots(path.to_path_buf()))?;
        if !ancestor_real.starts_with(root) {
            return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                path.to_path_buf(),
            ));
        }
        Ok((root, relative))
    }
}
/// Successful ChangeSet application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Database operation identifier.
    pub operation_id: String,
    /// Number of operations applied.
    pub completed: usize,
    /// Single-use rollback token when the reversible apply API was selected.
    pub rollback_handle: Option<RollbackHandle>,
}

/// In-memory token for reversing a completed ChangeSet in the same process.
///
/// The token retains reviewed pre-state and delete backups. Dropping it commits the applied
/// operation permanently; it is not a durable undo record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackHandle {
    set: ChangeSet,
    root: PathBuf,
    journal_path: PathBuf,
    done: Vec<CompletedOperation>,
    paths: Vec<PathBuf>,
    entries: Vec<JournalEntry>,
    created_dirs: Vec<PathBuf>,
}

/// ChangeSet application failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChangeSetApplyError {
    /// Pure ChangeSet validation failed.
    #[error("invalid change set: {0}")]
    Invalid(#[from] at_model::ChangeSetError),
    /// Path is unsafe or this implementation does not support the operation.
    #[error("unsafe or unsupported change-set path: {0}")]
    UnsafePath(PathBuf),
    /// Path is not beneath any explicitly authorized external root.
    #[error("path is outside authorized roots: {0}")]
    OutsideAuthorizedRoots(PathBuf),
    /// An explicit authorization root is invalid.
    #[error("invalid authorized root: {0}")]
    InvalidAuthorizedRoot(PathBuf),
    /// Reviewed path state is stale.
    #[error("stale change-set target: {0}")]
    Stale(PathBuf),
    /// Filesystem pipeline failure.
    #[error(transparent)]
    Fs(#[from] FsError),
    /// Journal failure.
    #[error("journal failure: {0}")]
    Journal(String),
    /// Operation repository failure.
    #[error("operation history failure: {0}")]
    History(String),
    /// Apply failed and reverse rollback was attempted.
    #[error("change-set failed: {cause}; rollback: {rollback}")]
    Apply {
        /// Failure that began rollback.
        cause: String,
        /// Rollback result or manual-review detail.
        rollback: String,
    },
}

/// Applies a reviewed ChangeSet, journals it before writes and rolls back completed operations.
///
/// Preflight errors produce neither history rows nor journals. In-flight journals are v2 and
/// retain the full reviewed set and per-op progress; v1 recovery journals remain readable.
///
/// # Errors
/// Returns a validation, stale-state, IO, journal, or operation-history error. Failed rollback
/// preserves its journal for manual recovery and marks the operation as requiring review.
pub fn apply(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    apply_inner(set, context, None, None, false)
}

/// Applies a ChangeSet with ownership checks for destructive operations on native paths.
///
/// The supplied guard is checked during preflight and immediately before each absolute-path
/// `Delete` or `Move` source is moved. Relative paths remain store-owned operations.
///
/// # Errors
/// Returns the same errors as [`apply`], including [`FsError::RefusedOverwrite`] for an
/// unowned native target.
pub fn apply_with_ownership_guard(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    guard: &dyn crate::guard::OverwriteGuard,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    apply_inner(set, context, None, Some(guard), false)
}

/// Applies a ChangeSet that may include absolute paths beneath explicitly authorized roots.
///
/// Relative operations retain the existing store-root behavior. Absolute destinations are
/// accepted beneath any entry; when entries overlap, the most specific matching root is used.
///
/// # Errors
/// Returns [`ChangeSetApplyError::OutsideAuthorizedRoots`] when a path escapes or crosses a
/// symlink outside its authorized root; other failures match [`apply`].
pub fn apply_with_roots(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    roots: &ApplyRoots,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    apply_inner(set, context, Some(roots), None, false)
}
/// Applies a reviewed ChangeSet while retaining a single-use in-process rollback token.
///
/// This is for multi-phase engine operations that must reverse a completed first phase when a
/// later phase fails. The token is not durable and must be consumed or dropped before shutdown.
///
/// # Errors
/// Returns the same validation, stale-state, IO, journal, or history errors as
/// [`apply_with_roots`].
pub fn apply_with_roots_reversible(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    roots: &ApplyRoots,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    apply_inner(set, context, Some(roots), None, true)
}

/// Reverses a completed reversible ChangeSet, refusing to overwrite later external edits.
///
/// # Errors
/// Returns [`ChangeSetApplyError::Apply`] when the reviewed result has changed or restoration
/// fails, and a journal error when its lock or journal cannot be maintained.
pub fn rollback_committed(handle: RollbackHandle) -> Result<(), ChangeSetApplyError> {
    let lock_dir = handle.root.join(".state/locks");
    fs::create_dir_all(&lock_dir)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_dir.join("store.lock"))
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    FileExt::try_lock(&lock_file)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    rollback(
        &handle.set,
        &handle.root,
        &handle.done,
        &handle.paths,
        &handle.entries,
        &handle.created_dirs,
    )
    .map_err(|cause| ChangeSetApplyError::Apply {
        cause,
        rollback: "phase rollback failed".to_owned(),
    })?;
    fs::remove_file(&handle.journal_path)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    Ok(())
}
/// Commits a reversible apply by removing its in-process journal and discarding rollback state.
///
/// The operation's normal backup files remain available through the operation history.
///
/// # Errors
/// Returns a journal error if the retained journal cannot be removed.
pub fn commit_reversible(handle: RollbackHandle) -> Result<(), ChangeSetApplyError> {
    fs::remove_file(&handle.journal_path)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))
}

/// Applies a ChangeSet under authorized roots and with ownership checks for destructive paths.
///
/// # Errors
/// Returns [`ChangeSetApplyError::OutsideAuthorizedRoots`] for an escaping absolute path and
/// [`FsError::RefusedOverwrite`] for an unowned destructive target.
pub fn apply_with_roots_and_guard(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    roots: &ApplyRoots,
    guard: &dyn crate::guard::OverwriteGuard,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    apply_inner(set, context, Some(roots), Some(guard), false)
}

fn apply_inner(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    authorized_roots: Option<&ApplyRoots>,
    ownership_guard: Option<&dyn crate::guard::OverwriteGuard>,
    reversible: bool,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    set.validate()?;
    let mut paths = Vec::with_capacity(set.ops.len());
    let mut path_roots = Vec::with_capacity(set.ops.len());
    let mut backup_relpaths = Vec::with_capacity(set.ops.len());
    for op in &set.ops {
        let destination = op
            .destination()
            .ok_or_else(|| ChangeSetApplyError::UnsafePath(PathBuf::new()))?;
        if destination.as_os_str().is_empty() {
            return Err(ChangeSetApplyError::UnsafePath(destination.to_path_buf()));
        }
        if destination.is_absolute() {
            let roots = authorized_roots
                .ok_or_else(|| ChangeSetApplyError::UnsafePath(destination.to_path_buf()))?;
            let (root, relative) = roots.root_for(destination)?;
            paths.push(root.join(&relative));
            path_roots.push(root.to_path_buf());
            backup_relpaths.push(relative);
        } else {
            if destination
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(ChangeSetApplyError::UnsafePath(destination.to_path_buf()));
            }
            paths.push(context.store_root.join(destination));
            path_roots.push(context.store_root.to_path_buf());
            backup_relpaths.push(destination.to_path_buf());
        }
    }
    let mut created_dirs = Vec::new();
    if authorized_roots.is_some() {
        for ((op, path), root) in set.ops.iter().zip(&paths).zip(&path_roots) {
            if !op.destination().is_some_and(Path::is_absolute) {
                continue;
            }
            let mut parent = path.parent();
            while let Some(directory) = parent {
                if directory == root {
                    break;
                }
                match fs::symlink_metadata(directory) {
                    Ok(metadata) if metadata.is_dir() => break,
                    Ok(_) => {
                        return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                            op.destination().map(Path::to_path_buf).unwrap_or_default(),
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        created_dirs.push(directory.to_path_buf());
                        parent = directory.parent();
                    }
                    Err(_) => {
                        return Err(ChangeSetApplyError::OutsideAuthorizedRoots(
                            op.destination().map(Path::to_path_buf).unwrap_or_default(),
                        ));
                    }
                }
            }
        }
    }
    created_dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    created_dirs.dedup();
    for (index, ((op, path), root)) in set.ops.iter().zip(&paths).zip(&path_roots).enumerate() {
        let replaced_by_prior_move = matches!(op, FileOp::Symlink { .. })
            && set.ops[..index].iter().any(|prior| {
                matches!(prior, FileOp::Move { from, .. }
                    if op.destination().is_some_and(|destination| from == destination)
                        || context.store_root.join(from) == *path)
                    || matches!(prior, FileOp::Delete { path: deleted, .. }
                        if Some(deleted.as_path()) == op.destination())
            })
            || matches!(
                op,
                FileOp::Copy { .. } | FileOp::CopyTree { .. } | FileOp::Move { .. }
            ) && set.ops[..index].iter().any(|prior| {
                matches!(prior, FileOp::Delete { path: deleted, .. }
                    if Some(deleted.as_path()) == op.destination())
            });
        validate_expected(op, path, root, context.store_root, replaced_by_prior_move)?;
    }
    if let Some(guard) = ownership_guard {
        for op in &set.ops {
            let destructive_path = match op {
                FileOp::Delete { path, .. } if path.is_absolute() => Some(path),
                FileOp::Move { from, .. } if from.is_absolute() => Some(from),
                _ => None,
            };
            if let Some(path) = destructive_path {
                guard.check(path, false)?;
            }
        }
    }

    let lock_dir = context.store_root.join(".state/locks");
    fs::create_dir_all(&lock_dir)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_dir.join("store.lock"))
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    FileExt::try_lock(&lock_file)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    let operation_id = context
        .recorder
        .begin(set)
        .map_err(ChangeSetApplyError::History)?;
    let started_at = OffsetDateTime::now_utc();
    let backup_folder =
        crate::backup::operation_backup_dir(context.backup_folder, started_at, &operation_id)
            .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    fs::create_dir_all(&backup_folder)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    let journal_dir = context.store_root.join(".state/journal");
    let entries = set
        .ops
        .iter()
        .zip(&paths)
        .map(|(op, path)| {
            let before_hash = node_hash(path).ok().flatten();
            JournalEntry {
                path: path.clone(),
                before_hash,
                after_hash: match op {
                    FileOp::Write { bytes, .. } => sha256_hex(bytes),
                    _ => String::new(),
                },
            }
        })
        .collect();
    let mut journal = PendingJournal {
        journal_version: JOURNAL_VERSION,
        operation_id: operation_id.clone(),
        started_at,
        entries,
        temp_files: Vec::new(),
        change_set: Some(set.clone()),
        completed_ops: Vec::new(),
        created_dirs: created_dirs.clone(),
        committed: false,
    };
    let journal_path = write_journal(&journal_dir, &journal)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    let mut done = Vec::with_capacity(set.ops.len());
    for (index, ((op, path), backup_relative)) in
        set.ops.iter().zip(&paths).zip(&backup_relpaths).enumerate()
    {
        let prior = fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|_| fs::read(path).ok());
        let backup_path = match execute_op(
            op,
            path,
            backup_relative,
            &ExecuteContext {
                set,
                operation_id: &operation_id,
                store_root: context.store_root,
                backup_root: &backup_folder,
                ownership_guard,
            },
        ) {
            Ok(path) => path,
            Err(cause) => {
                return fail_rollback(
                    context,
                    RollbackState {
                        set,
                        id: &operation_id,
                        journal_path: &journal_path,
                        done: &done,
                        paths: &paths,
                        entries: &journal.entries,
                        created_dirs: &created_dirs,
                    },
                    format!(
                        "ChangeSet operation {index} at `{}` failed: {cause}",
                        path.display()
                    ),
                );
            }
        };
        let before_hash = journal
            .entries
            .get(index)
            .and_then(|entry| entry.before_hash.clone());
        let after_hash = node_hash(path).ok().flatten();
        if let Some(entry) = journal.entries.get_mut(index) {
            entry.after_hash = after_hash.clone().unwrap_or_default();
        }
        done.push(CompletedOperation {
            index,
            prior,
            delete_backup: backup_path.clone(),
        });
        journal.completed_ops.push(index);
        if let Err(error) = write_journal(&journal_dir, &journal) {
            return fail_rollback(
                context,
                RollbackState {
                    set,
                    id: &operation_id,
                    journal_path: &journal_path,
                    done: &done,
                    paths: &paths,
                    entries: &journal.entries,
                    created_dirs: &created_dirs,
                },
                error.to_string(),
            );
        }
        if let Err(error) = context.recorder.file_completed(
            &operation_id,
            path,
            before_hash.as_deref(),
            after_hash.as_deref(),
            backup_path.as_deref(),
        ) {
            return fail_rollback(
                context,
                RollbackState {
                    set,
                    id: &operation_id,
                    journal_path: &journal_path,
                    done: &done,
                    paths: &paths,
                    entries: &journal.entries,
                    created_dirs: &created_dirs,
                },
                error,
            );
        }
    }
    journal.committed = true;
    write_journal(&journal_dir, &journal)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    context
        .recorder
        .finish(&operation_id, true, false)
        .map_err(ChangeSetApplyError::History)?;
    if !reversible {
        fs::remove_file(&journal_path)
            .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    }
    let completed = done.len();
    let rollback_handle = reversible.then(|| RollbackHandle {
        set: set.clone(),
        root: context.store_root.to_path_buf(),
        journal_path,
        done,
        paths,
        entries: journal.entries,
        created_dirs,
    });
    Ok(ApplyOutcome {
        operation_id,
        completed,
        rollback_handle,
    })
}

struct ExecuteContext<'a> {
    set: &'a ChangeSet,
    operation_id: &'a str,
    store_root: &'a Path,
    backup_root: &'a Path,
    ownership_guard: Option<&'a dyn crate::guard::OverwriteGuard>,
}

fn execute_op(
    op: &FileOp,
    path: &Path,
    backup_relative: &Path,
    context: &ExecuteContext<'_>,
) -> Result<Option<PathBuf>, String> {
    crate::failpoint::hit(crate::failpoint::FaultPoint::ChangeSetOperation, path)
        .map_err(|failure| failure.into_io().to_string())?;
    if path.is_absolute() {
        crate::guard::create_parents_if_target_absent(path).map_err(|error| error.to_string())?;
    }
    match op {
        FileOp::Write {
            bytes,
            expected,
            follow_symlink,
            backup,
            ..
        } => {
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err("write exceeds maximum file size".to_owned());
            }
            let expected = translate_expected(expected).map_err(|error| error.to_string())?;
            let mut request = if matches!(&expected, ExpectedState::Absent) {
                WriteRequest::create(path, bytes.clone())
            } else {
                WriteRequest::replace(path, bytes.clone(), expected)
            };
            request.follow_symlink = *follow_symlink;
            if *backup {
                request.backup = BackupPolicy::OperationFolder(backup_folder(
                    context.backup_root,
                    context.set.scope,
                    backup_relative,
                )?);
            }
            request.backup_meta =
                backup_meta(context.set, context.operation_id, path, backup_relative);
            apply_write(&request)
                .map(|outcome| outcome.backup_path)
                .map_err(|error| error.to_string())
        }
        FileOp::Delete { expected, .. } => {
            let wanted = translate_expected(expected).map_err(|error| error.to_string())?;
            let actual = node_expected(path)?;
            if !same_expected(&wanted, &actual) {
                return Err(format!("stale target: {}", path.display()));
            }
            if matches!(actual, ExpectedState::Absent) {
                return Err(format!("delete target is absent: {}", path.display()));
            }
            let folder = backup_folder(context.backup_root, context.set.scope, backup_relative)?;
            let deleted = folder.join(format!("{}.deleted", context.operation_id));
            if !expected_absent(&deleted) {
                return Err(format!(
                    "delete backup already exists: {}",
                    deleted.display()
                ));
            }
            if path.is_absolute()
                && let Some(guard) = context.ownership_guard
            {
                guard
                    .check(path, false)
                    .map_err(|error| error.to_string())?;
            }
            fs::rename(path, &deleted).map_err(|error| error.to_string())?;
            Ok(Some(deleted))
        }
        FileOp::Symlink { target, mode, .. } => {
            let source = resolve_path(context.store_root, target)?;
            let request = match mode {
                at_model::DeployMode::Link => {
                    crate::deploy::DeployRequest::new(&source, path, at_model::LinkMode::Link)
                        .with_copy_fallback()
                }
                at_model::DeployMode::Copy => {
                    crate::deploy::DeployRequest::new(&source, path, at_model::LinkMode::Copy)
                }
                _ => return Err("unsupported link mode".to_owned()),
            };
            crate::deploy::deploy(&request).map_err(|error| error.to_string())?;
            Ok(None)
        }
        FileOp::Copy {
            source, expected, ..
        } => {
            let source = resolve_path(context.store_root, source)?;
            let wanted = translate_expected(expected).map_err(|error| error.to_string())?;
            if wanted != ExpectedState::Absent {
                return Err("copy destination replacement is not supported".to_owned());
            }
            if source.is_dir() {
                crate::deploy::copy_dir(&source, path).map_err(|error| error.to_string())?;
                Ok(None)
            } else {
                let bytes = fs::read(&source).map_err(|error| error.to_string())?;
                let request = WriteRequest::create(path, bytes);
                apply_write(&request)
                    .map(|_| None)
                    .map_err(|error| error.to_string())
            }
        }
        FileOp::CopyTree {
            source, expected, ..
        } => {
            let source = resolve_path(context.store_root, source)?;
            let wanted = translate_expected(expected).map_err(|error| error.to_string())?;
            if wanted != ExpectedState::Absent {
                return Err("copy-tree destination replacement is not supported".to_owned());
            }
            // Plain content: no copy manifest, so the destination hashes as the source does.
            copy_node(&source, path)?;
            Ok(None)
        }
        FileOp::Mkdir { mode, parents, .. } => {
            if path.is_absolute() {
                crate::guard::create_parents_if_target_absent(path)
                    .map_err(|error| error.to_string())?;
                fs::create_dir(path).map_err(|error| error.to_string())?;
            } else if *parents {
                fs::create_dir_all(path).map_err(|error| error.to_string())?;
            } else {
                fs::create_dir(path).map_err(|error| error.to_string())?;
            }
            if let Some(mode) = mode {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(*mode & 0o777))
                    .map_err(|error| error.to_string())?;
            }
            Ok(None)
        }
        FileOp::Move { from, expected, .. } => {
            let source = resolve_path(context.store_root, from)?;
            if from.is_absolute()
                && let Some(guard) = context.ownership_guard
            {
                guard
                    .check(&source, false)
                    .map_err(|error| error.to_string())?;
            }
            let wanted = translate_expected(expected).map_err(|error| error.to_string())?;
            let actual = node_expected(&source)?;
            if !same_expected(&wanted, &actual) || !expected_absent(path) {
                return Err("move source or destination changed".to_owned());
            }
            match fs::rename(&source, path) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                    let stage = path.with_file_name(format!(
                        ".{}.agentuna-move-stage-{}",
                        path.file_name().and_then(OsStr::to_str).unwrap_or("move"),
                        context.operation_id
                    ));
                    copy_node(&source, &stage)?;
                    if node_hash(&source)? != node_hash(&stage)? {
                        remove_node(&stage)?;
                        return Err("cross-device move verification failed".to_owned());
                    }
                    fs::rename(&stage, path).map_err(|error| error.to_string())?;
                    remove_node(&source)?;
                }
                Err(error) => {
                    return Err(format!(
                        "cannot move `{}` to `{}`: {error}",
                        source.display(),
                        path.display()
                    ));
                }
            }
            Ok(None)
        }
        _ => Err("unsupported operation".to_owned()),
    }
}

fn resolve_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("unsafe source path".to_owned());
    }
    Ok(root.join(path))
}

fn backup_meta(set: &ChangeSet, operation_id: &str, path: &Path, relative: &Path) -> BackupMeta {
    BackupMeta {
        scope: set.scope,
        relpath: Some(relative.to_string_lossy().into_owned()),
        operation_id: Some(operation_id.to_owned()),
        mode: fs::metadata(path)
            .ok()
            .map(|meta| std::os::unix::fs::MetadataExt::mode(&meta) & 0o7777),
    }
}

fn backup_folder(
    root: &Path,
    scope: Option<at_model::Scope>,
    relative: &Path,
) -> Result<PathBuf, String> {
    let scope = scope.ok_or_else(|| "backup scope is required".to_owned())?;
    let mut folder = root.join(scope.to_string());
    if let Some(parent) = relative.parent() {
        folder.push(parent);
    }
    fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    Ok(folder)
}

fn translate_expected(
    expected: &at_model::ExpectedState,
) -> Result<ExpectedState, ChangeSetApplyError> {
    match expected {
        at_model::ExpectedState::Absent => Ok(ExpectedState::Absent),
        at_model::ExpectedState::Hash { hash } => Ok(ExpectedState::Hash(hash.to_string())),
        at_model::ExpectedState::HashAndMtime {
            hash,
            mtime_unix_seconds,
        } => Ok(ExpectedState::HashAndMtime {
            hash: hash.to_string(),
            mtime: std::time::UNIX_EPOCH + std::time::Duration::from_secs(*mtime_unix_seconds),
        }),
        _ => Err(ChangeSetApplyError::UnsafePath(PathBuf::new())),
    }
}

fn same_expected(wanted: &ExpectedState, actual: &ExpectedState) -> bool {
    match (wanted, actual) {
        (ExpectedState::Absent, ExpectedState::Absent) => true,
        (ExpectedState::Hash(a), ExpectedState::Hash(b)) => a == b,
        (ExpectedState::HashAndMtime { hash: a, .. }, ExpectedState::Hash(b)) => a == b,
        _ => false,
    }
}

fn validate_expected(
    op: &FileOp,
    path: &Path,
    path_root: &Path,
    store_root: &Path,
    replaced_by_prior_move: bool,
) -> Result<(), ChangeSetApplyError> {
    let mut parent = path.parent();
    while let Some(current) = parent {
        if current == path_root {
            break;
        }
        if fs::symlink_metadata(current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(ChangeSetApplyError::UnsafePath(current.to_path_buf()));
        }
        parent = current.parent();
    }
    let valid = match op {
        FileOp::Write { expected, .. } | FileOp::Delete { expected, .. } => {
            expected_matches(path, expected)
        }
        // A Copy that replaces a just-deleted destination is the undo pipeline re-creating a
        // backed-up tree; the Delete immediately before it removed what was there.
        FileOp::Copy { expected, .. } | FileOp::CopyTree { expected, .. } => {
            expected_matches(path, expected)
                || (replaced_by_prior_move && matches!(expected, at_model::ExpectedState::Absent))
        }
        FileOp::Symlink { target, .. } => {
            (expected_absent(path) || replaced_by_prior_move)
                && (resolve_path(store_root, target).is_ok_and(|target| target.exists())
                    || replaced_by_prior_move)
        }
        FileOp::Mkdir { parents, .. } => {
            expected_absent(path) && (*parents || path.parent().is_some_and(Path::is_dir))
        }
        FileOp::Move { from, expected, .. } => {
            let source = store_root.join(from);
            expected_matches(&source, expected) && (expected_absent(path) || replaced_by_prior_move)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ChangeSetApplyError::Stale(path.to_path_buf()))
    }
}

fn expected_absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}
fn expected_matches(path: &Path, expected: &at_model::ExpectedState) -> bool {
    let Ok(wanted) = translate_expected(expected) else {
        return false;
    };
    matches!(node_expected(path), Ok(actual) if same_expected(&wanted, &actual))
}

/// Hashes a filesystem node using the same representation as ChangeSet preflight.
///
/// Files hash their bytes, symlinks hash their target text and directories hash their
/// sorted recursive entries. Missing paths return `Ok(None)`.
///
/// # Errors
/// Returns an IO error when the path cannot be inspected or read.
pub fn hash_node(path: &Path) -> Result<Option<String>, std::io::Error> {
    node_hash(path).map_err(std::io::Error::other)
}
fn node_expected(path: &Path) -> Result<ExpectedState, String> {
    match node_hash(path)? {
        Some(hash) => Ok(ExpectedState::Hash(hash)),
        None => Ok(ExpectedState::Absent),
    }
}

fn node_hash(path: &Path) -> Result<Option<String>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|error| error.to_string())?;
        return Ok(Some(sha256_hex(target.as_os_str().as_encoded_bytes())));
    }
    if metadata.is_file() {
        return fs::read(path)
            .map(|bytes| Some(sha256_hex(&bytes)))
            .map_err(|error| error.to_string());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .map(|entry| entry.map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bytes = Vec::new();
        for entry in entries {
            let name = entry.file_name();
            bytes.extend_from_slice(name.as_encoded_bytes());
            bytes.push(0);
            if let Some(hash) = node_hash(&entry.path())? {
                bytes.extend_from_slice(hash.as_bytes());
            }
            bytes.push(0xff);
        }
        return Ok(Some(sha256_hex(&bytes)));
    }
    Err(format!("unsupported filesystem node: {}", path.display()))
}

fn remove_node(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        fs::remove_file(path).map_err(|error| error.to_string())
    }
}

fn copy_node(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::create_dir(destination).map_err(|error| error.to_string())?;
        for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            copy_node(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                fs::read_link(source).map_err(|error| error.to_string())?,
                destination,
            )
            .map_err(|error| error.to_string())
        }
        #[cfg(not(unix))]
        {
            Err("symlink copy is unsupported on this platform".to_owned())
        }
    } else {
        fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletedOperation {
    index: usize,
    prior: Option<Vec<u8>>,
    delete_backup: Option<PathBuf>,
}
fn rollback(
    set: &ChangeSet,
    root: &Path,
    done: &[CompletedOperation],
    paths: &[PathBuf],
    entries: &[JournalEntry],
    created_dirs: &[PathBuf],
) -> Result<(), String> {
    for CompletedOperation {
        index,
        prior: before,
        delete_backup,
    } in done.iter().rev()
    {
        let path = paths
            .get(*index)
            .ok_or_else(|| "rollback path missing".to_owned())?;
        let after = entries
            .get(*index)
            .ok_or_else(|| "journal entry missing".to_owned())?
            .after_hash
            .as_str();
        let current_hash = node_hash(path)?;
        if current_hash.as_deref() != (!after.is_empty()).then_some(after) {
            return Err(format!(
                "target changed during rollback: {}",
                path.display()
            ));
        }
        if matches!(set.ops.get(*index), Some(FileOp::Delete { .. })) {
            let backup = delete_backup
                .as_deref()
                .ok_or_else(|| "delete backup missing during rollback".to_owned())?;
            if !expected_absent(path) {
                return Err(format!(
                    "delete target changed during rollback: {}",
                    path.display()
                ));
            }
            fs::rename(backup, path).map_err(|error| error.to_string())?;
            continue;
        }
        if let Some(FileOp::Move { from, .. }) = set.ops.get(*index) {
            let source = resolve_path(root, from)?;
            if !expected_absent(&source) {
                return Err(format!(
                    "move source changed during rollback: {}",
                    source.display()
                ));
            }
            fs::rename(path, &source).map_err(|error| error.to_string())?;
            continue;
        }
        match before {
            Some(bytes) => {
                let expected = if after.is_empty() {
                    ExpectedState::Absent
                } else {
                    ExpectedState::Hash(after.to_owned())
                };
                apply_write(&WriteRequest::replace(path, bytes.clone(), expected))
                    .map_err(|error| error.to_string())?;
            }
            None => match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => remove_node(path)?,
                Err(error) => return Err(error.to_string()),
            },
        }
    }
    for directory in created_dirs {
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

struct RollbackState<'a> {
    set: &'a ChangeSet,
    id: &'a str,
    journal_path: &'a Path,
    done: &'a [CompletedOperation],
    paths: &'a [PathBuf],
    entries: &'a [JournalEntry],
    created_dirs: &'a [PathBuf],
}

fn fail_rollback(
    context: &mut ApplyContext<'_>,
    state: RollbackState<'_>,
    cause: String,
) -> Result<ApplyOutcome, ChangeSetApplyError> {
    let rolled_back = rollback(
        state.set,
        context.store_root,
        state.done,
        state.paths,
        state.entries,
        state.created_dirs,
    );
    let _ = context
        .recorder
        .finish(state.id, false, rolled_back.is_err());
    if rolled_back.is_ok() {
        let _ = fs::remove_file(state.journal_path);
    }
    Err(ChangeSetApplyError::Apply {
        cause,
        rollback: rolled_back.err().unwrap_or_else(|| "complete".to_owned()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::failpoint::{FailInjector, Fault, FaultPoint};
    use at_model::{ExpectedState as ModelExpected, OperationKind, Scope};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::TempDir;

    #[derive(Default)]
    struct Recorder {
        starts: usize,
        completed: usize,
        finish: Vec<(bool, bool)>,
    }
    impl OperationRecorder for Recorder {
        fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
            self.starts += 1;
            Ok(format!("op-{}", self.starts))
        }
        fn file_completed(
            &mut self,
            _: &str,
            _: &Path,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&Path>,
        ) -> Result<(), String> {
            self.completed += 1;
            Ok(())
        }
        fn finish(&mut self, _: &str, ok: bool, review: bool) -> Result<(), String> {
            self.finish.push((ok, review));
            Ok(())
        }
    }
    struct FailThird(AtomicUsize);
    impl FailInjector for FailThird {
        fn fault(&self, point: FaultPoint, _: &Path) -> Option<Fault> {
            (point == FaultPoint::TempWrite && self.0.fetch_add(1, Ordering::SeqCst) == 2)
                .then_some(Fault::Fail(libc::EIO))
        }
    }
    fn changeset(ops: Vec<FileOp>) -> ChangeSet {
        ChangeSet {
            id: "cs-test".to_owned(),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse("edit_rule").expect("known operation"),
            scope: Some(Scope::Project),
            project_id: None,
            summary: "test".to_owned(),
            ops,
            created_at_unix_seconds: 0,
        }
    }
    #[test]
    fn third_write_failure_rolls_back_first_two_and_leaves_rest_untouched() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        let backups = root.join("backups").join("20261106T120000Z-op-1");
        fs::create_dir_all(&backups).expect("backup root");
        let mut operations = Vec::new();
        let mut paths = Vec::new();
        for index in 0..5 {
            let relative = PathBuf::from(format!("f{index}.txt"));
            let full = root.join(&relative);
            fs::create_dir_all(&root).expect("store root");
            let before = format!("before-{index}");
            fs::write(&full, &before).expect("seed");
            operations.push(FileOp::Write {
                path: relative,
                bytes: format!("after-{index}").into_bytes(),
                expected: ModelExpected::hash(sha256_hex(before.as_bytes())).expect("valid hash"),
                follow_symlink: false,
                backup: true,
            });
            paths.push(full);
        }
        let set = changeset(operations);
        let mut recorder = Recorder::default();
        let mut context = ApplyContext {
            store_root: &root,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let injector = Arc::new(FailThird(AtomicUsize::new(0)));
        let result = crate::failpoint::with_injector(injector, || apply(&set, &mut context));
        assert!(
            matches!(result, Err(ChangeSetApplyError::Apply { .. })),
            "{result:?}"
        );
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                fs::read(path).expect("file"),
                format!("before-{index}").as_bytes()
            );
        }
        assert_eq!(recorder.starts, 1);
        assert_eq!(recorder.completed, 2);
        assert_eq!(recorder.finish, [(false, false)]);
    }
    struct FailSecondChangeSetOp(AtomicUsize);
    impl FailInjector for FailSecondChangeSetOp {
        fn fault(&self, point: FaultPoint, _: &Path) -> Option<Fault> {
            (point == FaultPoint::ChangeSetOperation && self.0.fetch_add(1, Ordering::SeqCst) == 1)
                .then_some(Fault::Fail(libc::EIO))
        }
    }

    fn fail_after_one_operation(root: &Path, first: FileOp) {
        fs::create_dir_all(root).expect("store");
        let second = FileOp::Mkdir {
            path: PathBuf::from("never-created"),
            mode: None,
            parents: false,
        };
        let set = changeset(vec![first, second]);
        let mut recorder = Recorder::default();
        let backup_root = root.join("backups");
        let mut context = ApplyContext {
            store_root: root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };
        let injector = Arc::new(FailSecondChangeSetOp(AtomicUsize::new(0)));
        let result = crate::failpoint::with_injector(injector, || apply(&set, &mut context));
        assert!(
            matches!(result, Err(ChangeSetApplyError::Apply { .. })),
            "{result:?}"
        );
    }
    struct FailAtChangeSetOp {
        current: AtomicUsize,
        fail_at: usize,
    }
    impl FailInjector for FailAtChangeSetOp {
        fn fault(&self, point: FaultPoint, _: &Path) -> Option<Fault> {
            (point == FaultPoint::ChangeSetOperation
                && self.current.fetch_add(1, Ordering::SeqCst) == self.fail_at)
                .then_some(Fault::Fail(libc::EIO))
        }
    }

    #[test]
    fn directory_move_delete_and_write_failures_restore_original_trees() {
        for fail_at in 1..=2 {
            let sandbox = TempDir::new().expect("tempdir");
            let root = sandbox.path().join("store");
            let moved = root.join("move-source");
            let deleted = root.join("delete-source");
            fs::create_dir_all(moved.join("nested")).expect("move source");
            fs::create_dir_all(deleted.join("nested")).expect("delete source");
            fs::write(moved.join("nested/file"), b"move bytes").expect("move bytes");
            fs::write(deleted.join("nested/file"), b"delete bytes").expect("delete bytes");
            fs::write(root.join("write-target"), b"before write").expect("write target");
            let before_move = node_hash(&moved).expect("move hash").expect("move exists");
            let before_delete = node_hash(&deleted)
                .expect("delete hash")
                .expect("delete exists");
            let set = changeset(vec![
                FileOp::Move {
                    from: PathBuf::from("move-source"),
                    to: PathBuf::from("move-destination"),
                    expected: ModelExpected::hash(before_move.clone()).expect("move expected hash"),
                },
                FileOp::Delete {
                    path: PathBuf::from("delete-source"),
                    expected: ModelExpected::hash(before_delete.clone())
                        .expect("delete expected hash"),
                },
                FileOp::Write {
                    path: PathBuf::from("write-target"),
                    bytes: b"after write".to_vec(),
                    expected: ModelExpected::hash(sha256_hex(b"before write"))
                        .expect("write expected hash"),
                    follow_symlink: false,
                    backup: false,
                },
            ]);
            let backup_root = root.join("backups");
            let mut recorder = Recorder::default();
            let mut context = ApplyContext {
                store_root: &root,
                backup_folder: &backup_root,
                recorder: &mut recorder,
            };
            let injector = Arc::new(FailAtChangeSetOp {
                current: AtomicUsize::new(0),
                fail_at,
            });
            let result = crate::failpoint::with_injector(injector, || apply(&set, &mut context));
            assert!(matches!(result, Err(ChangeSetApplyError::Apply { .. })));
            assert_eq!(
                node_hash(&moved).expect("post move hash").as_deref(),
                Some(before_move.as_str())
            );
            assert_eq!(
                node_hash(&deleted).expect("post delete hash").as_deref(),
                Some(before_delete.as_str())
            );
            assert_eq!(
                fs::read(root.join("write-target")).expect("restored write"),
                b"before write"
            );
            assert!(!root.join("move-destination").exists());
            assert!(!backup_root.join("op-1.deleted").exists());
            assert!(
                !fs::read_dir(&root)
                    .expect("store listing")
                    .filter_map(Result::ok)
                    .any(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .contains("agentuna-move-stage"))
            );
        }
    }
    #[derive(Debug)]
    struct RejectNativeDirectory;
    impl crate::guard::OverwriteGuard for RejectNativeDirectory {
        fn check(&self, path: &Path, _: bool) -> Result<(), crate::FsError> {
            Err(crate::FsError::RefusedOverwrite {
                path: path.to_path_buf(),
                kind: crate::guard::TargetKind::Dir,
                existing_hash: None,
                remedies: crate::guard::REMEDIES.to_vec(),
            })
        }
    }

    #[test]
    fn directory_delete_requires_ownership_guard_before_mutation() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        let native = sandbox.path().join("native");
        fs::create_dir_all(native.join("nested")).expect("native directory");
        fs::write(native.join("nested/item"), b"native bytes").expect("native file");
        fs::create_dir_all(&root).expect("store");
        let expected = node_hash(&native)
            .expect("directory hash")
            .expect("directory exists");
        let set = changeset(vec![FileOp::Delete {
            path: native.clone(),
            expected: ModelExpected::hash(expected).expect("valid hash"),
        }]);
        let roots = ApplyRoots::new([sandbox.path().to_path_buf()]).expect("authorized root");
        let backup_root = root.join("backups");
        let mut recorder = Recorder::default();
        let mut context = ApplyContext {
            store_root: &root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let result = apply_with_roots_and_guard(&set, &mut context, &roots, &RejectNativeDirectory);

        assert!(matches!(
            result,
            Err(ChangeSetApplyError::Fs(
                crate::FsError::RefusedOverwrite { .. }
            ))
        ));
        assert_eq!(
            fs::read(native.join("nested/item")).expect("native file remains"),
            b"native bytes"
        );
        assert_eq!(recorder.starts, 0);
    }

    #[cfg(unix)]
    #[test]
    fn deleting_a_deployment_symlink_never_deletes_its_target() {
        use std::os::unix::fs::symlink;

        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(&root).expect("store");
        let target = root.join("canonical");
        let deployment = root.join("deployment");
        fs::write(&target, b"canonical bytes").expect("target");
        symlink(&target, &deployment).expect("deployment link");
        let link_hash = node_hash(&deployment)
            .expect("link hash")
            .expect("link exists");
        let set = changeset(vec![FileOp::Delete {
            path: PathBuf::from("deployment"),
            expected: ModelExpected::hash(link_hash).expect("valid link hash"),
        }]);
        let backup_root = root.join("backups");
        let mut recorder = Recorder::default();
        let mut context = ApplyContext {
            store_root: &root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        apply(&set, &mut context).expect("delete symlink deployment");

        assert!(fs::symlink_metadata(&deployment).is_err());
        assert_eq!(
            fs::read(&target).expect("canonical target"),
            b"canonical bytes"
        );
    }
    #[test]
    fn symlink_failure_rolls_back_created_link() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(root.join("source")).expect("source");
        fail_after_one_operation(
            &root,
            FileOp::Symlink {
                link_path: PathBuf::from("link"),
                target: PathBuf::from("source"),
                mode: at_model::DeployMode::Link,
            },
        );
        assert!(fs::symlink_metadata(root.join("link")).is_err());
    }

    #[test]
    fn copied_directory_failure_rolls_back_tree() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(root.join("source")).expect("source");
        fs::write(root.join("source/file.txt"), b"source").expect("source file");
        fail_after_one_operation(
            &root,
            FileOp::Copy {
                source: PathBuf::from("source"),
                dest: PathBuf::from("copy"),
                expected: ModelExpected::Absent,
            },
        );
        assert!(fs::symlink_metadata(root.join("copy")).is_err());
        assert_eq!(
            fs::read(root.join("source/file.txt")).expect("source"),
            b"source"
        );
    }

    #[test]
    fn copy_tree_writes_plain_content_without_a_deploy_manifest() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(root.join("source/nested")).expect("source");
        fs::write(root.join("source/SKILL.md"), b"canonical").expect("skill doc");
        fs::write(root.join("source/nested/helper.txt"), b"helper").expect("nested file");
        let set = changeset(vec![FileOp::CopyTree {
            source: PathBuf::from("source"),
            dest: PathBuf::from("item"),
            expected: ModelExpected::Absent,
        }]);
        let backup_root = root.join("backups");
        let mut recorder = Recorder::default();
        let mut context = ApplyContext {
            store_root: &root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        apply(&set, &mut context).expect("copy tree");

        assert_eq!(
            fs::read(root.join("item/SKILL.md")).expect("skill doc"),
            b"canonical"
        );
        assert_eq!(
            fs::read(root.join("item/nested/helper.txt")).expect("nested file"),
            b"helper"
        );
        assert!(
            fs::symlink_metadata(root.join("item").join(crate::deploy::COPY_MANIFEST_FILE))
                .is_err(),
            "a store item carries no deployment manifest"
        );
        assert_eq!(
            hash_node(&root.join("item")).expect("item hash"),
            hash_node(&root.join("source")).expect("source hash"),
            "the copy hashes exactly as the source does"
        );
    }

    #[test]
    fn copy_tree_failure_rolls_back_the_whole_tree() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(root.join("source")).expect("source");
        fs::write(root.join("source/file.txt"), b"source").expect("source file");
        fail_after_one_operation(
            &root,
            FileOp::CopyTree {
                source: PathBuf::from("source"),
                dest: PathBuf::from("item"),
                expected: ModelExpected::Absent,
            },
        );
        assert!(fs::symlink_metadata(root.join("item")).is_err());
        assert_eq!(
            fs::read(root.join("source/file.txt")).expect("source"),
            b"source"
        );
    }

    #[test]
    fn move_failure_restores_original_name() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join("before"), b"content").expect("source");
        fail_after_one_operation(
            &root,
            FileOp::Move {
                from: PathBuf::from("before"),
                to: PathBuf::from("after"),
                expected: ModelExpected::hash(sha256_hex(b"content")).expect("valid hash"),
            },
        );
        assert_eq!(fs::read(root.join("before")).expect("restored"), b"content");
        assert!(fs::symlink_metadata(root.join("after")).is_err());
    }

    #[test]
    fn mkdir_failure_removes_created_directory() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fail_after_one_operation(
            &root,
            FileOp::Mkdir {
                path: PathBuf::from("created"),
                mode: Some(0o700),
                parents: false,
            },
        );
        assert!(fs::symlink_metadata(root.join("created")).is_err());
    }

    #[test]
    fn reversible_apply_restores_the_pre_apply_file_tree() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path().join("store");
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join("item.txt"), b"before").expect("seed file");
        let set = changeset(vec![FileOp::Write {
            path: PathBuf::from("item.txt"),
            bytes: b"after".to_vec(),
            expected: ModelExpected::hash(sha256_hex(b"before")).expect("valid hash"),
            follow_symlink: false,
            backup: true,
        }]);
        let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
        let backup_root = root.join("backups");
        let mut recorder = Recorder::default();
        let mut context = ApplyContext {
            store_root: &root,
            backup_folder: &backup_root,
            recorder: &mut recorder,
        };

        let applied =
            apply_with_roots_reversible(&set, &mut context, &roots).expect("reversible apply");
        assert_eq!(fs::read(root.join("item.txt")).expect("after"), b"after");
        let handle = applied.rollback_handle.expect("rollback token");
        rollback_committed(handle).expect("rollback");
        assert_eq!(
            fs::read(root.join("item.txt")).expect("restored"),
            b"before"
        );
    }

    #[test]
    fn public_node_hash_matches_change_set_preflight_for_files_and_directories() {
        let sandbox = TempDir::new().expect("tempdir");
        let root = sandbox.path();
        fs::create_dir_all(root.join("directory")).expect("directory");
        fs::write(root.join("directory/file"), b"content").expect("file");

        assert_eq!(
            hash_node(&root.join("directory/file")).expect("file hash"),
            Some(sha256_hex(b"content"))
        );
        assert!(
            hash_node(&root.join("directory"))
                .expect("directory hash")
                .is_some()
        );
        assert_eq!(
            hash_node(&root.join("missing")).expect("missing path"),
            None
        );
    }
}
