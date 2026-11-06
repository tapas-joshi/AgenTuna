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
            let size = fs::metadata(blob).map_err(|error| error.to_string())?.len();
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
    apply_inner(set, context, None)
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
    apply_inner(set, context, Some(roots))
}

fn apply_inner(
    set: &ChangeSet,
    context: &mut ApplyContext<'_>,
    authorized_roots: Option<&ApplyRoots>,
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
    for ((op, path), root) in set.ops.iter().zip(&paths).zip(&path_roots) {
        validate_expected(op, path, root)?;
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
        .map(|(op, path)| JournalEntry {
            path: path.clone(),
            before_hash: fs::read(path).ok().map(|bytes| sha256_hex(&bytes)),
            after_hash: match op {
                FileOp::Write { bytes, .. } => sha256_hex(bytes),
                FileOp::Delete { .. } => String::new(),
                _ => String::new(),
            },
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
        let prior = fs::read(path).ok();
        let backup_path = match execute_op(
            op,
            path,
            backup_relative,
            prior.as_deref(),
            &ExecuteContext {
                set,
                operation_id: &operation_id,
                store_root: context.store_root,
                backup_root: &backup_folder,
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
                    cause,
                );
            }
        };
        let before_hash = prior.as_ref().map(|bytes| sha256_hex(bytes));
        let after_hash = fs::read(path).ok().map(|bytes| sha256_hex(&bytes));
        if let Some(entry) = journal.entries.get_mut(index) {
            entry.after_hash = after_hash.clone().unwrap_or_default();
        }
        done.push((index, prior));
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
    fs::remove_file(&journal_path)
        .map_err(|error| ChangeSetApplyError::Journal(error.to_string()))?;
    Ok(ApplyOutcome {
        operation_id,
        completed: done.len(),
    })
}

struct ExecuteContext<'a> {
    set: &'a ChangeSet,
    operation_id: &'a str,
    store_root: &'a Path,
    backup_root: &'a Path,
}

fn execute_op(
    op: &FileOp,
    path: &Path,
    backup_relative: &Path,
    prior: Option<&[u8]>,
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
            let actual = prior
                .map(|bytes| ExpectedState::Hash(sha256_hex(bytes)))
                .unwrap_or(ExpectedState::Absent);
            if !same_expected(&wanted, &actual) {
                return Err(format!("stale target: {}", path.display()));
            }
            let bytes =
                prior.ok_or_else(|| format!("delete target is absent: {}", path.display()))?;
            let folder = backup_folder(context.backup_root, context.set.scope, backup_relative)?;
            let files = crate::backup::copy_into(
                &folder,
                path,
                bytes,
                &backup_meta(context.set, context.operation_id, path, backup_relative),
                OffsetDateTime::now_utc(),
            )
            .map_err(|error| error.to_string())?;
            fs::remove_file(path).map_err(|error| error.to_string())?;
            Ok(Some(files.blob))
        }
        FileOp::Symlink { target, mode, .. } => {
            let source = resolve_path(context.store_root, target)?;
            let link_mode = match mode {
                at_model::DeployMode::Link => at_model::LinkMode::Link,
                at_model::DeployMode::Copy => at_model::LinkMode::Copy,
                _ => return Err("unsupported link mode".to_owned()),
            };
            crate::deploy::link_dir(&source, path, link_mode).map_err(|error| error.to_string())?;
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
            let wanted = translate_expected(expected).map_err(|error| error.to_string())?;
            let actual = fs::read(&source)
                .map(|bytes| ExpectedState::Hash(sha256_hex(&bytes)))
                .unwrap_or(ExpectedState::Absent);
            if !same_expected(&wanted, &actual) || !expected_absent(path) {
                return Err("move source or destination changed".to_owned());
            }
            fs::rename(&source, path).map_err(|error| error.to_string())?;
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
        at_model::ExpectedState::HashAndMtime { hash, .. } => {
            Ok(ExpectedState::Hash(hash.to_string()))
        }
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
    store_root: &Path,
) -> Result<(), ChangeSetApplyError> {
    let mut parent = path.parent();
    while let Some(current) = parent {
        if current == store_root {
            break;
        }
        if fs::symlink_metadata(current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(ChangeSetApplyError::UnsafePath(current.to_path_buf()));
        }
        parent = current.parent();
    }
    let valid = match op {
        FileOp::Write { expected, .. }
        | FileOp::Delete { expected, .. }
        | FileOp::Copy { expected, .. } => expected_matches(path, expected),
        FileOp::Symlink { target, .. } => {
            expected_absent(path)
                && resolve_path(store_root, target).is_ok_and(|target| target.exists())
        }
        FileOp::Mkdir { parents, .. } => {
            expected_absent(path) && (*parents || path.parent().is_some_and(Path::is_dir))
        }
        FileOp::Move { from, expected, .. } => {
            let source = store_root.join(from);
            expected_matches(&source, expected) && expected_absent(path)
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
    let Ok(expected) = translate_expected(expected) else {
        return false;
    };
    match (expected, fs::symlink_metadata(path)) {
        (ExpectedState::Absent, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => true,
        (ExpectedState::Hash(hash), Ok(meta)) if meta.is_file() => {
            fs::read(path).is_ok_and(|bytes| sha256_hex(&bytes) == hash)
        }
        (ExpectedState::HashAndMtime { hash, mtime }, Ok(meta)) if meta.is_file() => fs::read(path)
            .is_ok_and(|bytes| sha256_hex(&bytes) == hash && meta.modified().ok() == Some(mtime)),
        _ => false,
    }
}

fn rollback(
    set: &ChangeSet,
    root: &Path,
    done: &[(usize, Option<Vec<u8>>)],
    paths: &[PathBuf],
    entries: &[JournalEntry],
    created_dirs: &[PathBuf],
) -> Result<(), String> {
    for (index, before) in done.iter().rev() {
        let path = paths
            .get(*index)
            .ok_or_else(|| "rollback path missing".to_owned())?;
        let after = entries
            .get(*index)
            .ok_or_else(|| "journal entry missing".to_owned())?
            .after_hash
            .as_str();
        let current = fs::read(path).ok();
        if after.is_empty() {
            if current.is_some() {
                return Err(format!(
                    "target changed during rollback: {}",
                    path.display()
                ));
            }
        } else if current.as_ref().map(|bytes| sha256_hex(bytes)).as_deref() != Some(after) {
            return Err(format!(
                "target changed during rollback: {}",
                path.display()
            ));
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
                Ok(meta) if meta.is_dir() => {
                    if matches!(set.ops.get(*index), Some(FileOp::Copy { .. })) {
                        fs::remove_dir_all(path).map_err(|error| error.to_string())?;
                    } else {
                        fs::remove_dir(path).map_err(|error| error.to_string())?;
                    }
                }
                Ok(_) => fs::remove_file(path).map_err(|error| error.to_string())?,
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
    done: &'a [(usize, Option<Vec<u8>>)],
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
}
