//! End-to-end tests for restoring catalogue backups in an isolated store.
use std::fs;
use std::path::{Path, PathBuf};

use at_db::{Database, backups, ops};
use at_fs::{RestoreOutcome, RestoreRequest, backup, restore_backup, sha256_hex};
use tempfile::TempDir;
use time::OffsetDateTime;

struct Fixture {
    _temp: TempDir,
    database: Database,
    backup_id: String,
    target: PathBuf,
    root: PathBuf,
    backup_blob: PathBuf,
}
fn fixture(target_contents: Option<&[u8]>, backup_contents: &[u8]) -> Fixture {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("store");
    fs::create_dir_all(&root).expect("store root");
    let relative = Path::new("nested/deeper/item.txt");
    let target = root.join(relative);
    if let Some(contents) = target_contents {
        fs::create_dir_all(target.parent().expect("parent")).expect("target parent");
        fs::write(&target, contents).expect("target bytes");
    }
    let source_backup_dir = temp.path().join("catalogue-seed");
    fs::create_dir_all(&source_backup_dir).expect("catalogue backup folder");
    let files = backup::copy_into(
        &source_backup_dir,
        &target,
        backup_contents,
        &backup::BackupMeta {
            relpath: Some(relative.to_string_lossy().into_owned()),
            mode: Some(0o640),
            ..backup::BackupMeta::default()
        },
        OffsetDateTime::now_utc(),
    )
    .expect("seed backup");
    let backup_blob = files.blob.clone();
    let database = Database::open_in_memory().expect("database");
    let original = relative.to_string_lossy().into_owned();
    let blob_path = files.blob.to_string_lossy().into_owned();
    let backup_id = backups::record_backup(
        database.connection(),
        &backups::NewBackup::new(&original, &blob_path).with_content(
            Some(&sha256_hex(backup_contents)),
            Some(backup_contents.len() as i64),
        ),
    )
    .expect("catalogue row");
    Fixture {
        _temp: temp,
        database,
        backup_id,
        target,
        root,
        backup_blob,
    }
}
fn request<'a>(fixture: &'a Fixture, confirmation: Option<&'a str>) -> RestoreRequest<'a> {
    RestoreRequest {
        connection: fixture.database.connection(),
        store_root: &fixture.root,
        backup_id: &fixture.backup_id,
        confirmation,
        actor: ops::OperationActor::Cli,
    }
}

fn operation_count(database: &Database) -> i64 {
    database
        .connection()
        .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
        .expect("operation count")
}

#[test]
fn restores_deleted_file_byte_for_byte_and_recreates_parent_directories() {
    let fixture = fixture(None, b"original\0bytes\n");
    let operation_id = match restore_backup(request(&fixture, None)).expect("restore") {
        RestoreOutcome::Restored { operation_id, .. } => operation_id,
        other => panic!("expected restore, got {other:?}"),
    };
    assert_eq!(
        fs::read(&fixture.target).expect("restored bytes"),
        b"original\0bytes\n"
    );
    assert!(fixture.target.parent().expect("parent").is_dir());
    let row = backups::backup_by_id(fixture.database.connection(), &fixture.backup_id)
        .expect("catalogue query")
        .expect("original backup");
    assert_eq!(row.restore_count, 1);
    assert_eq!(operation_count(&fixture.database), 1);
    let history = ops::operation_detail(fixture.database.connection(), &operation_id)
        .expect("operation query")
        .expect("restore history");
    assert_eq!(history.files.len(), 1);
    assert_eq!(history.files[0].action, Some(ops::FileAction::Created));
    assert_eq!(
        history.files[0].after_hash.as_deref(),
        Some(sha256_hex(b"original\0bytes\n").as_str())
    );
}

