//! Restore a catalogue backup into a caller-supplied store root.

use std::fs;
use std::path::{Component, Path, PathBuf};

use at_db::{backups, ops};
use rusqlite::Connection;
use time::OffsetDateTime;

use crate::backup::{self, BackupMeta};
use crate::hash::sha256_hex;

/// Inputs needed to restore one catalogue entry.
#[derive(Debug, Clone)]
pub struct RestoreRequest<'a> {
    /// Database containing the backup catalogue and operation history.
    pub connection: &'a Connection,
    /// Store directory that bounds the restored target.
    pub store_root: &'a Path,
    /// Catalogue backup id.
    pub backup_id: &'a str,
    /// Token from a previous restore call, required when target content differs.
    pub confirmation: Option<&'a str>,
    /// Actor recorded in history.
    pub actor: ops::OperationActor,
}

/// Outcome of requesting a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RestoreOutcome {
    /// Changed target needs explicit confirmation; no writes were made.
    ConfirmationRequired {
        /// Token bound to the backup and observed current hash.
        token: String,
        /// Diff of current content to restored content.
        diff: String,
    },
    /// Backup bytes were restored.
    Restored {
        /// History operation id.
        operation_id: String,
        /// Target path restored.
        target: PathBuf,
    },
}

/// Restore refusal or failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreError {
    /// Catalogue id does not exist.
    #[error("backup {0} was not found")]
    NotFound(String),
    /// Catalogue row lacks a usable original or blob path/hash.
    #[error("backup catalogue row is incomplete: {0}")]
    InvalidRow(String),
    /// Target escaped the supplied store root.
    #[error("backup target is outside the supplied store root")]
    OutsideRoot,
    /// Backup blob or sidecar failed verification.
    #[error(transparent)]
    Backup(#[from] backup::BackupError),
    /// Database operation failed.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Filesystem operation failed.
    #[error("restore IO failed at {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Confirmation is absent or bound to a different observed target hash.
    #[error("restore confirmation is missing or stale")]
    StaleConfirmation,
    /// Restored bytes failed post-write verification.
    #[error("restored target hash does not match backup")]
    VerificationFailed,
    /// The atomic write was refused because target changed.
    #[error(transparent)]
    Fs(#[from] crate::FsError),
}

fn io(path: &Path, source: std::io::Error) -> RestoreError {
    RestoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn target_path(root: &Path, original: &str) -> Result<PathBuf, RestoreError> {
    let original = Path::new(original);
    let relative = if original.is_absolute() {
        original
            .strip_prefix(root)
            .map_err(|_| RestoreError::OutsideRoot)?
    } else {
        original
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)) && !matches!(part, Component::CurDir))
    {
        return Err(RestoreError::OutsideRoot);
    }
    Ok(root.join(relative))
}

fn reject_symlink_parents(root: &Path, target: &Path) -> Result<(), RestoreError> {
    let mut parent = target.parent();
    while let Some(path) = parent {
        if path == root {
            break;
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RestoreError::OutsideRoot);
            }
            Ok(metadata) if !metadata.is_dir() => return Err(RestoreError::OutsideRoot),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(path, error)),
            _ => {}
        }
        parent = path.parent();
    }
    Ok(())
}

fn token(backup_id: &str, observed: &str) -> String {
    sha256_hex(format!("{backup_id}\0{observed}").as_bytes())
}

fn read_current(target: &Path) -> Result<Option<Vec<u8>>, RestoreError> {
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(target, error)),
        Ok(metadata) if metadata.is_file() => crate::symlink::read_nofollow(target)
            .map(Some)
            .map_err(RestoreError::Fs),
        Ok(_) => Err(RestoreError::InvalidRow(
            "restore target is not a regular file".to_owned(),
        )),
    }
}

