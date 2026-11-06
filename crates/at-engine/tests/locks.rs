//! Global locks: denial reasons, resolution propagation and orphan diagnostics.
//!
//! Everything here runs against [`MemoryLockStore`]; the SQLite repository is a
//! later story, and the trait is the contract both share.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use at_engine::EngineError;
use at_engine::locks::{
    self, ItemCatalog, LockDiagnostic, LockId, LockRecord, LockStore, LockStoreError,
    MemoryLockStore,
};
use at_engine::resolve::{ResolveInput, resolve};
use at_model::{Definition, ItemId, ItemKind, Scope, ScopeSnapshot};

fn definition(scope: Scope, name: &str, item_id: Option<ItemId>) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id,
        path: PathBuf::from(format!("{scope}/{name}/SKILL.md")),
    }
}

fn layer(scope: Scope, definitions: Vec<Definition>) -> ScopeSnapshot {
    ScopeSnapshot { scope, definitions }
}

fn hash_file(path: &Path) -> u64 {
    let bytes = fs::read(path).unwrap();
    let mut hasher = DefaultHasher::new();
    hasher.write(&bytes);
    hasher.finish()
}

/// The pattern every mutating engine operation follows: check the lock, then write.
fn guarded_edit(store: &dyn LockStore, item_id: ItemId, path: &Path) -> Result<(), EngineError> {
    locks::ensure_unlocked(store, item_id)?;
    fs::write(path, b"changed").unwrap();
    Ok(())
}

fn place_lock(store: &MemoryLockStore, item_id: ItemId, reason: &str) -> LockRecord {
    locks::lock(store, Scope::Global, item_id, reason, "cli").unwrap()
}

/// A lock pins the item to its Global definition: the Global definition wins even
/// though a Project definition outranks it, and the Project definition is reported
/// as blocked by that exact lock instead of merely shadowed.
#[test]
fn lock_marks_global_winner_and_blocks_every_deeper_definition() {
    let store = MemoryLockStore::new();
    let global_item = ItemId::new();
    let project_item = ItemId::new();
    let layers = [
        layer(
            Scope::Global,
            vec![definition(Scope::Global, "review", Some(global_item))],
        ),
        layer(
            Scope::Project,
            vec![definition(Scope::Project, "review", Some(project_item))],
        ),
    ];

    let unlocked = resolve(ResolveInput::new(&layers));
    assert_eq!(unlocked.items.len(), 1);
    assert_eq!(unlocked.items[0].scope, Scope::Project);
    assert!(unlocked.items[0].lock.is_none());
    assert!(unlocked.shadowed[0].blocked.is_none());

    let locked = place_lock(&store, global_item, "review policy is central");
    let resolved = resolve(ResolveInput::new(&layers).with_locks(&store));

    assert_eq!(resolved.items.len(), 1);
    let winner = &resolved.items[0];
    assert_eq!(winner.scope, Scope::Global);
    assert_eq!(winner.definition.item_id, Some(global_item));
    let mark = winner.lock.as_ref().expect("winner carries its lock");
    assert_eq!(mark.id, locked.id);
    assert_eq!(mark.reason, "review policy is central");
    assert_eq!(mark.locked_by_scope, Scope::Global);
    assert_eq!(mark.locked_by, "cli");
    assert!(winner.is_locked());
    assert_eq!(winner.lock_reason(), Some("review policy is central"));

    assert_eq!(resolved.shadowed.len(), 1);
    let blocked = &resolved.shadowed[0];
    assert_eq!(blocked.scope, Scope::Project);
    assert!(blocked.is_blocked());
    assert_eq!(
        blocked.blocking_lock().map(|mark| mark.id.clone()),
        Some(locked.id.clone())
    );
    assert_eq!(blocked.shadowed_by.scope, Scope::Global);
}

/// Without a lock the same layers resolve the other way round, so the test above
/// proves the lock and not the fixture made the difference.
#[test]
fn unlocking_restores_nearest_scope_precedence() {
    let store = MemoryLockStore::new();
    let global_item = ItemId::new();
    let layers = [
        layer(
            Scope::Global,
            vec![definition(Scope::Global, "review", Some(global_item))],
        ),
        layer(
            Scope::Local,
            vec![definition(Scope::Local, "review", Some(ItemId::new()))],
        ),
    ];

    place_lock(&store, global_item, "central");
    assert_eq!(
        resolve(ResolveInput::new(&layers).with_locks(&store)).items[0].scope,
        Scope::Global
    );

    locks::unlock(&store, Scope::Global, global_item).unwrap();
    let resolved = resolve(ResolveInput::new(&layers).with_locks(&store));
    assert_eq!(resolved.items[0].scope, Scope::Local);
    assert!(resolved.items[0].lock.is_none());
    assert!(resolved.shadowed[0].blocked.is_none());
}

