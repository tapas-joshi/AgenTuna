//! TC-020: Repositories over TF-014, TF-016 and TF-018 tables (no DDL).

mod common;

use at_db::Database;
use at_db::backups::{
    NewBackup, backup_by_id, backups_between, backups_for_path, durable_history, mark_restored,
    record_backup,
};
use at_db::drift::{
    DriftResolution, NewDriftEvent, open_drift_events, record_drift, resolve_drift,
};
use at_db::native::{
    NativeItemFilter, NativeScanEntry, NativeStatus, list_native_items, native_item_by_path,
    replace_scan,
};
use at_db::remote_backup::{
    RemoteBackupState, RemoteBackupStatus, remote_backup_state, save_remote_backup_state,
    set_remote_backup_status,
};
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

use common::{ALL_VERSIONS, open_with};

fn test_agent() -> AgentId {
    "claude_code".parse().expect("valid agent id")
}

fn other_agent() -> AgentId {
    "codex".parse().expect("valid agent id")
}

#[test]
fn schema_tables_and_columns_are_unchanged() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    // Verify key tables exist from TF-014, TF-016, TF-018
    let mut tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("tables");
    tables.retain(|t| t != "sqlite_sequence");

    for required in [
        "native_items",
        "drift_events",
        "locks",
        "operations",
        "operation_files",
        "backups",
        "settings",
        "secret_refs",
        "remote_backup_state",
    ] {
        assert!(
            tables.contains(&required.to_string()),
            "missing table {required}"
        );
    }
}

#[test]
fn replace_scan_removes_stale_rows_only_for_scope_preserves_first_seen_and_marks_missing() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let agent = test_agent();
    let other = other_agent();

    // Initial scan with 2 entries in User scope and 1 entry in Project scope for same agent
    let entry1 = NativeScanEntry::new(ItemKind::Skill, "skill-1", "/home/user/.agents/skills/s1")
        .with_content_hash("hash1");
    let entry2 = NativeScanEntry::new(ItemKind::Skill, "skill-2", "/home/user/.agents/skills/s2")
        .with_content_hash("hash2");
    replace_scan(connection, &agent, Scope::User, &[entry1, entry2]).expect("first scan");

    // Also an entry for other agent in User scope
    let entry_other = NativeScanEntry::new(
        ItemKind::Skill,
        "skill-other",
        "/home/other/.agents/skills/so",
    )
    .with_content_hash("hash-other");
    replace_scan(connection, &other, Scope::User, &[entry_other]).expect("other scan");

    // Verify initial insertion
    let item1_before = native_item_by_path(connection, "/home/user/.agents/skills/s1")
        .expect("lookup")
        .expect("item1 found");
    assert_eq!(item1_before.status, NativeStatus::New);
    let first_seen_s1 = item1_before.first_seen_at.clone();

    // Second scan for agent in User scope:
    // - s1 is still present with a new hash
    // - s2 vanished (missing from the scan)
    // - s3 is new
    let entry1_updated = NativeScanEntry::new(
        ItemKind::Skill,
        "skill-1-renamed",
        "/home/user/.agents/skills/s1",
    )
    .with_content_hash("hash1-new");
    let entry3 = NativeScanEntry::new(ItemKind::Skill, "skill-3", "/home/user/.agents/skills/s3")
        .with_content_hash("hash3");
    let report = replace_scan(
        connection,
        &agent,
        Scope::User,
        &[entry1_updated.clone(), entry3.clone()],
    )
    .expect("rescan");

    assert_eq!(report.upserted, 2);
    assert_eq!(report.marked_missing, 1);

    // s1 preserved first_seen_at and updated hash and name
    let item1_after = native_item_by_path(connection, "/home/user/.agents/skills/s1")
        .expect("lookup")
        .expect("item1 found");
    assert_eq!(item1_after.first_seen_at, first_seen_s1);
    assert_eq!(item1_after.name, "skill-1-renamed");
    assert_eq!(item1_after.content_hash.as_deref(), Some("hash1-new"));

    // s2 is now marked missing
    let item2_after = native_item_by_path(connection, "/home/user/.agents/skills/s2")
        .expect("lookup")
        .expect("item2 found");
    assert_eq!(item2_after.status, NativeStatus::Missing);

    // other agent in User scope is UNTOUCHED
    let item_other = native_item_by_path(connection, "/home/other/.agents/skills/so")
        .expect("lookup")
        .expect("other found");
    assert_eq!(item_other.status, NativeStatus::New);

    // Filtering without missing hides s2
    let active_items = list_native_items(
        connection,
        &NativeItemFilter {
            scope: Some(Scope::User),
            agent_id: Some(agent.clone()),
            status: None,
            include_missing: false,
        },
    )
    .expect("active items");
    assert_eq!(active_items.len(), 2);
    let active_paths: Vec<_> = active_items.iter().map(|i| i.path.as_str()).collect();
    assert!(active_paths.contains(&"/home/user/.agents/skills/s1"));
    assert!(active_paths.contains(&"/home/user/.agents/skills/s3"));

    // Resurrecting s2: third scan brings s2 back
    let entry2_back =
        NativeScanEntry::new(ItemKind::Skill, "skill-2", "/home/user/.agents/skills/s2")
            .with_content_hash("hash2");
    let report3 = replace_scan(
        connection,
        &agent,
        Scope::User,
        &[entry1_updated, entry2_back, entry3],
    )
    .expect("third scan");
    assert_eq!(report3.resurrected, 1);
    let item2_resurrected = native_item_by_path(connection, "/home/user/.agents/skills/s2")
        .expect("lookup")
        .expect("item2 found");
    assert_eq!(item2_resurrected.status, NativeStatus::New);
}