fn restore_tree(
    request: RestoreRequest<'_>,
    original: &str,
    blob: &Path,
    expected_hash: &str,
) -> Result<RestoreOutcome, RestoreError> {
    let target = target_path(request.store_root, original)?;
    reject_symlink_parents(request.store_root, &target)?;
    if crate::changeset::hash_node(blob)
        .map_err(|error| io(blob, error))?
        .as_deref()
        != Some(expected_hash)
    {
        return Err(RestoreError::VerificationFailed);
    }
    let current_hash = crate::changeset::hash_node(&target).map_err(|error| io(&target, error))?;
    if current_hash
        .as_deref()
        .is_some_and(|hash| hash != expected_hash)
    {
        let observed = current_hash
            .as_deref()
            .ok_or(RestoreError::StaleConfirmation)?;
        let expected_token = token(request.backup_id, observed);
        if request.confirmation != Some(expected_token.as_str()) {
            return if request.confirmation.is_none() {
                Ok(RestoreOutcome::ConfirmationRequired {
                    token: expected_token,
                    diff: "Directory contents differ; review before restoring.".to_owned(),
                })
            } else {
                Err(RestoreError::StaleConfirmation)
            };
        }
    } else if request.confirmation.is_some() {
        return Err(RestoreError::StaleConfirmation);
    }

    let parent = target.parent().ok_or(RestoreError::OutsideRoot)?;
    fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
    let operation = ops::begin(
        request.connection,
        &ops::NewOperation::new(ops::OperationKind::Restore, request.actor)
            .with_summary("Restored directory backup"),
    )?;
    let operation_text = operation.to_string();
    let operation_folder = backup::operation_backup_dir(
        &request.store_root.join("backups"),
        OffsetDateTime::now_utc(),
        &operation_text,
    )?;
    fs::create_dir_all(&operation_folder).map_err(|error| io(&operation_folder, error))?;
    let stage = parent.join(format!(".agentuna-restore-stage-{operation_text}"));
    copy_tree(blob, &stage).map_err(|error| io(&stage, error))?;
    if crate::changeset::hash_node(&stage)
        .map_err(|error| io(&stage, error))?
        .as_deref()
        != Some(expected_hash)
    {
        remove_tree(&stage).map_err(|error| io(&stage, error))?;
        return Err(RestoreError::VerificationFailed);
    }

    let before_hash = crate::changeset::hash_node(&target).map_err(|error| io(&target, error))?;
    if before_hash != current_hash {
        remove_tree(&stage).map_err(|error| io(&stage, error))?;
        return Err(RestoreError::StaleConfirmation);
    }
    let mut current_backup_id = None;
    if current_hash.is_some() {
        let current_backup = operation_folder.join("current-tree");
        fs::rename(&target, &current_backup).map_err(|error| io(&target, error))?;
        let moved_hash = crate::changeset::hash_node(&current_backup)
            .map_err(|error| io(&current_backup, error))?;
        if moved_hash != current_hash {
            fs::rename(&current_backup, &target).map_err(|error| io(&target, error))?;
            remove_tree(&stage).map_err(|error| io(&stage, error))?;
            return Err(RestoreError::StaleConfirmation);
        }
        let current_text = current_backup.to_string_lossy().into_owned();
        let original_text = target.to_string_lossy().into_owned();
        let current_digest = current_hash.as_deref().unwrap_or_default();
        let size = fs::metadata(&current_backup)
            .map_err(|error| io(&current_backup, error))?
            .len();
        let row = backups::NewBackup::new(&original_text, &current_text)
            .with_operation(&operation_text)
            .with_content(Some(current_digest), i64::try_from(size).ok());
        current_backup_id = Some(backups::record_backup(request.connection, &row)?);
    }
    if let Err(error) = fs::rename(&stage, &target) {
        if let Some(current_hash) = current_hash.as_ref() {
            let current_backup = operation_folder.join("current-tree");
            if crate::changeset::hash_node(&current_backup)
                .ok()
                .flatten()
                .as_ref()
                == Some(current_hash)
            {
                let _ = fs::rename(&current_backup, &target);
            }
        }
        remove_tree(&stage).ok();
        return Err(io(&target, error));
    }
    let after_hash = crate::changeset::hash_node(&target)
        .map_err(|error| io(&target, error))?
        .ok_or(RestoreError::VerificationFailed)?;
    if after_hash != expected_hash {
        return Err(RestoreError::VerificationFailed);
    }
    let path_text = target.to_string_lossy().into_owned();
    let mut touched = ops::TouchedFile::new(
        &path_text,
        if current_hash.is_some() {
            ops::FileAction::Modified
        } else {
            ops::FileAction::Created
        },
    )
    .with_hashes(current_hash.as_deref(), Some(&after_hash));
    touched.backup_id = current_backup_id.as_deref();
    ops::attach_file(request.connection, &operation, &touched)?;
    ops::finish(
        request.connection,
        &operation,
        ops::OperationStatus::Succeeded,
        None,
    )?;
    backups::mark_restored(request.connection, request.backup_id)?;
    Ok(RestoreOutcome::Restored {
        operation_id: operation_text,
        target,
    })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(fs::read_link(source)?, destination);
        }
        #[cfg(not(unix))]
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "directory backup symlinks are unsupported",
            ));
        }
    }
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory backup contains a special file",
        ))
    }
}