/// Every mutating operation calls `ensure_unlocked` first, so a denial performs no
/// IO: the target file has the same bytes (and hash) before and after.
#[test]
fn denial_returns_the_recorded_reason_and_writes_zero_bytes() {
    let sandbox = tempfile::tempdir().unwrap();
    let target = sandbox.path().join("settings.json");
    fs::write(&target, br#"{"theme":"dark"}"#).unwrap();
    let before = hash_file(&target);

    let store = MemoryLockStore::new();
    let item_id = ItemId::new();
    let record = place_lock(&store, item_id, "owned by the platform team");

    let result = guarded_edit(&store, item_id, &target);
    match result {
        Err(EngineError::Locked {
            item_id: denied,
            reason,
            lock_id,
        }) => {
            assert_eq!(denied, item_id);
            assert_eq!(reason, "owned by the platform team");
            assert_eq!(lock_id, record.id);
        }
        other => panic!("expected a lock denial, got {other:?}"),
    }
    assert_eq!(hash_file(&target), before);

    // An unlocked item goes through and the guard actually lets the write happen.
    let other = ItemId::new();
    guarded_edit(&store, other, &target).unwrap();
    assert_ne!(hash_file(&target), before);
}

/// Lock and unlock are Global-only. A lower active scope is refused before the
/// store is touched, so the existing lock row keeps its id, reason and timestamp.
#[test]
fn lower_scope_may_neither_lock_nor_unlock() {
    let store = MemoryLockStore::new();
    let item_id = ItemId::new();
    let record = place_lock(&store, item_id, "central definition");

    for scope in [Scope::User, Scope::Project, Scope::Local] {
        let denied = locks::unlock(&store, scope, item_id);
        assert!(
            matches!(denied, Err(EngineError::LockedAtGlobal { active }) if active == scope),
            "unlock from {scope} must be refused"
        );
        assert_eq!(store.get(item_id).unwrap(), Some(record.clone()));
    }

    let unlocked = locks::unlock(&store, Scope::Global, item_id).unwrap();
    assert_eq!(unlocked, record);
    assert!(store.get(item_id).unwrap().is_none());

    let denied = locks::lock(&store, Scope::Project, item_id, "nope", "cli");
    assert!(matches!(
        denied,
        Err(EngineError::LockedAtGlobal {
            active: Scope::Project
        })
    ));
    assert!(
        store.list().unwrap().is_empty(),
        "a refused lock must not create a row"
    );
}

/// Resolution re-reads the store on every call, so a lock placed by another
/// process (here: after the input was built) shows up without rebuilding it.
#[test]
fn resolve_reads_locks_every_call() {
    let store = RecordingStore::default();
    let global_item = ItemId::new();
    let layers = [
        layer(
            Scope::Global,
            vec![definition(Scope::Global, "review", Some(global_item))],
        ),
        layer(
            Scope::Project,
            vec![definition(Scope::Project, "review", Some(ItemId::new()))],
        ),
    ];
    let input = ResolveInput::new(&layers).with_locks(&store);

    assert_eq!(resolve(input).items[0].scope, Scope::Project);
    let record = place_lock(&store.inner, global_item, "placed by the CLI");
    let after_lock = resolve(input);
    assert_eq!(after_lock.items[0].scope, Scope::Global);
    assert_eq!(
        after_lock.items[0]
            .lock
            .as_ref()
            .map(|mark| mark.id.clone()),
        Some(record.id)
    );
    locks::unlock(&store.inner, Scope::Global, global_item).unwrap();
    assert_eq!(resolve(input).items[0].scope, Scope::Project);
    assert_eq!(store.lists(), 3, "one store read per resolve call");
}

/// A lock whose item no longer exists is reported, not panicked on, and it pins
/// nothing: the layers still resolve normally.
#[test]
fn orphan_lock_is_reported_and_does_not_block() {
    let store = MemoryLockStore::new();
    let missing = ItemId::new();
    let record = place_lock(&store, missing, "the item was deleted");
    let healthy = place_lock(&store, ItemId::new(), "still here");

    let known = |item_id: ItemId| item_id == healthy.lock.item_id;
    let catalog: &dyn ItemCatalog = &known;
    let overviews = locks::inspect(&store, catalog).unwrap();
    assert_eq!(overviews.len(), 2);
    let orphan = overviews
        .iter()
        .find(|overview| overview.record.id == record.id)
        .expect("orphan lock is listed");
    assert!(orphan.is_orphaned());
    assert_eq!(
        orphan.diagnostic,
        Some(LockDiagnostic::OrphanedItem { item_id: missing })
    );
    let healthy_overview = overviews
        .iter()
        .find(|overview| overview.record.id == healthy.id)
        .expect("healthy lock is listed");
    assert!(!healthy_overview.is_orphaned());
    assert_eq!(healthy_overview.diagnostic, None);

    let layers = [layer(
        Scope::Project,
        vec![definition(Scope::Project, "review", Some(ItemId::new()))],
    )];
    let resolved = resolve(ResolveInput::new(&layers).with_locks(&store));
    assert_eq!(resolved.items.len(), 1);
    assert_eq!(resolved.items[0].scope, Scope::Project);
    assert!(resolved.items[0].lock.is_none());
    assert!(resolved.shadowed.is_empty());
}

/// An unreadable lock store does not fail the pass silently: the set carries a
/// diagnostic and enforcement is reported as missing.
#[test]
fn unreadable_lock_store_is_diagnosed() {
    let layers = [layer(
        Scope::Global,
        vec![definition(Scope::Global, "review", Some(ItemId::new()))],
    )];
    let resolved = resolve(ResolveInput::new(&layers).with_locks(&FailingStore));
    assert_eq!(resolved.items.len(), 1);
    assert!(resolved.lock_read_failed());
    assert_eq!(
        resolved.diagnostics,
        vec![at_engine::resolve::ResolveDiagnostic::LockReadFailed {
            message: "lock store: database is locked".to_owned()
        }]
    );
}

/// Re-locking keeps the row identity but records the newer reason and actor.
#[test]
fn relocking_keeps_the_id_and_updates_the_reason() {
    let store = MemoryLockStore::new();
    let item_id = ItemId::new();
    let first = place_lock(&store, item_id, "first reason");
    let second = locks::lock(&store, Scope::Global, item_id, "second reason", "ui").unwrap();

    assert_eq!(second.id, first.id);
    assert_eq!(second.lock.reason, "second reason");
    assert_eq!(second.lock.locked_by, "ui");
    assert_eq!(store.list().unwrap(), vec![second]);
    assert!(matches!(
        locks::unlock(&store, Scope::Global, ItemId::new()),
        Err(EngineError::NotLocked { .. })
    ));
}

#[test]
fn lock_ids_round_trip_through_text() {
    let id = LockId::new();
    assert!(id.as_str().starts_with("lck_"));
    assert_eq!(id.as_str().parse::<LockId>().unwrap(), id);
    assert!("lck_not-a-uuid".parse::<LockId>().is_err());
}

/// Counts reads so "every resolve call re-reads the store" is observable.
#[derive(Default)]
struct RecordingStore {
    inner: MemoryLockStore,
    lists: AtomicUsize,
}

impl RecordingStore {
    fn lists(&self) -> usize {
        self.lists.load(Ordering::Relaxed)
    }
}

impl LockStore for RecordingStore {
    fn list(&self) -> Result<Vec<LockRecord>, LockStoreError> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list()
    }

    fn get(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        self.inner.get(item_id)
    }

    fn put(&self, record: LockRecord) -> Result<(), LockStoreError> {
        self.inner.put(record)
    }

    fn remove(&self, item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        self.inner.remove(item_id)
    }
}

struct FailingStore;

impl LockStore for FailingStore {
    fn list(&self) -> Result<Vec<LockRecord>, LockStoreError> {
        Err(LockStoreError::Backend {
            message: "database is locked".to_owned(),
        })
    }

    fn get(&self, _item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        Err(LockStoreError::Backend {
            message: "database is locked".to_owned(),
        })
    }

    fn put(&self, _record: LockRecord) -> Result<(), LockStoreError> {
        Err(LockStoreError::Backend {
            message: "database is locked".to_owned(),
        })
    }

    fn remove(&self, _item_id: ItemId) -> Result<Option<LockRecord>, LockStoreError> {
        Err(LockStoreError::Backend {
            message: "database is locked".to_owned(),
        })
    }
}