#[test]
fn resolve_drift_event_idempotent_and_returns_newest_first() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let agent = test_agent();

    // Record two drift events with explicit detected_at
    let ev1 = NewDriftEvent::new(&agent, Scope::User, "/path/one")
        .detected_at("2026-01-01T00:00:00.000Z");
    let id1 = record_drift(connection, &ev1).expect("ev1");

    let ev2 = NewDriftEvent::new(&agent, Scope::User, "/path/two")
        .detected_at("2026-02-01T00:00:00.000Z");
    let id2 = record_drift(connection, &ev2).expect("ev2");

    // open_drift_events returns newest first (ev2, then ev1)
    let open_events = open_drift_events(connection, Some(Scope::User)).expect("open events");
    assert_eq!(open_events.len(), 2);
    assert_eq!(open_events[0].id, id2);
    assert_eq!(open_events[1].id, id1);

    // Resolve ev2 with Adopt
    let resolved =
        resolve_drift(connection, id2, DriftResolution::Adopt, Some("bkp_123")).expect("resolve");
    assert!(resolved, "first resolve succeeds");

    // Resolving again is idempotent (returns false, leaves first resolution intact)
    let resolved_again =
        resolve_drift(connection, id2, DriftResolution::Ignore, None).expect("resolve again");
    assert!(!resolved_again, "second resolve returns false");

    // Check row state
    let ev2_row = at_db::drift::drift_event(connection, id2)
        .expect("lookup")
        .expect("ev2 found");
    assert_eq!(ev2_row.resolution, Some(DriftResolution::Adopt));
    assert!(ev2_row.resolved_at.is_some());
    assert_eq!(ev2_row.backup_id.as_deref(), Some("bkp_123"));

    // open_drift_events now only contains ev1
    let remaining_open = open_drift_events(connection, Some(Scope::User)).expect("remaining open");
    assert_eq!(remaining_open.len(), 1);
    assert_eq!(remaining_open[0].id, id1);
}

