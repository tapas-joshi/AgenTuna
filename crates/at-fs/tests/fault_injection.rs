//! TF-049: Fault injection suite for the at-fs safe write pipeline.
//!
//! Table-driven integration scenarios:
//! (1) external writer modifies the destination between read and rename;
//! (2) EACCES creating the temp file;
//! (3) ENOSPC during write and during fsync;
//! (4) EPERM on symlink creation;
//! (5) EXDEV on rename for a link;
//! (6) SIGKILL between backup and rename;
//! (7) disk full while writing a backup;
//! (8) read-only destination volume.
//!
//! Asserts typed error kinds, backup existence, destination bytes integrity (never partial),
//! zero temp leftovers, at-db operation row failure, and crash recovery idempotency.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use at_db::Database;
use at_db::ops::{
    NewOperation, OperationActor, OperationKind, OperationRow, OperationStatus, RecentChangesQuery,
    begin, finish, recent_changes, record_interrupted,
};
use at_fs::atomic::{Stage, apply_with_hook};
use at_fs::failpoint::{Fault, FaultPoint, TestInjector, with_injector};
use at_fs::recovery::{RestoreRequest, is_temp_file, restore_file, sweep_stale_temp_files};
use at_fs::{
    BackupMeta, BackupPolicy, DeployRequest, ExpectedState, FsError, LinkMode, WriteRequest, apply,
    deploy, sha256_hex,
};
use tempfile::TempDir;

fn count_tmp_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| is_temp_file(&e.file_name().to_string_lossy()))
        .count()
}

fn open_test_db() -> Database {
    Database::open_in_memory().expect("in-memory test database opens")
}

fn latest_op(db: &Database) -> OperationRow {
    let changes = recent_changes(db.connection(), &RecentChangesQuery::new(1))
        .expect("recent changes query succeeds");
    let change = changes.first().expect("at least one operation row exists");
    OperationRow {
        id: change.id.clone(),
        kind: change.kind,
        scope: change.scope,
        actor: change.actor,
        status: change.status,
        summary: change.summary.clone(),
        error_kind: change.error_kind.clone(),
        started_at: change.started_at.clone(),
        finished_at: change.finished_at.clone(),
        correlation_id: None,
        undo_of: None,
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: External writer modifies destination between read and rename
// ---------------------------------------------------------------------------
#[test]
fn scenario_1_external_writer_race_detected_and_preserves_external_bytes() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("config.toml");
    let initial_bytes = b"key = \"initial\"\n";
    fs::write(&dest, initial_bytes).expect("seed file");
    let initial_hash = sha256_hex(initial_bytes);

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
    )
    .expect("begin op");

    let external_bytes = b"key = \"concurrently_edited\"\n".to_vec();
    let injector = Arc::new(TestInjector::new());
    injector.arm(
        FaultPoint::BeforeRename,
        Fault::ExternalWrite(external_bytes.clone()),
    );

    let new_bytes = b"key = \"new_agen_tuna\"\n".to_vec();
    let req = WriteRequest::replace(&dest, new_bytes, ExpectedState::Hash(initial_hash));

    let result = with_injector(injector, || apply(&req));

    assert!(
        matches!(result, Err(FsError::ConcurrentModification { .. })),
        "expected ConcurrentModification, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("ConcurrentModification"),
    )
    .expect("finish op failed");

    // Bytes must equal the external writer's content, never partial or empty
    let on_disk = fs::read(&dest).expect("read dest");
    assert_eq!(on_disk, external_bytes, "external bytes must remain intact");

    // Zero temporary files remain
    assert_eq!(
        count_tmp_files(dir.path()),
        0,
        "zero temp files should remain"
    );

    // Operation status is failed rather than succeeded
    let op = latest_op(&db);
    assert_eq!(op.status, Some(OperationStatus::Failed));
    assert_eq!(op.error_kind.as_deref(), Some("ConcurrentModification"));
}

