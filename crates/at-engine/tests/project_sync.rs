//! TC-015 sync-label consumption and allowed-action tests.
use std::cell::Cell;

use at_engine::project_sync::{
    ProjectSyncCache, ProjectSyncProvider, SyncCopyRequest, SyncDirection, SyncEvidence, SyncPlan,
    SyncPlanError, SyncRecord, plan_sync_copy,
};
use at_model::{FileOp, ItemId, SyncLabel};

fn sync_request(label: SyncLabel) -> SyncCopyRequest {
    SyncCopyRequest {
        item_id: ItemId::new(),
        project_id: "project-test".to_owned(),
        direction: SyncDirection::Push,
        label,
        base_hash: Some("b".repeat(64)),
        destination_hash: Some("b".repeat(64)),
        source_hash: at_fs::sha256_hex(b"new content"),
        destination_file_hash: Some("b".repeat(64)),
        force_reviewed: false,
        source_file_hash: at_fs::sha256_hex(b"new content"),
        destination_path: "skills/demo/SKILL.md".into(),
        destination_root_exists: true,
        source_bytes: b"new content".to_vec(),
    }
}

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

#[test]
fn sync_planning_refuses_conflict_drift_and_changed_destination() {
    for label in [SyncLabel::Conflict, SyncLabel::Drifted] {
        assert!(matches!(
            plan_sync_copy(sync_request(label)),
            Err(SyncPlanError::Conflict)
        ));
    }

    let mut changed = sync_request(SyncLabel::ProjectNewer);
    changed.destination_hash = Some("c".repeat(64));
    changed.destination_file_hash = Some("c".repeat(64));
    assert!(matches!(
        plan_sync_copy(changed.clone()),
        Err(SyncPlanError::TargetChanged)
    ));
    changed.force_reviewed = true;
    let SyncPlan::Change(set) = plan_sync_copy(changed).expect("reviewed force plan") else {
        panic!("forced change requires a reviewed ChangeSet");
    };
    assert!(matches!(
        &set.ops[0],
        FileOp::Write {
            expected: at_model::ExpectedState::Hash { hash },
            ..
        } if hash.as_str().bytes().all(|byte| byte == b'c')
    ));
}

#[test]
fn sync_planning_uses_label_and_produces_reviewed_write_changeset() {
    let plan = plan_sync_copy(sync_request(SyncLabel::ProjectNewer)).expect("plan push");
    let SyncPlan::Change(set) = plan else {
        panic!("changed copies require a change set");
    };
    assert_eq!(set.operation_kind.as_str(), "sync_push");
    assert_eq!(set.ops.len(), 1);
    assert!(matches!(
        &set.ops[0],
        FileOp::Write {
            path,
            bytes,
            expected: at_model::ExpectedState::Hash { hash },
            backup: true,
            ..
        } if path == &std::path::PathBuf::from("skills/demo/SKILL.md")
            && bytes == b"new content"
            && hash.as_str().bytes().all(|byte| byte == b'b')
    ));
    set.validate().expect("valid reviewed plan");
}

#[test]
fn in_sync_copy_is_a_noop_and_direction_is_enforced() {
    let mut request = sync_request(SyncLabel::InSync);
    request.source_hash = at_fs::sha256_hex(b"new content");
    request.destination_hash = Some(request.source_hash.clone());
    assert_eq!(
        plan_sync_copy(request).expect("no-op"),
        SyncPlan::AlreadyInSync
    );

    let mut invalid = sync_request(SyncLabel::LibraryNewer);
    assert!(matches!(
        plan_sync_copy(invalid.clone()),
        Err(SyncPlanError::DirectionNotAllowed { .. })
    ));
    invalid.direction = SyncDirection::Pull;
    invalid.destination_hash = None;
    invalid.base_hash = None;
    invalid.destination_file_hash = None;
    invalid.destination_root_exists = false;
    assert!(matches!(
        plan_sync_copy(invalid.clone()),
        Err(SyncPlanError::MissingProjectStore)
    ));
    invalid.destination_root_exists = true;
    assert!(matches!(
        plan_sync_copy(invalid).expect("pull plan"),
        SyncPlan::Change(_)
    ));
}
