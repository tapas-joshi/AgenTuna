//! Generic operation undo integration coverage.
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use at_db::{Database, ops};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder};
use at_migrate::apply::{UndoOperationRequest, UndoOutcome, undo_operation};
use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind};
#[test]
fn undo_of_linked_directory_restores_original_tree_from_directory_backup() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    let backups = root.join("backups");
    let native = root.join("native/skill");
    let canonical = store.join("skills/skill");
    fs::create_dir_all(native.join("assets")).expect("native tree");
    fs::create_dir_all(&canonical).expect("canonical tree");
    fs::create_dir_all(&backups).expect("backup root");
    fs::write(native.join("SKILL.md"), b"native skill\n").expect("native document");
    fs::write(native.join("assets/logo.svg"), b"<svg/>\n").expect("native asset");
    fs::write(canonical.join("SKILL.md"), b"native skill\n").expect("canonical document");
    let native_hash = at_fs::changeset::hash_node(&native)
        .expect("hash native tree")
        .expect("native tree exists");
    let database = Database::open_in_memory().expect("database");
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let set = ChangeSet {
        id: "replace-native".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("migrate").expect("operation kind"),
        scope: Some(at_model::Scope::User),
        project_id: None,
        summary: "Replace native skill".to_owned(),
        ops: vec![
            FileOp::Delete {
                path: native.clone(),
                expected: ExpectedState::hash(&native_hash).expect("expected native tree"),
            },
            FileOp::Symlink {
                link_path: native.clone(),
                target: canonical.clone(),
                mode: at_model::DeployMode::Link,
            },
        ],
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    };
    let roots = ApplyRoots::new(vec![root.clone()]).expect("authorized root");
    let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply replacement");
    assert!(
        fs::symlink_metadata(&native)
            .expect("replacement link")
            .file_type()
            .is_symlink()
    );
    let detail = ops::operation_detail(database.connection(), &applied.operation_id)
        .expect("original operation history")
        .expect("original operation exists");
    let original_row = detail.files.first().expect("deleted directory row");
    let backup_id = original_row
        .backup_id
        .as_deref()
        .expect("directory backup id");
    let backup = at_db::backups::backup_by_id(database.connection(), backup_id)
        .expect("backup lookup")
        .expect("directory backup row");
    let backup_path = backup
        .backup_path
        .as_deref()
        .expect("directory backup path");
    assert!(
        std::path::Path::new(backup_path).is_dir(),
        "directory backup: {backup_path}"
    );

    let result = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
        },
        &applied.operation_id,
    )
    .expect("undo replacement");
    assert!(matches!(result, UndoOutcome::Restored { .. }));
    assert_eq!(
        fs::read(native.join("SKILL.md")).expect("restored document"),
        b"native skill\n"
    );
    assert_eq!(
        fs::read(native.join("assets/logo.svg")).expect("restored asset"),
        b"<svg/>\n"
    );
    assert!(
        !fs::symlink_metadata(&native)
            .expect("restored real directory")
            .file_type()
            .is_symlink()
    );
}
use tempfile::TempDir;

