//! Disable-inherited tombstones (TA-003).
//!
//! A tombstone is a `disabled[]` entry in one scope's `store.json`
//! (`{item_id, kind, agents}` — decision 16, schema owned by TC-002). It removes the
//! item from the effective set for that scope and every deeper scope, optionally only
//! for the listed agents (`agents: null` means every agent).
//!
//! Tombstones are applied after precedence inside [`crate::resolve`]: the winning
//! definition is chosen first, then any logical id a matching tombstone names is
//! removed from the effective set entirely. A Global [`crate::locks`] pin dominates a
//! tombstone — a locked item cannot be disabled, which is also what
//! [`disable`] enforces before it writes.
//!
//! Persistence is the caller's job through [`at_store::store_io`]: [`disable`] and
//! [`enable`] show the locked load → mutate → hash-checked save sequence every writer
//! must follow, so `store.json` is never written without the manifest hash observed
//! under `.state/store.lock`.

use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::error::EngineError;
use crate::locks::{self, LockStore};
use at_fs::BackupPolicy;
use at_model::{ItemId, ItemKind, Scope};
use at_store::layout::StoreLayout;
use at_store::manifest::{DisabledAgents, DisabledEntry, ManifestError, StoreManifest};
use at_store::store_io::{self, StoreAccess};

/// Why a tombstone source could not be read.
///
/// Mirrors [`crate::locks::LockStoreError`]: a failed read must not be reported as
/// "no tombstones".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TombstoneError {
    /// The backing store returned an error.
    #[error("tombstone source: {message}")]
    Backend {
        /// Backend error text, already free of secret values.
        message: String,
    },
    /// The store could not be reached at all.
    #[error("tombstone source unavailable: {message}")]
    Unavailable {
        /// What made the store unreachable.
        message: String,
    },
}

/// Per-scope tombstone lookup, the resolution-time counterpart of [`LockStore`].
///
/// Implementations return the `disabled[]` entries of one scope's `store.json`.
/// A scope with no manifest returns an empty list; a failed read returns
/// [`TombstoneError`] so resolution can report it as a diagnostic instead of silently
/// treating every item as enabled.
pub trait TombstoneSource: Send + Sync {
    /// Every tombstone of `scope`, in manifest order.
    fn disabled(&self, scope: Scope) -> Result<Vec<DisabledEntry>, TombstoneError>;
}

/// In-memory [`TombstoneSource`] for tests and callers without a store on disk.
///
/// Locks last as long as the value; nothing here touches the filesystem.
#[derive(Debug, Default)]
pub struct MemoryTombstones {
    scopes: RwLock<BTreeMap<Scope, Vec<DisabledEntry>>>,
}

impl MemoryTombstones {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn poisoned() -> TombstoneError {
        TombstoneError::Unavailable {
            message: "in-process tombstone source was poisoned by a panicking writer".to_owned(),
        }
    }

    /// Replaces every tombstone of `scope`.
    ///
    /// # Errors
    /// [`TombstoneError::Unavailable`] when the map lock is poisoned.
    pub fn set(&self, scope: Scope, entries: Vec<DisabledEntry>) -> Result<(), TombstoneError> {
        let mut scopes = self.scopes.write().map_err(|_| Self::poisoned())?;
        scopes.insert(scope, entries);
        Ok(())
    }

    /// Adds one tombstone to `scope`.
    ///
    /// # Errors
    /// [`TombstoneError::Unavailable`] when the map lock is poisoned.
    pub fn add(&self, scope: Scope, entry: DisabledEntry) -> Result<(), TombstoneError> {
        let mut scopes = self.scopes.write().map_err(|_| Self::poisoned())?;
        scopes.entry(scope).or_default().push(entry);
        Ok(())
    }
}

impl TombstoneSource for MemoryTombstones {
    fn disabled(&self, scope: Scope) -> Result<Vec<DisabledEntry>, TombstoneError> {
        let scopes = self.scopes.read().map_err(|_| Self::poisoned())?;
        Ok(scopes.get(&scope).cloned().unwrap_or_default())
    }
}

/// [`TombstoneSource`] backed by already-loaded [`StoreManifest`]s.
///
/// Build it from the manifests of the scopes taking part in a resolution; scopes
/// never inserted answer with an empty list.
#[derive(Debug, Default)]
pub struct ManifestTombstones {
    scopes: BTreeMap<Scope, Vec<DisabledEntry>>,
}

