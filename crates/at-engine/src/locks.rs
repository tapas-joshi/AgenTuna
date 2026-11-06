//! Global locks: persistence seam, write rules and lock metadata for resolution.
//!
//! A lock pins one item to its definition at the locking scope and blocks every
//! lower scope from shadowing, disabling, editing or undeploying it. Locks live
//! only in the database `locks` table (TF-014); the Global store is not a lock
//! source, so a lock created by the CLI is visible in an open GUI as soon as the
//! next read happens.
//!
//! [`LockStore`] is the persistence seam: this crate ships [`MemoryLockStore`]
//! for tests and for callers that have no database yet, and the SQLite repository
//! lands in TA-041. Every read goes through the trait, never through a cached
//! copy, which is what makes cross-process visibility work.
//!
//! ## Denial rule
//!
//! Lock and unlock are only accepted while the caller's active scope is
//! [`Scope::Global`]; anything else returns [`EngineError::LockedAtGlobal`] before
//! the store is touched. Operations that would change a locked item must call
//! [`ensure_unlocked`] first and return [`EngineError::Locked`] untouched.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeMap;
use std::sync::RwLock;

use at_model::{ItemId, Lock, Scope};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::EngineError;

const PREFIX: &str = "lck_";

/// Why a string is not a valid [`LockId`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LockIdError {
    /// The `lck_` prefix is missing.
    #[error("lock id must start with `lck_`: `{0}`")]
    MissingPrefix(String),
    /// The part after the prefix is not a UUID.
    #[error("lock id does not contain a valid UUID: `{0}`")]
    InvalidUuid(String),
    /// The UUID is not version 7.
    #[error("lock id UUID must be version 7: `{0}`")]
    NotVersion7(String),
    /// The UUID is valid but not lowercase and hyphenated.
    #[error("lock id UUID must be lowercase and hyphenated: `{0}`")]
    NotCanonical(String),
}

/// Opaque identity of one lock row: `lck_` followed by a UUIDv7.
///
/// The id is allocated when the lock is placed and kept when its reason is
/// edited, so a denial recorded earlier still names a row that exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LockId(String);

impl LockId {
    /// Allocates a fresh id. Ids from one process sort in creation order.
    // A `Default` impl would hide that every call allocates a new identity.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self(format!("{PREFIX}{}", Uuid::now_v7().hyphenated()))
    }

    /// The id as stored, for example `lck_0193f0c4-…`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for LockId {
    type Err = LockIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| LockIdError::MissingPrefix(s.to_owned()))?;
        let uuid = Uuid::parse_str(rest).map_err(|_| LockIdError::InvalidUuid(s.to_owned()))?;
        if uuid.get_version_num() != 7 {
            return Err(LockIdError::NotVersion7(s.to_owned()));
        }
        let mut buf = Uuid::encode_buffer();
        if uuid.hyphenated().encode_lower(&mut buf) != rest {
            return Err(LockIdError::NotCanonical(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }
}

/// One stored lock: the row identity plus the domain [`Lock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecord {
    /// Identity of this lock row.
    pub id: LockId,
    /// The lock itself.
    pub lock: Lock,
}

impl LockRecord {
    /// The item the lock pins.
    #[must_use]
    pub const fn item_id(&self) -> ItemId {
        self.lock.item_id
    }

    /// The recorded reason shown wherever the lock denies or explains something.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.lock.reason
    }
}

/// Why the lock store could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LockStoreError {
    /// The backing store returned an error. Nothing is assumed about its contents.
    #[error("lock store: {message}")]
    Backend {
        /// Backend error text, already free of secret values.
        message: String,
    },
    /// The store could not be reached at all, for example a poisoned in-process lock.
    #[error("lock store unavailable: {message}")]
    Unavailable {
        /// What made the store unreachable.
        message: String,
    },
}