#[test]
fn undo_of_created_file_removes_it_and_records_a_linked_undo_operation() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let backups = root.join("backups");
    let store = root.join("store");
    fs::create_dir_all(&store).expect("store root");
    fs::create_dir_all(&backups).expect("backup root");
    let target = root.join("native/item.txt");
    let database = Database::open_in_memory().expect("database");
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &store,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let change_set = ChangeSet {
        id: "create-file".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("migrate").expect("operation kind"),
        scope: Some(at_model::Scope::User),
        project_id: None,
        summary: "Create fixture file".to_owned(),
        ops: vec![FileOp::Write {
            path: target.clone(),
            bytes: b"created by operation".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    };
    let roots = ApplyRoots::new(vec![root.clone()]).expect("authorized root");
    let applied =
        at_fs::apply_with_roots(&change_set, &mut context, &roots).expect("apply operation");
    assert_eq!(
        fs::read(&target).expect("created file"),
        b"created by operation"
    );

    let outcome = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
        },
        &applied.operation_id,
    )
    .expect("undo operation");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected completed undo, got {other:?}"),
    };
    assert!(!target.exists());
    let undo = ops::operation_detail(database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo operation row");
    assert_eq!(
        undo.operation.undo_of.as_deref(),
        Some(applied.operation_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// TC-010: every operation kind, migrate metadata cleanup, window, idempotency.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{BuildPlanRequest, MigrationStrategy, build_plan};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ItemId, ItemKind, Scope};

struct MigrateFixture {
    _temp: TempDir,
    root: PathBuf,
    store: PathBuf,
    backups: PathBuf,
    native: PathBuf,
    database: Database,
    native_hash: String,
    store_manifest_bytes: Vec<u8>,
}

fn migrate_fixture() -> MigrateFixture {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    at_store::layout::init_store(Scope::User, &store).expect("initialised store");
    let backups = root.join("backups");
    fs::create_dir_all(&backups).expect("backup root");
    let native = root.join("native/skills/sample-skill");
    fs::create_dir_all(&native).expect("native tree");
    fs::write(native.join("SKILL.md"), b"# Sample\nbody\n").expect("native document");
    let native_hash = at_fs::changeset::hash_node(&native)
        .expect("hash native tree")
        .expect("native tree");
    let store_manifest_bytes = fs::read(store.join("store.json")).expect("manifest bytes");
    let database = Database::open_in_memory().expect("database");
    MigrateFixture {
        _temp: temp,
        root,
        store,
        backups,
        native,
        database,
        native_hash,
        store_manifest_bytes,
    }
}

fn migrate_plan(fixture: &MigrateFixture) -> at_migrate::plan::MigrationPlan {
    let normalized = at_store::manifest::hash_skill_folder(&fixture.native)
        .expect("skill hash")
        .to_string();
    let item = ScanItem {
        kind: ItemKind::Skill,
        name: "sample-skill".to_owned(),
        agents: BTreeSet::from([AgentId::ClaudeCode]),
        path: fixture.native.clone(),
        scope: Scope::User,
        content_hash: Some(fixture.native_hash.clone()),
        normalized_hash: Some(normalized),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };
    let scan = ScanReport {
        items: vec![item],
        partial: false,
        skipped_roots: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    let selected = BTreeSet::from([fixture.native.clone()]);
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected,
        store_root: fixture.store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: fixture.backups.clone(),
    })
    .expect("migration plan")
}

fn apply_migrate(fixture: &mut MigrateFixture) -> at_migrate::apply::MigrationApplyOutcome {
    let plan = migrate_plan(fixture);
    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let mut context = ApplyPlanContext {
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &roots,
        database: &mut fixture.database,
    };
    apply_plan(&plan, &mut context).expect("apply migration")
}

fn undo_request<'a>(
    fixture: &'a MigrateFixture,
    roots: &'a ApplyRoots,
) -> UndoOperationRequest<'a> {
    UndoOperationRequest {
        connection: fixture.database.connection(),
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: roots,
        actor: ops::OperationActor::Ui,
    }
}

fn tree_hash(path: &Path) -> String {
    at_fs::changeset::hash_node(path)
        .expect("hash tree")
        .expect("tree exists")
}