fn remove_tree(path: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Verifies and restores a catalogue backup, requiring confirmation before overwriting changed bytes.
///
/// The token is bound to the selected backup and current target hash. The target is re-read before
/// atomic installation, and any concurrent edit is refused. A restore first backs up the current
/// target into a timestamped operation folder and records the restore in history.
///
/// # Errors
/// Returns `NotFound`, `InvalidRow`, `OutsideRoot`, `StaleConfirmation`, `VerificationFailed`, or
/// the underlying backup, database, filesystem, or atomic-write error.
pub fn restore_backup(request: RestoreRequest<'_>) -> Result<RestoreOutcome, RestoreError> {
    let row = backups::backup_by_id(request.connection, request.backup_id)?
        .ok_or_else(|| RestoreError::NotFound(request.backup_id.to_owned()))?;
    let original = row
        .original_path
        .as_deref()
        .ok_or_else(|| RestoreError::InvalidRow("missing original_path".to_owned()))?;
    let blob = row
        .backup_path
        .as_deref()
        .ok_or_else(|| RestoreError::InvalidRow("missing backup_path".to_owned()))?;
    let expected_hash = row
        .content_hash
        .as_deref()
        .ok_or_else(|| RestoreError::InvalidRow("missing content_hash".to_owned()))?;
    if Path::new(blob).is_dir() {
        return restore_tree(request, original, Path::new(blob), expected_hash);
    }
    let sidecar = backup::verify(Path::new(blob), expected_hash)?;
    let bytes = fs::read(blob).map_err(|error| io(Path::new(blob), error))?;
    let target = target_path(request.store_root, original)?;
    reject_symlink_parents(request.store_root, &target)?;
    let current = read_current(&target)?;
    let current_hash = current.as_deref().map(sha256_hex);
    if let Some(current_hash) = current_hash.as_deref()
        && current_hash != expected_hash
    {
        let expected_token = token(request.backup_id, current_hash);
        if let Some(confirmation) = request.confirmation {
            if confirmation != expected_token {
                return Err(RestoreError::StaleConfirmation);
            }
        } else {
            let diff = crate::diff::unified(original, current.as_deref(), &bytes);
            return Ok(RestoreOutcome::ConfirmationRequired {
                token: expected_token,
                diff,
            });
        }
    } else if request.confirmation.is_some() {
        return Err(RestoreError::StaleConfirmation);
    }

    let now = OffsetDateTime::now_utc();
    let mut new_operation = ops::NewOperation::new(ops::OperationKind::Restore, request.actor)
        .with_summary("Restored backup");
    new_operation.scope = sidecar.scope;
    let operation = ops::begin(request.connection, &new_operation)?;
    let operation_text = operation.to_string();
    let backups_root = request.store_root.join("backups");
    let operation_folder = backup::operation_backup_dir(&backups_root, now, &operation_text)?;
    fs::create_dir_all(&operation_folder).map_err(|error| io(&operation_folder, error))?;
    let mut current_backup_id = None;
    if let Some(current_bytes) = current.as_deref() {
        let metadata = fs::symlink_metadata(&target).map_err(|error| io(&target, error))?;
        let mode = std::os::unix::fs::MetadataExt::mode(&metadata) & 0o7777;
        let relative = target
            .strip_prefix(request.store_root)
            .map_err(|_| RestoreError::OutsideRoot)?;
        let current_backup = backup::copy_into(
            &operation_folder,
            &target,
            current_bytes,
            &BackupMeta {
                scope: sidecar.scope,
                relpath: Some(relative.to_string_lossy().into_owned()),
                operation_id: Some(operation_text.clone()),
                mode: Some(mode),
            },
            now,
        )?;
        let current_hash = sha256_hex(current_bytes);
        let blob_path = current_backup.blob.to_string_lossy().into_owned();
        let size = i64::try_from(current_bytes.len()).ok();
        let created = (now + time::Duration::days(30))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| RestoreError::InvalidRow(error.to_string()))?;
        let backup_row = backups::NewBackup::new(original, &blob_path)
            .with_operation(&operation_text)
            .with_content(Some(&current_hash), size);
        let mut backup_row = backup_row;
        backup_row.expires_at = Some(&created);
        current_backup_id = Some(backups::record_backup(request.connection, &backup_row)?);
    }

    let expected = current_hash
        .clone()
        .map_or_else(|| crate::ExpectedState::Absent, crate::ExpectedState::Hash);
    let parent = target.parent().ok_or(RestoreError::OutsideRoot)?;
    fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
    let mut write = crate::WriteRequest::replace(&target, bytes.clone(), expected);
    if current.is_none() {
        write = crate::WriteRequest::create(&target, bytes.clone());
    }
    crate::apply(&write)?;
    if let Some(mode) = sidecar.mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|error| io(&target, error))?;
    }
    let actual = crate::symlink::read_nofollow(&target).map_err(RestoreError::Fs)?;
    let after_hash = sha256_hex(&actual);
    if after_hash != expected_hash {
        return Err(RestoreError::VerificationFailed);
    }
    let path_text = target.to_string_lossy().into_owned();
    let before_size = current
        .as_ref()
        .map_or(0, |value| i64::try_from(value.len()).unwrap_or(i64::MAX));
    let after_size = i64::try_from(actual.len()).unwrap_or(i64::MAX);
    let mut touched = ops::TouchedFile::new(
        &path_text,
        if current.is_some() {
            ops::FileAction::Modified
        } else {
            ops::FileAction::Created
        },
    )
    .with_hashes(current_hash.as_deref(), Some(&after_hash))
    .with_sizes(before_size, after_size);
    if let Some(backup_id) = current_backup_id.as_deref() {
        touched.backup_id = Some(backup_id);
    }
    ops::attach_file(request.connection, &operation, &touched)?;
    ops::finish(
        request.connection,
        &operation,
        ops::OperationStatus::Succeeded,
        None,
    )?;
    backups::mark_restored(request.connection, request.backup_id)?;
    Ok(RestoreOutcome::Restored {
        operation_id: operation_text,
        target,
    })
}
