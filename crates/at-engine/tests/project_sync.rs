//! TC-015 sync-label consumption and allowed-action tests.
use std::cell::Cell;

use at_engine::project_sync::{ProjectSyncCache, ProjectSyncProvider, SyncEvidence, SyncRecord};
use at_model::{ItemId, SyncLabel};

struct Provider {
    version: Cell<u64>,
    label: Cell<SyncLabel>,
    calls: Cell<usize>,
}

impl ProjectSyncProvider for Provider {
    fn state_version(&self) -> u64 {
        self.version.get()
    }

    fn sync_record(&self, _item_id: ItemId) -> Result<Option<SyncRecord>, at_db::DbError> {
        self.calls.set(self.calls.get() + 1);
        Ok(Some(SyncRecord {
            label: self.label.get(),
            evidence: SyncEvidence {
                project_hash: Some("project".to_owned()),
                library_hash: Some("library".to_owned()),
                base_hash: Some("base".to_owned()),
            },
        }))
    }
}

#[test]
fn preserves_provider_label_evidence_and_caches_by_state_version() {
    let provider = Provider {
        version: Cell::new(1),
        label: Cell::new(SyncLabel::Conflict),
        calls: Cell::new(0),
    };
    let cache = ProjectSyncCache::default();
    let item = ItemId::new();

    let first = cache
        .get(&provider, item)
        .expect("database query")
        .expect("provider record");
    assert_eq!(first.label, SyncLabel::Conflict);
    assert_eq!(first.evidence.project_hash.as_deref(), Some("project"));
    assert_eq!(provider.calls.get(), 1);

    provider.label.set(SyncLabel::LibraryOnly);
    assert_eq!(
        cache
            .get(&provider, item)
            .expect("database query")
            .expect("cached record")
            .label,
        SyncLabel::Conflict
    );
    assert_eq!(provider.calls.get(), 1);

    provider.version.set(2);
    assert_eq!(
        cache
            .get(&provider, item)
            .expect("database query")
            .expect("new record")
            .label,
        SyncLabel::LibraryOnly
    );
    assert_eq!(provider.calls.get(), 2);
}

#[test]
fn consumes_a_real_ta005_classification_without_rederiving_it() {
    use at_engine::project_sync::SyncRecord;
    use at_engine::state::{DeploymentFacts, classify};

    let classified = classify(
        DeploymentFacts::managed(Some("project"), Some("library"), Some("project"))
            .with_state_version(7),
    );
    let record = SyncRecord::from_derived(classified).expect("TA-005 label");
    assert_eq!(record.label, SyncLabel::ProjectNewer);
    assert_eq!(record.evidence.project_hash.as_deref(), Some("project"));
    assert_eq!(record.evidence.library_hash.as_deref(), Some("library"));
    assert_eq!(record.evidence.base_hash.as_deref(), Some("project"));
}

#[test]
fn maps_every_label_to_only_its_allowed_action() {
    use at_engine::project_sync::{SyncAction, action_for};
    let cases = [
        (SyncLabel::InSync, SyncAction::None),
        (SyncLabel::ProjectOnly, SyncAction::Push),
        (SyncLabel::LibraryOnly, SyncAction::Pull),
        (SyncLabel::LibraryNewer, SyncAction::Pull),
        (SyncLabel::ProjectNewer, SyncAction::Push),
        (SyncLabel::Conflict, SyncAction::Compare),
        (SyncLabel::Drifted, SyncAction::Compare),
    ];
    for (label, expected) in cases {
        assert_eq!(action_for(label), expected);
    }
}

#[test]
fn explicit_invalidation_refreshes_after_push_or_pull() {
    let provider = Provider {
        version: Cell::new(1),
        label: Cell::new(SyncLabel::ProjectNewer),
        calls: Cell::new(0),
    };
    let cache = ProjectSyncCache::default();
    let item = ItemId::new();
    let _ = cache.get(&provider, item).expect("database query");
    provider.label.set(SyncLabel::InSync);
    cache.invalidate(item);
    assert_eq!(
        cache
            .get(&provider, item)
            .expect("database query")
            .expect("refreshed record")
            .label,
        SyncLabel::InSync
    );
    assert_eq!(provider.calls.get(), 2);
}
