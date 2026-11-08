//! End-to-end tests for restoring catalogue backups in an isolated store.

// Unix-only harness: POSIX symlinks, permission bits and raw errno constants. The Windows
// twins arrive with the slice that enables the Windows test suite.
#![cfg(unix)]
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

// ---------------------------------------------------------------------------
// Directory-tree restores: modes, confirmation, external writers, recovery.
// ---------------------------------------------------------------------------

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use at_fs::failpoint::{FailInjector, Fault, FaultPoint, TestInjector, with_injector};

/// Writes into a live tree when the restore re-reads it, standing in for another process.
struct TreeWriter {
    tree: PathBuf,
    relative: &'static str,
    bytes: &'static [u8],
}

impl FailInjector for TreeWriter {
    fn fault(&self, point: FaultPoint, path: &Path) -> Option<Fault> {
        if point == FaultPoint::RestoreRecheck && path == self.tree {
            let _ = fs::write(self.tree.join(self.relative), self.bytes);
        }
        None
    }
}

fn tree_hash(path: &Path) -> String {
    at_fs::changeset::hash_node(path)
        .expect("hash tree")
        .expect("tree exists")
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

fn stage_debris(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".agentuna-restore-stage-"))
        })
        .collect()
}

fn journals(root: &Path) -> Vec<PathBuf> {
    let backups = root.join("backups");
    let Ok(entries) = fs::read_dir(&backups) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("restore-journal.json"))
        .filter(|path| path.is_file())
        .collect()
}

struct TreeFixture {
    _temp: TempDir,
    database: Database,
    backup_id: String,
    root: PathBuf,
    target: PathBuf,
    live_hash: String,
    backup_hash: String,
}

/// A live tree at `native/skill` and a different catalogue tree recorded as its backup.
fn tree_fixture() -> TreeFixture {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("store");
    let target = root.join("native/skill");
    fs::create_dir_all(target.join("assets")).expect("live tree");
    fs::write(target.join("SKILL.md"), b"live skill\n").expect("live document");
    fs::write(target.join("assets/logo.svg"), b"<svg>live</svg>\n").expect("live asset");
    fs::set_permissions(target.join("SKILL.md"), fs::Permissions::from_mode(0o644))
        .expect("live mode");

    let backup_tree = temp.path().join("catalogue-tree/skill");
    fs::create_dir_all(backup_tree.join("assets")).expect("backup tree");
    fs::write(backup_tree.join("SKILL.md"), b"saved skill\n").expect("backup document");
    fs::write(backup_tree.join("assets/logo.svg"), b"<svg>saved</svg>\n").expect("backup asset");
    fs::set_permissions(
        backup_tree.join("SKILL.md"),
        fs::Permissions::from_mode(0o640),
    )
    .expect("backup document mode");
    fs::set_permissions(
        backup_tree.join("assets"),
        fs::Permissions::from_mode(0o750),
    )
    .expect("backup asset dir mode");

    let live_hash = tree_hash(&target);
    let backup_hash = tree_hash(&backup_tree);
    assert_ne!(live_hash, backup_hash);
    let database = Database::open_in_memory().expect("database");
    let backup_path = backup_tree.to_string_lossy().into_owned();
    let backup_id = backups::record_backup(
        database.connection(),
        &backups::NewBackup::new("native/skill", &backup_path)
            .with_content(Some(&backup_hash), None),
    )
    .expect("catalogue row");
    TreeFixture {
        _temp: temp,
        database,
        backup_id,
        root,
        target,
        live_hash,
        backup_hash,
    }
}

fn tree_request<'a>(fixture: &'a TreeFixture, confirmation: Option<&'a str>) -> RestoreRequest<'a> {
    RestoreRequest {
        connection: fixture.database.connection(),
        store_root: &fixture.root,
        backup_id: &fixture.backup_id,
        confirmation,
        actor: ops::OperationActor::Ui,
    }
}