impl ManifestTombstones {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A source holding only `manifest`'s tombstones for `scope`.
    #[must_use]
    pub fn from_manifest(scope: Scope, manifest: &StoreManifest) -> Self {
        let mut source = Self::new();
        source.insert_manifest(scope, manifest);
        source
    }

    /// Adds `manifest`'s tombstones for `scope`, replacing any earlier entries.
    pub fn insert_manifest(&mut self, scope: Scope, manifest: &StoreManifest) -> &mut Self {
        self.scopes.insert(scope, manifest.disabled().to_vec());
        self
    }

    /// Adds raw entries for `scope`, replacing any earlier entries.
    pub fn insert_entries(
        &mut self,
        scope: Scope,
        entries: impl IntoIterator<Item = DisabledEntry>,
    ) -> &mut Self {
        self.scopes.insert(scope, entries.into_iter().collect());
        self
    }
}

impl TombstoneSource for ManifestTombstones {
    fn disabled(&self, scope: Scope) -> Result<Vec<DisabledEntry>, TombstoneError> {
        Ok(self.scopes.get(&scope).cloned().unwrap_or_default())
    }
}

/// Writes one tombstone into `layout`'s `store.json`.
///
/// The sequence is the safe-write contract: the Global [`LockStore`] is checked
/// first — a locked item returns [`EngineError::Locked`] with the recorded reason and
/// nothing is written — then `.state/store.lock` is taken exclusively,
/// `store.json` is loaded, `manifest.disable` writes the `{item_id, kind, agents}`
/// entry and the manifest is saved against the hash just observed, so a concurrent
/// writer fails with [`EngineError::Store`]. `agents` is [`DisabledAgents::All`] for
/// `agents: null` or [`DisabledAgents::Only`] for a per-agent list.
///
/// # Errors
/// [`EngineError::Locked`] for a locked item, [`EngineError::NotLocked`] never,
/// [`EngineError::Manifest`] when the item is not in this scope's manifest or the
/// document is invalid, [`EngineError::Store`] for lock, read-only and concurrency
/// failures.
pub fn disable(
    layout: &StoreLayout,
    locks: &dyn LockStore,
    item_id: ItemId,
    kind: ItemKind,
    agents: DisabledAgents,
) -> Result<(), EngineError> {
    locks::ensure_unlocked(locks, item_id)?;
    mutate_manifest(layout, |manifest| {
        manifest
            .disable_inherited(item_id, kind, agents)
            .map_err(ManifestError::from)?;
        Ok(true)
    })
    .map(|_| ())
}

/// Convenience alias for [`disable`].
pub fn disable_inherited(
    layout: &StoreLayout,
    locks: &dyn LockStore,
    item_id: ItemId,
    kind: ItemKind,
    agents: DisabledAgents,
) -> Result<(), EngineError> {
    disable(layout, locks, item_id, kind, agents)
}

/// Removes the tombstone for `item_id` from `layout`'s `store.json`.
///
/// Returns whether a tombstone was present. Re-enabling an item that was never
/// disabled is a successful no-op: no write happens and `store.json` keeps its
/// bytes. The same lock check as [`disable`] applies.
///
/// # Errors
/// Same as [`disable`].
pub fn enable(
    layout: &StoreLayout,
    locks: &dyn LockStore,
    item_id: ItemId,
) -> Result<bool, EngineError> {
    locks::ensure_unlocked(locks, item_id)?;
    mutate_manifest(layout, |manifest| Ok(manifest.enable(item_id)))
}

/// Loads `layout`'s manifest under the exclusive store lock, applies `change`, and
/// saves against the observed hash. Nothing is written when `change` reports no
/// mutation (`false`) or fails.
fn mutate_manifest(
    layout: &StoreLayout,
    change: impl FnOnce(&mut StoreManifest) -> Result<bool, ManifestError>,
) -> Result<bool, EngineError> {
    let loaded = StoreManifest::load(layout)?;
    let mut manifest = loaded.manifest.clone();
    if !change(&mut manifest)? {
        return Ok(false);
    }
    if manifest == loaded.manifest {
        return Ok(false);
    }
    store_io::save_store(
        layout,
        &manifest,
        &loaded.hash,
        StoreAccess::ReadWrite,
        BackupPolicy::None,
    )?;
    Ok(true)
}