#[test]
fn undo_of_a_migrate_operation_restores_originals_and_removes_metadata() {
    let mut fixture = migrate_fixture();
    let applied = apply_migrate(&mut fixture);
    let store_item = fixture.store.join("skills/sample-skill");
    assert!(store_item.join("SKILL.md").is_file(), "store item created");
    assert!(
        fs::symlink_metadata(&fixture.native)
            .expect("link")
            .file_type()
            .is_symlink(),
        "original replaced by a link"
    );
    let item_id = at_db::items::item_id_by_identity(
        fixture.database.connection(),
        ItemKind::Skill,
        Scope::User,
        None,
        "sample-skill",
    )
    .expect("item lookup")
    .expect("item row written");
    assert_eq!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .len(),
        1
    );
    let native_text = fixture.native.to_string_lossy().into_owned();
    assert_eq!(
        at_db::native::native_item_by_path(fixture.database.connection(), &native_text)
            .expect("native lookup")
            .expect("native row")
            .status,
        at_db::native::NativeStatus::Adopted
    );

    let roots = ApplyRoots::new([fixture.root.clone()]).expect("authorized root");
    let outcome = undo_operation(undo_request(&fixture, &roots), &applied.operation_id)
        .expect("undo migration");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected completed undo, got {other:?}"),
    };

    // Files: the original tree is back, the store item and its link are gone.
    assert_eq!(tree_hash(&fixture.native), fixture.native_hash);
    assert!(
        !fs::symlink_metadata(&fixture.native)
            .expect("restored tree")
            .file_type()
            .is_symlink()
    );
    assert!(!store_item.exists(), "store item removed");
    assert_eq!(
        fs::read(fixture.store.join("store.json")).expect("manifest"),
        fixture.store_manifest_bytes,
        "manifest restored to its pre-migration bytes"
    );

    // Metadata: deployments, native adoption and the item row are gone.
    assert!(
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments")
            .is_empty()
    );
    let native = at_db::native::native_item_by_path(fixture.database.connection(), &native_text)
        .expect("native lookup")
        .expect("native row survives");
    assert_eq!(native.status, at_db::native::NativeStatus::New);
    assert!(native.adopted_item_id.is_none());
    assert!(
        at_db::items::item_id_by_identity(
            fixture.database.connection(),
            ItemKind::Skill,
            Scope::User,
            None,
            "sample-skill"
        )
        .expect("item lookup")
        .is_none(),
        "item row removed with the store files"
    );

    // The undo is a single Undo record linked to the original.
    let undo = ops::operation_detail(fixture.database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo operation row");
    assert_eq!(undo.operation.kind, Some(ops::OperationKind::Undo));
    assert_eq!(
        undo.operation.undo_of.as_deref(),
        Some(applied.operation_id.as_str())
    );
    assert!(!undo.files.is_empty());

    // A second undo refuses.
    assert_eq!(
        undo_operation(undo_request(&fixture, &roots), &applied.operation_id).expect("second undo"),
        UndoOutcome::AlreadyUndone
    );

    // The first undo is itself undoable: the migrated state comes back.
    let redo = undo_operation(undo_request(&fixture, &roots), &undo_id).expect("undo the undo");
    assert!(matches!(redo, UndoOutcome::Restored { .. }));
    assert_eq!(tree_hash(&store_item), fixture.native_hash);
    assert!(
        fs::symlink_metadata(&fixture.native)
            .expect("link again")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn undo_of_a_drift_adopt_or_sync_push_restores_recorded_bytes_with_its_own_backup() {
    for kind in ["drift_adopt", "sync_push"] {
        let temp = TempDir::new().expect("temporary root");
        let root = fs::canonicalize(temp.path()).expect("canonical root");
        let store = root.join("store");
        let backups = root.join("backups");
        fs::create_dir_all(&store).expect("store root");
        fs::create_dir_all(&backups).expect("backup root");
        let target = store.join("rules/team-rules.md");
        fs::create_dir_all(target.parent().expect("parent")).expect("rules dir");
        fs::write(&target, b"before drift\n").expect("seed rule");
        let database = Database::open_in_memory().expect("database");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let set = ChangeSet {
            id: format!("{kind}-operation"),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse(kind).expect("operation kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: format!("{kind} wrote the rule"),
            ops: vec![FileOp::Write {
                path: target.clone(),
                bytes: b"adopted external bytes\n".to_vec(),
                expected: ExpectedState::hash(at_fs::sha256_hex(b"before drift\n"))
                    .expect("expected hash"),
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        };
        let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
        let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply operation");
        assert_eq!(
            fs::read(&target).expect("written bytes"),
            b"adopted external bytes\n"
        );

        let outcome = undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Cli,
            },
            &applied.operation_id,
        )
        .expect("undo operation");
        let undo_id = match outcome {
            UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
            other => panic!("expected completed undo for {kind}, got {other:?}"),
        };
        assert_eq!(
            fs::read(&target).expect("restored bytes"),
            b"before drift\n",
            "{kind} restores the recorded bytes"
        );
        let own_backups = at_db::backups::backups_for_operation(database.connection(), &undo_id)
            .expect("undo backups");
        assert_eq!(own_backups.len(), 1, "{kind} undo creates its own backup");
        let blob = own_backups[0].backup_path.as_deref().expect("backup path");
        assert_eq!(
            fs::read(blob).expect("backup bytes"),
            b"adopted external bytes\n",
            "{kind} undo backs up the bytes it replaces"
        );
    }
}

#[test]
fn undo_window_is_thirty_days_evaluated_in_utc() {
    for (age_days, expected) in [(29_u64, None), (31_u64, Some(UndoOutcome::Expired))] {
        let temp = TempDir::new().expect("temporary root");
        let root = fs::canonicalize(temp.path()).expect("canonical root");
        let store = root.join("store");
        let backups = root.join("backups");
        fs::create_dir_all(&store).expect("store root");
        fs::create_dir_all(&backups).expect("backup root");
        let target = root.join("native/item.txt");
        let database = Database::open_in_memory().expect("database");
        let mut recorder = DbOperationRecorder::new(database.connection(), 30);
        let mut context = ApplyContext {
            store_root: &store,
            backup_folder: &backups,
            recorder: &mut recorder,
        };
        let set = ChangeSet {
            id: format!("window-{age_days}"),
            preview_id: "preview".to_owned(),
            operation_kind: OperationKind::parse("deploy").expect("operation kind"),
            scope: Some(Scope::User),
            project_id: None,
            summary: "window fixture".to_owned(),
            ops: vec![FileOp::Write {
                path: target.clone(),
                bytes: b"deployed\n".to_vec(),
                expected: ExpectedState::Absent,
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        };
        let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
        let applied = at_fs::apply_with_roots(&set, &mut context, &roots).expect("apply operation");
        database
            .connection()
            .execute(
                "UPDATE operations SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) \
                  WHERE id = ?1",
                rusqlite::params![applied.operation_id, format!("-{age_days} days")],
            )
            .expect("age the operation");
        let outcome = undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Ui,
            },
            &applied.operation_id,
        )
        .expect("undo window check");
        match expected {
            None => {
                assert!(
                    matches!(outcome, UndoOutcome::Restored { .. }),
                    "day {age_days} is inside the window, got {outcome:?}"
                );
                assert!(!target.exists(), "day {age_days} undo removed the file");
            }
            Some(expected) => {
                assert_eq!(outcome, expected, "day {age_days} is outside the window");
                assert_eq!(
                    fs::read(&target).expect("file kept"),
                    b"deployed\n",
                    "an expired undo writes nothing"
                );
            }
        }
    }
}

#[test]
fn undo_of_an_operation_without_file_rows_is_recorded_and_idempotent() {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    let backups = root.join("backups");
    fs::create_dir_all(&store).expect("store root");
    fs::create_dir_all(&backups).expect("backup root");
    let database = Database::open_in_memory().expect("database");
    let operation = ops::record_completed(
        database.connection(),
        ops::OperationKind::Scan,
        ops::OperationActor::Cli,
        Some("scanned agents"),
    )
    .expect("record scan operation");
    let roots = ApplyRoots::new([root.clone()]).expect("authorized root");
    let outcome = undo_operation(
        UndoOperationRequest {
            connection: database.connection(),
            store_root: &store,
            backup_root: &backups,
            authorized_roots: &roots,
            actor: ops::OperationActor::Ui,
        },
        operation.as_str(),
    )
    .expect("undo scan");
    let undo_id = match outcome {
        UndoOutcome::Restored { undo_operation_id } => undo_operation_id,
        other => panic!("expected recorded undo, got {other:?}"),
    };
    let undo = ops::operation_detail(database.connection(), &undo_id)
        .expect("undo history")
        .expect("undo row");
    assert_eq!(undo.operation.kind, Some(ops::OperationKind::Undo));
    assert!(undo.files.is_empty());
    assert_eq!(
        undo_operation(
            UndoOperationRequest {
                connection: database.connection(),
                store_root: &store,
                backup_root: &backups,
                authorized_roots: &roots,
                actor: ops::OperationActor::Ui,
            },
            operation.as_str(),
        )
        .expect("second undo"),
        UndoOutcome::AlreadyUndone
    );
    let _ = ItemId::new();
}