// ---------------------------------------------------------------------------
// Scenario 2: EACCES creating the temp file
// ---------------------------------------------------------------------------
#[test]
fn scenario_2_eacces_creating_temp_file_fails_with_permission_denied() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("skill.json");
    let initial_bytes = b"{\"name\": \"test\"}\n";
    fs::write(&dest, initial_bytes).expect("seed file");
    let initial_hash = sha256_hex(initial_bytes);

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::EditSkill, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::TempCreate, Fault::Fail(libc::EACCES));

    let new_bytes = b"{\"name\": \"updated\"}\n".to_vec();
    let req = WriteRequest::replace(&dest, new_bytes, ExpectedState::Hash(initial_hash));

    let result = with_injector(injector, || apply(&req));

    assert!(
        matches!(result, Err(FsError::PermissionDenied { .. })),
        "expected PermissionDenied, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("PermissionDenied"),
    )
    .expect("finish op failed");

    let on_disk = fs::read(&dest).expect("read dest");
    assert_eq!(
        on_disk, initial_bytes,
        "original bytes must remain unchanged"
    );
    assert_eq!(count_tmp_files(dir.path()), 0);

    let op = latest_op(&db);
    assert_eq!(op.status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Scenario 3: ENOSPC during write and during fsync
// ---------------------------------------------------------------------------
#[test]
fn scenario_3a_enospc_during_temp_write_returns_no_space_left() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("rule.md");
    let initial_bytes = b"# Rules\nRule 1\n";
    fs::write(&dest, initial_bytes).expect("seed file");
    let initial_hash = sha256_hex(initial_bytes);

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::EditRule, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::TempWrite, Fault::Fail(libc::ENOSPC));

    let new_bytes = b"# Rules\nRule 1\nRule 2\n".to_vec();
    let req = WriteRequest::replace(&dest, new_bytes, ExpectedState::Hash(initial_hash));

    let result = with_injector(injector, || apply(&req));

    assert!(
        matches!(result, Err(FsError::NoSpaceLeft { .. })),
        "expected NoSpaceLeft, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("NoSpaceLeft"),
    )
    .expect("finish op failed");

    assert_eq!(fs::read(&dest).expect("read dest"), initial_bytes);
    assert_eq!(count_tmp_files(dir.path()), 0);
    assert_eq!(latest_op(&db).status, Some(OperationStatus::Failed));
}