fn confirmation_token(fixture: &TreeFixture) -> String {
    match restore_backup(tree_request(fixture, None)).expect("preview") {
        RestoreOutcome::ConfirmationRequired { token, .. } => token,
        other => panic!("expected confirmation, got {other:?}"),
    }
}

#[test]
fn directory_restore_preserves_modes_and_records_the_replaced_tree() {
    let fixture = tree_fixture();
    let token = confirmation_token(&fixture);
    let outcome = restore_backup(tree_request(&fixture, Some(&token))).expect("confirmed restore");
    let operation_id = match outcome {
        RestoreOutcome::Restored {
            operation_id,
            target,
        } => {
            assert_eq!(target, fixture.target);
            operation_id
        }
        other => panic!("expected restore, got {other:?}"),
    };
    assert_eq!(tree_hash(&fixture.target), fixture.backup_hash);
    assert_eq!(mode_of(&fixture.target.join("SKILL.md")), 0o640);
    assert_eq!(mode_of(&fixture.target.join("assets")), 0o750);
    assert_eq!(
        fs::read(fixture.target.join("assets/logo.svg")).expect("restored asset"),
        b"<svg>saved</svg>\n"
    );
    let detail = ops::operation_detail(fixture.database.connection(), &operation_id)
        .expect("operation detail")
        .expect("restore history");
    assert_eq!(detail.operation.kind, Some(ops::OperationKind::Restore));
    assert_eq!(detail.files.len(), 1);
    assert_eq!(
        detail.files[0].before_hash.as_deref(),
        Some(fixture.live_hash.as_str())
    );
    assert_eq!(
        detail.files[0].after_hash.as_deref(),
        Some(fixture.backup_hash.as_str())
    );
    let preserved = backups::backups_for_operation(fixture.database.connection(), &operation_id)
        .expect("preserved tree row");
    assert_eq!(preserved.len(), 1);
    assert_eq!(
        preserved[0].content_hash.as_deref(),
        Some(fixture.live_hash.as_str())
    );
    let preserved_path = preserved[0].backup_path.as_deref().expect("backup path");
    assert_eq!(tree_hash(Path::new(preserved_path)), fixture.live_hash);
    assert!(journals(&fixture.root).is_empty(), "journal is removed");
    assert!(stage_debris(fixture.target.parent().expect("parent")).is_empty());
}

#[test]
fn directory_restore_refuses_a_newer_external_edit_inside_the_tree() {
    let fixture = tree_fixture();
    let token = confirmation_token(&fixture);
    let writer = Arc::new(TreeWriter {
        tree: fixture.target.clone(),
        relative: "SKILL.md",
        bytes: b"newer external edit\n",
    });
    let error = with_injector(writer, || {
        restore_backup(tree_request(&fixture, Some(&token)))
    })
    .expect_err("external edit refusal");
    assert!(matches!(error, at_fs::RestoreError::StaleConfirmation));
    assert_eq!(
        fs::read(fixture.target.join("SKILL.md")).expect("external bytes"),
        b"newer external edit\n"
    );
    assert!(stage_debris(fixture.target.parent().expect("parent")).is_empty());
    assert!(journals(&fixture.root).is_empty());
    let changes = at_db::ops::recent_changes(
        fixture.database.connection(),
        &at_db::ops::RecentChangesQuery::new(5),
    )
    .expect("recent changes");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].status, Some(ops::OperationStatus::Failed));
    assert_eq!(changes[0].error_kind.as_deref(), Some("StaleConfirmation"));
}

#[test]
fn directory_restore_failure_at_install_restores_the_previous_tree() {
    let fixture = tree_fixture();
    let token = confirmation_token(&fixture);
    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::RestoreInstall, Fault::Fail(libc::EIO));
    let error = with_injector(injector, || {
        restore_backup(tree_request(&fixture, Some(&token)))
    })
    .expect_err("injected install failure");
    assert!(matches!(error, at_fs::RestoreError::Io { .. }));
    assert_eq!(
        tree_hash(&fixture.target),
        fixture.live_hash,
        "the live tree is back byte for byte"
    );
    assert!(stage_debris(fixture.target.parent().expect("parent")).is_empty());
    assert!(journals(&fixture.root).is_empty());
    let changes = at_db::ops::recent_changes(
        fixture.database.connection(),
        &at_db::ops::RecentChangesQuery::new(5),
    )
    .expect("recent changes");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].status, Some(ops::OperationStatus::Failed));

    // The same reviewed token still applies once the failure is gone.
    let outcome = restore_backup(tree_request(&fixture, Some(&token))).expect("retried restore");
    assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    assert_eq!(tree_hash(&fixture.target), fixture.backup_hash);
}

