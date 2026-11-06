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
