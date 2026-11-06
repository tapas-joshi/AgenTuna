//! TC-015 sync-label consumption and allowed-action tests.
use std::cell::Cell;

use at_engine::project_sync::{
    ProjectSyncCache, ProjectSyncProvider, SyncCopyRequest, SyncDirection, SyncEvidence,
    SyncNamedHash, SyncPlan, SyncPlanError, SyncRecord, SyncTreeRequest, detect_sync_renames,
    plan_sync_copy, plan_sync_tree,
};
use at_engine::project_sync::{SyncBaselineTarget, apply_sync_plan_and_update_baseline};
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

#[test]
fn tree_sync_plans_skill_mcp_and_rule_files_as_one_reviewed_change() {
    let item_id = ItemId::new();
    let files = [
        ("skills/demo/SKILL.md", b"skill".as_slice()),
        ("mcp.json", b"{\"servers\":{}}".as_slice()),
        ("rules/AGENTS.md", b"rule".as_slice()),
    ];
    let source_files = files
        .iter()
        .map(|(path, bytes)| (std::path::PathBuf::from(path), bytes.to_vec()))
        .collect();
    let source_file_hashes = files
        .iter()
        .map(|(path, bytes)| (std::path::PathBuf::from(path), at_fs::sha256_hex(bytes)))
        .collect();
    let plan = plan_sync_tree(SyncTreeRequest {
        item_id,
        project_id: "project-test".to_owned(),
        direction: SyncDirection::Pull,
        label: SyncLabel::LibraryNewer,
        base_hash: Some("a".repeat(64)),
        destination_hash: Some("a".repeat(64)),
        source_hash: "b".repeat(64),
        force_reviewed: false,
        destination_root_exists: true,
        source_files,
        source_file_hashes,
        destination_files: [
            ("skills/demo/SKILL.md", at_fs::sha256_hex(b"old skill")),
            ("skills/demo/old.txt", at_fs::sha256_hex(b"remove")),
            ("mcp.json", at_fs::sha256_hex(b"old mcp")),
        ]
        .into_iter()
        .map(|(path, hash)| (std::path::PathBuf::from(path), hash))
        .collect(),
    })
    .expect("tree plan");
    let SyncPlan::ChangeWithMissing {
        change_set,
        missing_source_paths,
    } = plan
    else {
        panic!("tree changes and missing paths should be reported");
    };
    assert_eq!(change_set.ops.len(), 3);
    assert_eq!(
        missing_source_paths,
        [std::path::PathBuf::from("skills/demo/old.txt")]
    );
    assert!(
        change_set
            .ops
            .iter()
            .all(|op| !matches!(op, FileOp::Delete { .. }))
    );
    change_set.validate().expect("tree changeset validates");
}

#[test]
fn rename_detection_requires_same_identity_and_content() {
    let id = ItemId::new();
    let candidates = detect_sync_renames(
        &[SyncNamedHash {
            item_id: id,
            name: "new-name".to_owned(),
            hash: "a".repeat(64),
        }],
        &[SyncNamedHash {
            item_id: id,
            name: "old-name".to_owned(),
            hash: "a".repeat(64),
        }],
    );
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].project_name, "new-name");
    assert_eq!(candidates[0].library_name, "old-name");
    assert!(
        detect_sync_renames(
            &[SyncNamedHash {
                item_id: id,
                name: "new-name".to_owned(),
                hash: "b".repeat(64),
            }],
            &[SyncNamedHash {
                item_id: id,
                name: "old-name".to_owned(),
                hash: "a".repeat(64),
            }],
        )
        .is_empty()
    );
}

#[derive(Default)]
struct SyncRecorder;

impl at_fs::changeset::OperationRecorder for SyncRecorder {
    fn begin(&mut self, _: &at_model::ChangeSet) -> Result<String, String> {
        Ok("sync-operation".to_owned())
    }

    fn file_completed(
        &mut self,
        _: &str,
        _: &std::path::Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&std::path::Path>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn successful_apply_updates_the_existing_project_sync_baseline() {
    let sandbox = tempfile::tempdir().expect("temporary sandbox");
    let db = at_db::Database::open_in_memory().expect("in-memory database");
    let item_id = ItemId::new();
    let project_id = "project-sync";
    db.connection()
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name) \
             VALUES (?1, 'skill', 'project', ?2, 'demo', 'Demo')",
            rusqlite::params![item_id.to_string(), project_id],
        )
        .expect("seed item");
    db.connection()
        .execute(
            "INSERT INTO agents (id, display_name, installed, managed) \
             VALUES ('claude_code', 'Claude Code', 1, 1)",
            [],
        )
        .expect("seed agent");
    let initial_baseline = "b".repeat(64);
    let spec = at_db::deployments::DeploymentSpec::new(
        &item_id,
        &at_model::AgentId::ClaudeCode,
        at_model::Scope::Project,
        at_model::ItemState::Linked,
    )
    .with_project(project_id)
    .with_hashes(None, None, Some(&initial_baseline));
    at_db::deployments::upsert_deployment(db.connection(), &spec).expect("seed deployment");

    let mut request = sync_request(SyncLabel::ProjectNewer);
    request.item_id = item_id;
    request.project_id = project_id.to_owned();
    request.destination_file_hash = None;
    let baseline = request.source_hash.clone();
    let SyncPlan::Change(mut change_set) = plan_sync_copy(request).expect("plan sync") else {
        panic!("project change requires an apply plan");
    };
    let root = sandbox.path().join("store");
    at_store::layout::StoreLayout::new(at_model::Scope::Project, root.clone())
        .init()
        .expect("initialize project store");
    std::fs::create_dir_all(root.join("skills/demo")).expect("create item directory");
    change_set.preview_id = at_engine::preview::preview(&change_set, &root)
        .expect("review preview")
        .id;
    assert_eq!(
        at_engine::preview::preview(&change_set, &root)
            .expect("unchanged preview")
            .id,
        change_set.preview_id
    );
    let backup_root = root.join("backups");
    let mut recorder = SyncRecorder;
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &root,
        backup_folder: &backup_root,
        recorder: &mut recorder,
    };
    apply_sync_plan_and_update_baseline(
        &change_set,
        &mut context,
        SyncBaselineTarget {
            database: &db,
            item_id: &item_id,
            project_id,
            hash: &baseline,
        },
    )
    .expect("apply and record baseline");
    let rows = at_db::deployments::list_deployments_for_item(db.connection(), &item_id)
        .expect("read deployment");
    assert_eq!(rows[0].baseline_hash.as_deref(), Some(baseline.as_str()));
    assert_eq!(
        std::fs::read(root.join("skills/demo/SKILL.md")).expect("read applied content"),
        b"new content"
    );
}