/// Persistence seam for locks.
///
/// Implementations return locks ordered by ascending [`ItemId`] so resolution is
/// deterministic. A failed read must not be reported as "no locks": return
/// [`LockStoreError`] and let the caller decide.
pub trait LockStore: Send + Sync {
    /// Every lock, ordered by item id.
    fn list(&self) -> Result<Vec<LockRecord>, LockStoreError>;
    /// The lock on `item_id`, if there is one.
    fn get(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError>;
    /// Inserts or replaces the lock on `record.lock.item_id`.
    fn put(&self, record: LockRecord) -> Result<(), LockStoreError>;
    /// Removes and returns the lock on `item_id`.
    fn remove(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError>;
}

/// In-process [`LockStore`]. Locks last as long as the value.
///
/// Used by tests and by callers that run before the database exists. It is not a
/// file: it never touches disk and never sees another process's locks.
#[derive(Debug, Default)]
pub struct MemoryLockStore {
    locks: RwLock<BTreeMap<ItemId, LockRecord>>,
}

impl MemoryLockStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn poisoned() -> LockStoreError {
        LockStoreError::Unavailable {
            message: "in-process lock store was poisoned by a panicking writer".to_owned(),
        }
    }
}

impl LockStore for MemoryLockStore {
    fn list(&self) -> Result<Vec<LockRecord>, LockStoreError> {
        let locks = self.locks.read().map_err(|_| Self::poisoned())?;
        Ok(locks.values().cloned().collect())
    }

    fn get(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        let locks = self.locks.read().map_err(|_| Self::poisoned())?;
        Ok(locks.get(&item_id).cloned())
    }

    fn put(&self, record: LockRecord) -> Result<(), LockStoreError> {
        let mut locks = self.locks.write().map_err(|_| Self::poisoned())?;
        locks.insert(record.lock.item_id, record);
        Ok(())
    }

    fn remove(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        let mut locks = self.locks.write().map_err(|_| Self::poisoned())?;
        Ok(locks.remove(&item_id))
    }
}

/// Whether an item still exists in the canonical catalog.
///
/// The lock list uses it to report orphan locks, and later stories use it to
/// refuse locking an item that would be an orphan immediately.
pub trait ItemCatalog: Send + Sync {
    /// Whether `item_id` is a live item.
    fn contains(&self, item_id: ItemId) -> bool;
}

impl<F: Fn(ItemId) -> bool + Send + Sync> ItemCatalog for F {
    fn contains(&self, item_id: ItemId) -> bool {
        self(item_id)
    }
}

/// A lock as it applies to a resolved item.
///
/// The name mirrors the story's `Locked{reason, locked_at, locked_by_scope}` mark:
/// [`LockMark::reason`] and [`LockMark::locked_at`] are the recorded values and
/// [`LockMark::locked_by_scope`] is the scope holding the pinned definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockMark {
    /// Identity of the lock row; this is the `lock_id` of a blocked definition.
    pub id: LockId,
    /// Scope that holds the locked definition.
    pub locked_by_scope: Scope,
    /// Why the item was locked.
    pub reason: String,
    /// Who placed the lock.
    pub locked_by: String,
    /// When the lock was placed.
    pub locked_at: OffsetDateTime,
}

impl From<&LockRecord> for LockMark {
    fn from(record: &LockRecord) -> Self {
        Self {
            id: record.id.clone(),
            locked_by_scope: record.lock.scope,
            reason: record.lock.reason.clone(),
            locked_by: record.lock.locked_by.clone(),
            locked_at: record.lock.locked_at,
        }
    }
}

/// A problem found while listing locks.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockDiagnostic {
    /// The lock points at an item that no longer exists in the catalog.
    ///
    /// Resolution ignores such a lock: there is no definition to pin. The list
    /// shows it so the user can unlock or repair it instead of it taking effect
    /// silently never.
    OrphanedItem {
        /// Item the lock names.
        item_id: ItemId,
    },
}