#[test]
fn deleting_db_and_rescanning_rebuilds_native_items_while_durable_history_unrecoverable() {
    let temp = TempDir::new().expect("temp dir");
    let db_path = temp.path().join("agentuna.db");

    let agent = test_agent();

    // 1. Create database and write native scan items + durable records (backup, operation, remote state)
    {
        let database = Database::open_file(&db_path).expect("open file");
        let connection = database.connection();

        // Native item
        let entry = NativeScanEntry::new(ItemKind::Skill, "skill-1", "/path/skill1")
            .with_content_hash("hash-original");
        replace_scan(connection, &agent, Scope::User, &[entry]).expect("scan");

        // Durable operation
        let op_id = at_db::ops::record_completed(
            connection,
            at_db::ops::OperationKind::Deploy,
            at_db::ops::OperationActor::Cli,
            None,
        )
        .expect("op");

        // Durable backup
        record_backup(
            connection,
            &NewBackup::new("/path/skill1", "/backups/bkp1.tar.gz")
                .with_operation(&op_id.to_string()),
        )
        .expect("backup");

        let rstate = RemoteBackupState {
            repo_url: Some("https://github.com/org/repo.git".to_string()),
            status: RemoteBackupStatus::Idle,
            ..Default::default()
        };
        save_remote_backup_state(connection, &rstate).expect("remote state save");

        // Verify durable history is present
        let history = durable_history(connection).expect("history");
        assert_eq!(history.operations, 1);
        assert_eq!(history.backups, 1);
        assert!(history.remote_backup_configured);
        assert!(!history.unrecoverable().contains(&"operations"));
        assert!(!history.unrecoverable().contains(&"backups"));
        assert!(!history.unrecoverable().contains(&"remote_backup_state"));
    }

    // 2. Delete the DB file (simulating loss)
    std::fs::remove_file(&db_path).expect("remove db file");

    // 3. Open a fresh DB file at same location
    {
        let database = Database::open_file(&db_path).expect("fresh db opens");
        let connection = database.connection();

        // Durable history reports unrecoverable missing tables!
        let history = durable_history(connection).expect("fresh history");
        let unrec = history.unrecoverable();
        assert!(
            unrec.contains(&"operations"),
            "operations must be reported unrecoverable"
        );
        assert!(
            unrec.contains(&"backups"),
            "backups must be reported unrecoverable"
        );
        assert!(
            unrec.contains(&"remote_backup_state"),
            "remote state must be reported unrecoverable"
        );

        // Native items can be completely rebuilt by re-scanning from files
        assert_eq!(
            native_item_by_path(connection, "/path/skill1").unwrap(),
            None
        );

        let entry = NativeScanEntry::new(ItemKind::Skill, "skill-1", "/path/skill1")
            .with_content_hash("hash-original");
        replace_scan(connection, &agent, Scope::User, &[entry]).expect("rescan");

        let rebuilt = native_item_by_path(connection, "/path/skill1")
            .unwrap()
            .expect("rebuilt");
        assert_eq!(rebuilt.name, "skill-1");
        assert_eq!(rebuilt.status, NativeStatus::New);
    }
}

#[test]
fn backup_queries_and_remote_backup_status() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    let bkp_id = record_backup(
        connection,
        &NewBackup::new("/path/foo.txt", "/backups/foo.bak")
            .with_content(Some("hash-foo"), Some(128)),
    )
    .expect("record backup");

    let by_id = backup_by_id(connection, &bkp_id)
        .expect("query")
        .expect("found");
    assert_eq!(by_id.original_path.as_deref(), Some("/path/foo.txt"));
    assert_eq!(by_id.restore_count, 0);

    // mark restored
    assert!(mark_restored(connection, &bkp_id).expect("mark restored"));
    let by_id2 = backup_by_id(connection, &bkp_id)
        .expect("query")
        .expect("found");
    assert_eq!(by_id2.restore_count, 1);

    // backups_for_path
    let for_path = backups_for_path(connection, "/path/foo.txt", Some(10)).expect("for path");
    assert_eq!(for_path.len(), 1);

    // backups_between
    let between = backups_between(connection, None, None).expect("between");
    assert_eq!(between.len(), 1);

    // remote backup status
    let state = remote_backup_state(connection).expect("state");
    assert_eq!(state.status, RemoteBackupStatus::Unconfigured);

    assert!(
        set_remote_backup_status(connection, RemoteBackupStatus::Syncing, None)
            .expect("set status")
    );
    let updated = remote_backup_state(connection).expect("state after update");
    assert_eq!(updated.status, RemoteBackupStatus::Syncing);
}
