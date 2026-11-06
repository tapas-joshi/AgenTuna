//! Typed failures for engine operations.
//!
//! Denials carry the data the caller needs to explain them: a locked item reports
//! the recorded reason, and a scope violation reports which scope was active. No
//! variant carries file content or secret values.

use at_model::{ItemId, Scope};

use crate::locks::{LockId, LockStoreError};

/// Why an engine operation did not happen.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The item is pinned to its definition at the locking scope, so a lower scope
    /// may not shadow, disable, edit or undeploy it.
    #[error("{item_id} is locked: {reason}")]
    Locked {
        /// The locked item.
        item_id: ItemId,
        /// Why the item was locked, as recorded when the lock was placed.
        reason: String,
        /// Identity of the lock that denied the operation.
        lock_id: LockId,
    },
    /// Locks may only be created or removed while the caller's active scope is Global.
    #[error("locks are Global-only; the active scope is `{active}`")]
    LockedAtGlobal {
        /// Scope the caller was working in.
        active: Scope,
    },
    /// The lock store failed. Nothing was written.
    #[error(transparent)]
    LockStore(#[from] LockStoreError),
    /// The caller asked to remove a lock that does not exist.
    #[error("{item_id} is not locked")]
    NotLocked {
        /// Item that has no lock row.
        item_id: ItemId,
    },
    /// The store manifest could not be loaded, parsed, validated or written.
    #[error(transparent)]
    Manifest(#[from] at_store::manifest::ManifestError),
    /// A store file could not be locked, saved or reconciled.
    #[error(transparent)]
    Store(#[from] at_store::store_io::StoreIoError),
    /// A tombstone source could not be read.
    #[error(transparent)]
    Tombstone(#[from] crate::tombstones::TombstoneError),
}