/// One lock plus the diagnostics that apply to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockOverview {
    /// The stored lock.
    pub record: LockRecord,
    /// `None` for a healthy lock.
    pub diagnostic: Option<LockDiagnostic>,
}

impl LockOverview {
    /// Whether this lock points at an item that no longer exists.
    #[must_use]
    pub fn is_orphaned(&self) -> bool {
        matches!(self.diagnostic, Some(LockDiagnostic::OrphanedItem { .. }))
    }
}

/// Creates or replaces the lock on `item_id`.
///
/// Only [`Scope::Global`] may lock; any other active scope returns
/// [`EngineError::LockedAtGlobal`] without touching the store. Re-locking an
/// already locked item keeps its [`LockId`] and updates the reason, who and when.
///
/// # Errors
///
/// [`EngineError::LockedAtGlobal`] for a non-Global active scope, and
/// [`EngineError::LockStore`] when the store fails.
pub fn lock(
    store: &dyn LockStore,
    active_scope: Scope,
    item_id: ItemId,
    reason: &str,
    locked_by: &str,
) -> Result<LockRecord, EngineError> {
    if active_scope != Scope::Global {
        return Err(EngineError::LockedAtGlobal {
            active: active_scope,
        });
    }
    let existing = store.get(item_id)?;
    let record = LockRecord {
        id: existing.map_or_else(LockId::new, |record| record.id),
        lock: Lock {
            scope: Scope::Global,
            item_id,
            reason: reason.to_owned(),
            locked_by: locked_by.to_owned(),
            locked_at: OffsetDateTime::now_utc(),
        },
    };
    store.put(record.clone())?;
    Ok(record)
}

/// Removes the lock on `item_id`, returning the row that was removed.
///
/// Only [`Scope::Global`] may unlock; any other active scope returns
/// [`EngineError::LockedAtGlobal`] and leaves the row untouched.
///
/// # Errors
///
/// [`EngineError::LockedAtGlobal`] for a non-Global active scope,
/// [`EngineError::NotLocked`] when no lock exists, and [`EngineError::LockStore`]
/// when the store fails.
pub fn unlock(
    store: &dyn LockStore,
    active_scope: Scope,
    item_id: ItemId,
) -> Result<LockRecord, EngineError> {
    if active_scope != Scope::Global {
        return Err(EngineError::LockedAtGlobal {
            active: active_scope,
        });
    }
    store
        .remove(item_id)?
        .ok_or(EngineError::NotLocked { item_id })
}

/// Fails with [`EngineError::Locked`] when `item_id` is locked.
///
/// Every operation that would shadow, disable, edit or undeploy an item calls
/// this before planning its writes, so a denied operation performs no IO at all.
///
/// # Errors
///
/// [`EngineError::Locked`] with the recorded reason, or [`EngineError::LockStore`]
/// when the lock cannot be read.
pub fn ensure_unlocked(store: &dyn LockStore, item_id: ItemId) -> Result<(), EngineError> {
    match store.get(item_id)? {
        Some(record) => Err(EngineError::Locked {
            item_id,
            reason: record.lock.reason.clone(),
            lock_id: record.id,
        }),
        None => Ok(()),
    }
}

/// Lists every lock with the diagnostics that apply to it, ordered by item id.
///
/// An orphaned lock is reported, never treated as a panic or silently dropped.
///
/// # Errors
///
/// [`EngineError::LockStore`] when the lock list cannot be read.
pub fn inspect(
    store: &dyn LockStore,
    catalog: &dyn ItemCatalog,
) -> Result<Vec<LockOverview>, EngineError> {
    Ok(store
        .list()?
        .into_iter()
        .map(|record| {
            let item_id = record.lock.item_id;
            let diagnostic =
                (!catalog.contains(item_id)).then_some(LockDiagnostic::OrphanedItem { item_id });
            LockOverview { record, diagnostic }
        })
        .collect())
}