#[test]
fn interrupted_directory_restore_is_repaired_before_the_next_restore() {
    let fixture = tree_fixture();
    let token = confirmation_token(&fixture);
    // Simulate a process that died after moving the live tree aside but before installing.
    let operation_folder = fixture.root.join("backups/2000-01-01-crash");
    fs::create_dir_all(&operation_folder).expect("crash operation folder");
    let preserved = operation_folder.join("current-tree");
    fs::rename(&fixture.target, &preserved).expect("moved aside");
    let parent = fixture.target.parent().expect("parent").to_path_buf();
    let stage = parent.join(".agentuna-restore-stage-crash");
    fs::create_dir_all(&stage).expect("staging debris");
    fs::write(stage.join("SKILL.md"), b"half staged\n").expect("staging debris file");
    let journal = serde_json::json!({
        "target": fixture.target.to_string_lossy(),
        "stage": stage.to_string_lossy(),
        "current_backup": preserved.to_string_lossy(),
    });
    fs::write(
        operation_folder.join("restore-journal.json"),
        serde_json::to_vec(&journal).expect("journal bytes"),
    )
    .expect("crash journal");
    assert!(!fixture.target.exists());

    // Recovery runs before the restore: the live tree comes back, debris disappears.
    let preview = restore_backup(tree_request(&fixture, None)).expect("preview after recovery");
    assert!(matches!(
        preview,
        RestoreOutcome::ConfirmationRequired { .. }
    ));
    assert_eq!(tree_hash(&fixture.target), fixture.live_hash);
    assert!(!stage.exists(), "staging debris removed");
    assert!(
        !operation_folder.join("restore-journal.json").exists(),
        "journal cleared"
    );
    assert!(!preserved.exists(), "preserved tree moved back");

    let outcome = restore_backup(tree_request(&fixture, Some(&token))).expect("restored");
    assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    assert_eq!(tree_hash(&fixture.target), fixture.backup_hash);
}

#[test]
fn directory_restore_creates_an_absent_tree_and_records_created_action() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("store");
    fs::create_dir_all(&root).expect("store root");
    let backup_tree = temp.path().join("catalogue-tree/skill");
    fs::create_dir_all(&backup_tree).expect("backup tree");
    fs::write(backup_tree.join("SKILL.md"), b"saved skill\n").expect("backup document");
    let hash = tree_hash(&backup_tree);
    let database = Database::open_in_memory().expect("database");
    let backup_path = backup_tree.to_string_lossy().into_owned();
    let backup_id = backups::record_backup(
        database.connection(),
        &backups::NewBackup::new("native/skill", &backup_path).with_content(Some(&hash), None),
    )
    .expect("catalogue row");
    let outcome = restore_backup(RestoreRequest {
        connection: database.connection(),
        store_root: &root,
        backup_id: &backup_id,
        confirmation: None,
        actor: ops::OperationActor::Cli,
    })
    .expect("create absent tree");
    let operation_id = match outcome {
        RestoreOutcome::Restored { operation_id, .. } => operation_id,
        other => panic!("expected restore, got {other:?}"),
    };
    assert_eq!(tree_hash(&root.join("native/skill")), hash);
    let detail = ops::operation_detail(database.connection(), &operation_id)
        .expect("operation detail")
        .expect("restore history");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].action, Some(ops::FileAction::Created));
    assert!(detail.files[0].before_hash.is_none());
    assert_eq!(detail.files[0].after_hash.as_deref(), Some(hash.as_str()));
}