#[test]
fn scenario_3b_enospc_during_temp_fsync_returns_no_space_left() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("rule.md");
    let initial_bytes = b"# Rules\n";
    fs::write(&dest, initial_bytes).expect("seed file");
    let initial_hash = sha256_hex(initial_bytes);

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::EditRule, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::TempSync, Fault::Fail(libc::ENOSPC));

    let new_bytes = b"# Rules\nModified\n".to_vec();
    let req = WriteRequest::replace(&dest, new_bytes, ExpectedState::Hash(initial_hash));

    let result = with_injector(injector, || apply(&req));

    assert!(
        matches!(result, Err(FsError::NoSpaceLeft { .. })),
        "expected NoSpaceLeft, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("NoSpaceLeft"),
    )
    .expect("finish op failed");

    assert_eq!(fs::read(&dest).expect("read dest"), initial_bytes);
    assert_eq!(count_tmp_files(dir.path()), 0);
    assert_eq!(latest_op(&db).status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Scenario 4: EPERM on symlink creation
// ---------------------------------------------------------------------------
#[test]
fn scenario_4_eperm_on_symlink_creation_returns_link_unsupported() {
    let dir = TempDir::new().expect("tempdir");
    let source = dir.path().join("source_skill");
    fs::create_dir_all(&source).expect("create source dir");
    fs::write(source.join("SKILL.md"), b"# Test Skill\n").expect("write skill");

    let target = dir.path().join("deployed_link");

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::DeploySymlink, Fault::Fail(libc::EPERM));

    let req = DeployRequest::new(source, target.clone(), LinkMode::Link);

    let result = with_injector(injector, || deploy(&req));

    assert!(
        matches!(result, Err(FsError::LinkUnsupported { .. })),
        "expected LinkUnsupported, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("LinkUnsupported"),
    )
    .expect("finish op failed");

    assert!(!target.exists(), "target should not have been created");
    assert_eq!(count_tmp_files(dir.path()), 0);
    assert_eq!(latest_op(&db).status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Scenario 5: EXDEV on rename for a link
// ---------------------------------------------------------------------------
#[test]
fn scenario_5_exdev_on_rename_returns_cross_device_rename() {
    let dir = TempDir::new().expect("tempdir");
    let source = dir.path().join("source_skill");
    fs::create_dir_all(&source).expect("create source dir");
    fs::write(source.join("SKILL.md"), b"# Test Skill\n").expect("write skill");

    let target = dir.path().join("deployed_link");

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::DeployRename, Fault::Fail(libc::EXDEV));

    let req = DeployRequest::new(source, target.clone(), LinkMode::Link);

    let result = with_injector(injector, || deploy(&req));

    assert!(
        matches!(result, Err(FsError::CrossDeviceRename { .. })),
        "expected CrossDeviceRename, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("CrossDeviceRename"),
    )
    .expect("finish op failed");

    assert!(!target.exists(), "target should not exist");
    assert_eq!(count_tmp_files(dir.path()), 0);
    assert_eq!(latest_op(&db).status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Scenario 6: SIGKILL between backup and rename
// ---------------------------------------------------------------------------
#[test]
fn crash_child_for_scenario_6() {
    let Ok(env_path) = std::env::var("AT_FS_SCENARIO_6_DIR") else {
        return;
    };
    let dir = PathBuf::from(env_path);
    let target = dir.join("target.json");
    let ops = dir.join("backups");
    let mut req = WriteRequest::replace(
        &target,
        b"{\"v\": 2}\n".to_vec(),
        ExpectedState::Hash(sha256_hex(b"{\"v\": 1}\n")),
    );
    req.backup = BackupPolicy::OperationFolder(ops);

    let _ = apply_with_hook(&req, &mut |stage| {
        if stage == Stage::BeforeRename {
            std::process::abort();
        }
    });
}

#[test]
fn scenario_6_sigkill_between_backup_and_rename_leaves_interrupted_and_recovers_idempotently() {
    let dir = TempDir::new().expect("tempdir");
    let target = dir.path().join("target.json");
    let initial_bytes = b"{\"v\": 1}\n";
    let initial_hash = sha256_hex(initial_bytes);
    fs::write(&target, initial_bytes).expect("seed target");

    let backups_dir = dir.path().join("backups");
    fs::create_dir_all(&backups_dir).expect("create backups dir");

    let db_path = dir.path().join("history.sqlite");
    let db = Database::open_file(&db_path).expect("open file db");

    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
    )
    .expect("begin op");

    // Spawn child that aborts at Stage::BeforeRename
    let status = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", "crash_child_for_scenario_6", "--nocapture"])
        .env("AT_FS_SCENARIO_6_DIR", dir.path())
        .status()
        .expect("child process starts");
    assert!(
        !status.success(),
        "child process must abort via SIGKILL/abort"
    );

    // Target is intact and holds original bytes
    let on_disk = fs::read(&target).expect("read target");
    assert_eq!(on_disk, initial_bytes, "original bytes must remain intact");

    // Backup exists
    let backup_blob = backups_dir.join("target.json");
    assert!(backup_blob.exists(), "backup blob must exist");
    assert_eq!(fs::read(&backup_blob).expect("read backup"), initial_bytes);

    // Record the interrupted operation in at-db
    record_interrupted(
        db.connection(),
        op_id.as_str(),
        Some(OperationKind::Deploy),
        Some("process terminated unexpectedly before rename"),
        time::OffsetDateTime::now_utc().unix_timestamp() * 1000,
    )
    .expect("record interrupted operation");

    // Run recovery logic (TF-027 simulation)
    let recovery_ops_folder = dir.path().join("recovery_ops");
    let run_recovery = || -> Result<(), String> {
        let now = time::OffsetDateTime::now_utc();
        let _ = sweep_stale_temp_files(&[dir.path().to_path_buf()], now, time::Duration::ZERO);
        let req = RestoreRequest {
            path: target.clone(),
            before_bytes: fs::read(&backup_blob).map_err(|e| e.to_string())?,
            before_hash: initial_hash.clone(),
            expected_current_hash: initial_hash.clone(),
            operation_folder: recovery_ops_folder.clone(),
            backup_meta: BackupMeta::default(),
        };
        restore_file(&req).map_err(|e| format!("{e:?}"))?;
        Ok(())
    };

    // First recovery run
    run_recovery().expect("recovery run 1 succeeds");
    assert_eq!(fs::read(&target).expect("read target"), initial_bytes);
    assert_eq!(
        count_tmp_files(dir.path()),
        0,
        "temp files must be swept during recovery"
    );

    // Second recovery run passes identically (idempotency: "the same test run passes twice in a row")
    run_recovery().expect("recovery run 2 succeeds idempotently");
    assert_eq!(fs::read(&target).expect("read target"), initial_bytes);
    assert_eq!(count_tmp_files(dir.path()), 0);

    let op = latest_op(&db);
    assert_eq!(
        op.status,
        Some(OperationStatus::Interrupted),
        "operation row must be interrupted"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7: Disk full while writing a backup
// ---------------------------------------------------------------------------
#[test]
fn scenario_7_disk_full_during_backup_aborts_before_dest_open_and_returns_backup_failed() {
    let dir = TempDir::new().expect("tempdir");
    let dest = dir.path().join("server.json");
    let initial_bytes = b"{\"server\": \"alpha\"}\n";
    fs::write(&dest, initial_bytes).expect("seed dest");
    let initial_hash = sha256_hex(initial_bytes);

    let backups_dir = dir.path().join("ops_backup");
    fs::create_dir_all(&backups_dir).expect("backups dir");

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::EditMcp, OperationActor::Cli),
    )
    .expect("begin op");

    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::BackupWrite, Fault::Fail(libc::ENOSPC));

    let mut req = WriteRequest::replace(
        &dest,
        b"{\"server\": \"beta\"}\n".to_vec(),
        ExpectedState::Hash(initial_hash),
    );
    req.backup = BackupPolicy::OperationFolder(backups_dir.clone());

    let result = with_injector(injector, || apply(&req));

    assert!(
        result.as_ref().is_err_and(FsError::is_backup_failed),
        "expected BackupFailed, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("BackupFailed"),
    )
    .expect("finish op failed");

    // Original destination file was never opened or modified
    assert_eq!(fs::read(&dest).expect("read dest"), initial_bytes);

    // No partial backup files left in operation folder
    let backup_entries: Vec<_> = fs::read_dir(&backups_dir)
        .expect("read backups dir")
        .filter_map(Result::ok)
        .collect();
    assert!(
        backup_entries.is_empty(),
        "backup files must be discarded on failure, found: {backup_entries:?}"
    );

    // Zero temp files in dest dir
    assert_eq!(count_tmp_files(dir.path()), 0);

    let op = latest_op(&db);
    assert_eq!(op.status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Scenario 8: Read-only destination volume
// ---------------------------------------------------------------------------
#[test]
fn scenario_8_read_only_destination_directory_returns_permission_denied() {
    let dir = TempDir::new().expect("tempdir");
    let read_only_dir = dir.path().join("readonly_vol");
    fs::create_dir(&read_only_dir).expect("create dir");

    let dest = read_only_dir.join("file.txt");
    let initial_bytes = b"immutable\n";
    fs::write(&dest, initial_bytes).expect("write file");
    let initial_hash = sha256_hex(initial_bytes);

    // Set read-only permissions on the directory
    fs::set_permissions(&read_only_dir, fs::Permissions::from_mode(0o555)).expect("chmod 0555");

    let db = open_test_db();
    let op_id = begin(
        db.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
    )
    .expect("begin op");

    let req = WriteRequest::replace(
        &dest,
        b"new content\n".to_vec(),
        ExpectedState::Hash(initial_hash),
    );

    let result = apply(&req);

    // Restore permissions so cleanup succeeds
    let _ = fs::set_permissions(&read_only_dir, fs::Permissions::from_mode(0o755));

    assert!(
        matches!(result, Err(FsError::PermissionDenied { .. })),
        "expected PermissionDenied, got {result:?}"
    );

    finish(
        db.connection(),
        &op_id,
        OperationStatus::Failed,
        Some("PermissionDenied"),
    )
    .expect("finish op failed");

    assert_eq!(fs::read(&dest).expect("read dest"), initial_bytes);
    assert_eq!(count_tmp_files(&read_only_dir), 0);
    assert_eq!(latest_op(&db).status, Some(OperationStatus::Failed));
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 5: Release profile check
// ---------------------------------------------------------------------------
#[test]
fn failpoints_are_absent_from_release_profile_dependencies() {
    // Cargo tree check: no `fail` or `fail-rs` dependency in release build
    let output = Command::new("cargo")
        .args(["tree", "-p", "at-fs", "--prefix", "none", "--no-dedupe"])
        .output()
        .expect("cargo tree runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let pkg = line.split_whitespace().next().unwrap_or_default();
        assert_ne!(pkg, "fail", "no external fail crate allowed");
        assert_ne!(pkg, "fail-rs", "no external fail-rs crate allowed");
    }
}
