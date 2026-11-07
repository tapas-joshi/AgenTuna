//! Durable snapshot of the index rows a delete operation removed.
//!
//! The `items` and `deployments` rows are a derived projection, but a delete removes rows that
//! no manifest or native file can fully reconstruct: which agents held a deployment, its mode
//! and its per-cell hashes are not recoverable from the filesystem alone. Before a delete removes
//! those rows the command captures them here, next to the operation backups, and a later undo
//! restores the exact rows. The file is best-effort: a missing or malformed snapshot never blocks
//! an undo, and older operations simply do not have one.

use std::fs;
use std::path::{Path, PathBuf};

use at_db::DbError;
use at_db::deployments::{UndoDeploymentSnapshot, restore_undo_snapshots, undo_snapshots};
use at_db::items::{UndoItemSnapshot, restore_undo_snapshot, undo_snapshot};
use at_fs::{ExpectedState, WriteRequest};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Directory below the backup root holding per-operation snapshots.
pub const METADATA_DIR: &str = "undo-metadata";

/// Rows removed by one delete operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoMetadata {
    /// Removed `items` rows.
    pub items: Vec<UndoItemSnapshot>,
    /// Removed `deployments` rows.
    pub deployments: Vec<UndoDeploymentSnapshot>,
}

/// Snapshot of the index rows for one item, ready to be written after the delete.
///
/// # Errors
/// Returns the database failure when the rows cannot be read.
pub fn capture(
    connection: &Connection,
    item_id: &at_model::ItemId,
) -> Result<UndoMetadata, DbError> {
    Ok(UndoMetadata {
        items: undo_snapshot(connection, item_id)?.into_iter().collect(),
        deployments: undo_snapshots(connection, item_id)?,
    })
}

/// Path of the snapshot for one operation.
#[must_use]
pub fn metadata_path(backup_root: &Path, operation_id: &str) -> PathBuf {
    backup_root
        .join(METADATA_DIR)
        .join(format!("{operation_id}.json"))
}

/// Writes the snapshot for one operation through the at-fs safe-write pipeline.
///
/// # Errors
/// Returns a human-readable failure when the directory or file cannot be written.
pub fn write(
    backup_root: &Path,
    operation_id: &str,
    metadata: &UndoMetadata,
) -> Result<(), String> {
    let directory = backup_root.join(METADATA_DIR);
    at_fs::ensure_dir(&directory).map_err(|error| error.to_string())?;
    let destination = directory.join(format!("{operation_id}.json"));
    let bytes = serde_json::to_vec(metadata).map_err(|error| error.to_string())?;
    let request = match fs::read(&destination) {
        Ok(current) => WriteRequest::replace(
            destination,
            bytes,
            ExpectedState::Hash(at_fs::sha256_hex(&current)),
        ),
        Err(_) => WriteRequest::create(destination, bytes),
    };
    at_fs::apply(&request)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Writes the snapshot under every operation a multi-phase apply created after `after_rowid`.
///
/// One reviewed delete is journaled as more than one ChangeSet (store and native phases), and
/// the undo runs newest-first. Attaching the snapshot to each phase means whichever phase is
/// undone first restores the rows, and the later phase restores them again instead of deleting
/// them before its own manifest is back.
///
/// # Errors
/// Returns the database failure when the operation rows cannot be read. Individual file-write
/// failures are ignored: the snapshot is a best-effort undo aid, never a delete precondition.
pub fn write_for_new_operations(
    connection: &Connection,
    backup_root: &Path,
    after_rowid: i64,
    metadata: &UndoMetadata,
) -> Result<usize, DbError> {
    let ids = at_db::ops::ids_after(connection, after_rowid)?;
    let mut written = 0;
    for id in &ids {
        if write(backup_root, id, metadata).is_ok() {
            written += 1;
        }
    }
    Ok(written)
}

/// The current operation rowid high-water mark.
///
/// # Errors
/// Returns the database failure when the history cannot be read.
pub fn operation_rowid_high_water(connection: &Connection) -> Result<i64, DbError> {
    at_db::ops::rowid_high_water(connection)
}

/// Reads the snapshot for one operation. A missing or malformed file is `None`.
#[must_use]
pub fn read(backup_root: &Path, operation_id: &str) -> Option<UndoMetadata> {
    let path = metadata_path(backup_root, operation_id);
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Restores the captured rows, replacing any current row for the same identity.
///
/// # Errors
/// Returns the database failure when the rows cannot be written.
pub fn restore(connection: &Connection, metadata: &UndoMetadata) -> Result<(), DbError> {
    for item in &metadata.items {
        let item_id = item
            .id
            .parse::<at_model::ItemId>()
            .map_err(|_| DbError::InvalidValue {
                field: "undo item id",
                reason: item.id.clone(),
            })?;
        restore_undo_snapshot(connection, item)?;
        let deployments = metadata
            .deployments
            .iter()
            .filter(|deployment| deployment.item_id == item.id)
            .cloned()
            .collect::<Vec<_>>();
        restore_undo_snapshots(connection, &item_id, &deployments)?;
    }
    Ok(())
}