#[test]
fn changed_target_requires_confirmation_then_backs_up_current_bytes_and_records_history() {
    let fixture = fixture(Some(b"before edit\n"), b"saved version\n");
    let preview = restore_backup(request(&fixture, None)).expect("preview");
    let (token, diff) = match preview {
        RestoreOutcome::ConfirmationRequired { token, diff } => (token, diff),
        other => panic!("expected confirmation, got {other:?}"),
    };
    assert!(diff.contains("before edit"));
    assert!(diff.contains("saved version"));
    assert_eq!(
        fs::read(&fixture.target).expect("untouched target"),
        b"before edit\n"
    );
    assert_eq!(operation_count(&fixture.database), 0);
    let original = backups::backup_by_id(fixture.database.connection(), &fixture.backup_id)
        .expect("catalogue query")
        .expect("original backup");
    assert_eq!(original.restore_count, 0);

    let restored = restore_backup(request(&fixture, Some(&token))).expect("confirmed restore");
    let operation_id = match restored {
        RestoreOutcome::Restored { operation_id, .. } => operation_id,
        other => panic!("expected restore, got {other:?}"),
    };
    assert_eq!(
        fs::read(&fixture.target).expect("restored target"),
        b"saved version\n"
    );
    assert_eq!(operation_count(&fixture.database), 1);
    let history = ops::operation_detail(fixture.database.connection(), &operation_id)
        .expect("operation detail")
        .expect("restore history");
    assert_eq!(history.files.len(), 1);
    assert_eq!(
        history.files[0].before_hash.as_deref(),
        Some(sha256_hex(b"before edit\n").as_str())
    );
    assert_eq!(
        history.files[0].after_hash.as_deref(),
        Some(sha256_hex(b"saved version\n").as_str())
    );
    let current_backups =
        backups::backups_for_operation(fixture.database.connection(), &operation_id)
            .expect("current-state backup row");
    assert_eq!(current_backups.len(), 1);
    assert_eq!(
        current_backups[0].content_hash.as_deref(),
        Some(sha256_hex(b"before edit\n").as_str())
    );
    assert_eq!(
        fs::read(
            current_backups[0]
                .backup_path
                .as_deref()
                .expect("backup path")
        )
        .expect("backup bytes"),
        b"before edit\n"
    );
    assert_eq!(
        backups::backup_by_id(fixture.database.connection(), &fixture.backup_id)
            .expect("catalogue query")
            .expect("original backup")
            .restore_count,
        1
    );
}

#[test]
fn stale_confirmation_refuses_to_overwrite_a_newer_external_edit() {
    let fixture = fixture(Some(b"first edit"), b"saved bytes");
    let token = match restore_backup(request(&fixture, None)).expect("preview") {
        RestoreOutcome::ConfirmationRequired { token, .. } => token,
        other => panic!("expected confirmation, got {other:?}"),
    };
    fs::write(&fixture.target, b"newer external edit").expect("external edit");
    let error = restore_backup(request(&fixture, Some(&token))).expect_err("stale token refusal");
    assert!(matches!(error, at_fs::RestoreError::StaleConfirmation));
    assert_eq!(
        fs::read(&fixture.target).expect("preserved external edit"),
        b"newer external edit"
    );
    assert_eq!(operation_count(&fixture.database), 0);
    assert_eq!(
        backups::backup_by_id(fixture.database.connection(), &fixture.backup_id)
            .expect("catalogue query")
            .expect("original backup")
            .restore_count,
        0
    );
}

#[test]
fn corrupt_blob_is_refused_without_target_or_history_changes() {
    let fixture = fixture(Some(b"existing"), b"saved bytes");
    fs::write(&fixture.backup_blob, b"corrupted blob").expect("corrupt seed blob");
    let error = restore_backup(request(&fixture, None)).expect_err("corrupt backup refusal");
    assert!(matches!(error, at_fs::RestoreError::Backup(_)));
    assert_eq!(
        fs::read(&fixture.target).expect("preserved target"),
        b"existing"
    );
    assert_eq!(operation_count(&fixture.database), 0);
    assert_eq!(
        backups::backup_by_id(fixture.database.connection(), &fixture.backup_id)
            .expect("catalogue query")
            .expect("original backup")
            .restore_count,
        0
    );
}

#[test]
fn restores_directory_tree_from_catalogue_backup() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("store");
    let target = root.join("native/skill");
    let backup_tree = temp.path().join("catalogue-tree/skill");
    fs::create_dir_all(backup_tree.join("assets")).expect("backup tree");
    fs::write(backup_tree.join("SKILL.md"), b"saved skill\n").expect("backup document");
    fs::write(backup_tree.join("assets/logo.svg"), b"<svg/>\n").expect("backup asset");
    fs::create_dir_all(&root).expect("store root");
    let hash = at_fs::changeset::hash_node(&backup_tree)
        .expect("hash tree")
        .expect("tree exists");
    let database = Database::open_in_memory().expect("database");
    let original = "native/skill";
    let backup_path = backup_tree.to_string_lossy().into_owned();
    let backup_id = backups::record_backup(
        database.connection(),
        &backups::NewBackup::new(original, &backup_path).with_content(Some(&hash), None),
    )
    .expect("catalogue row");
    let outcome = restore_backup(RestoreRequest {
        connection: database.connection(),
        store_root: &root,
        backup_id: &backup_id,
        confirmation: None,
        actor: ops::OperationActor::Ui,
    })
    .expect("restore tree");
    assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    assert_eq!(
        at_fs::changeset::hash_node(&target)
            .expect("hash restored tree")
            .as_deref(),
        Some(hash.as_str())
    );
}
