//! TF-041: the backup copy primitive itself — temp+rename install and idempotent re-runs.
//!
//! AC2 requires the blob to be created through a staged temp file that is fsynced before
//! the rename, and a repeated copy of identical bytes to leave exactly one blob plus one
//! sidecar. The fsync-failure case is the observable proof of staging: a failure between
//! write and rename must leave nothing at the final name, whereas a direct O_EXCL create
//! would have exposed a truncated blob.

use std::fs;
use std::sync::Arc;

use at_fs::backup::{self, BackupError, BackupMeta};
use at_fs::failpoint::{Fault, FaultPoint, TestInjector, with_injector};
use at_fs::recovery::is_temp_file;
use at_fs::sha256_hex;
use tempfile::TempDir;
use time::OffsetDateTime;

/// Entry names in `dir`, sorted.
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .expect("read operation folder")
        .map(|entry| {
            entry
                .expect("read dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

fn original(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).expect("write original");
    path
}

#[test]
fn repeating_an_identical_copy_is_idempotent() {
    let work = TempDir::new().expect("temp dir");
    let ops = TempDir::new().expect("operation folder");
    let source = original(&work, "settings.json", b"{\"a\":1}");
    let meta = BackupMeta::default();
    let now = OffsetDateTime::now_utc();

    let first = backup::copy_into(ops.path(), &source, b"{\"a\":1}", &meta, now)
        .expect("first copy succeeds");
    let second = backup::copy_into(ops.path(), &source, b"{\"a\":1}", &meta, now)
        .expect("repeated copy succeeds");

    assert_eq!(
        first, second,
        "re-running an identical copy reuses its files"
    );
    assert_eq!(
        names(ops.path()),
        ["settings.json", "settings.json.json"],
        "one blob plus one sidecar, no numeric suffix"
    );
    let sidecar = backup::verify(&first.blob, &sha256_hex(b"{\"a\":1}"))
        .expect("blob and sidecar verify against the copied bytes");
    assert_eq!(sidecar.content_hash, sha256_hex(b"{\"a\":1}"));
    assert_eq!(sidecar.original_path, source);
}

#[test]
fn a_fsync_failure_never_exposes_a_blob_at_the_final_name() {
    let work = TempDir::new().expect("temp dir");
    let ops = TempDir::new().expect("operation folder");
    let source = original(&work, "settings.json", b"old");
    let injector = Arc::new(TestInjector::new());
    injector.arm(FaultPoint::BackupSync, Fault::Fail(libc::ENOSPC));

    let result = with_injector(injector as Arc<dyn at_fs::failpoint::FailInjector>, || {
        backup::copy_into(
            ops.path(),
            &source,
            b"old",
            &BackupMeta::default(),
            OffsetDateTime::now_utc(),
        )
    });

    let error = result.expect_err("a failed fsync aborts the backup");
    assert!(
        matches!(error, BackupError::BackupFailed { .. }),
        "disk full must surface as BackupFailed, got {error:?}"
    );
    let left = names(ops.path());
    assert!(
        left.is_empty(),
        "a failed staged write leaves no blob, sidecar or temp behind, found {left:?}"
    );
    assert!(
        !ops.path().join("settings.json").exists(),
        "the final blob name must never exist before its bytes are durable"
    );
}

#[test]
fn a_successful_copy_leaves_no_staging_file() {
    let work = TempDir::new().expect("temp dir");
    let ops = TempDir::new().expect("operation folder");
    let source = original(&work, "f", b"bytes");

    backup::copy_into(
        ops.path(),
        &source,
        b"bytes",
        &BackupMeta::default(),
        OffsetDateTime::now_utc(),
    )
    .expect("copy succeeds");

    let left = names(ops.path());
    assert!(
        left.iter().all(|name| !is_temp_file(name)),
        "the staging temp file is renamed away on success, found {left:?}"
    );
}

#[test]
fn a_different_copy_with_the_same_file_name_is_preserved_alongside() {
    let work = TempDir::new().expect("temp dir");
    let ops = TempDir::new().expect("operation folder");
    let first_source = work.path().join("a");
    let second_source = work.path().join("b");
    fs::create_dir(&first_source).expect("first subdir");
    fs::create_dir(&second_source).expect("second subdir");
    let first_file = first_source.join("settings.json");
    fs::write(&first_file, b"one").expect("write first file");
    let second_file = second_source.join("settings.json");
    fs::write(&second_file, b"two").expect("write second file");

    let now = OffsetDateTime::now_utc();
    let first = backup::copy_into(ops.path(), &first_file, b"one", &BackupMeta::default(), now)
        .expect("first copy succeeds");
    let second = backup::copy_into(
        ops.path(),
        &second_file,
        b"two",
        &BackupMeta::default(),
        now,
    )
    .expect("second copy succeeds");

    assert_ne!(first.blob, second.blob, "both backups are kept");
    assert_eq!(
        names(ops.path()),
        [
            "settings.json",
            "settings.json.1",
            "settings.json.1.json",
            "settings.json.json"
        ],
        "the different-content collision keeps both blobs and both sidecars"
    );
    backup::verify(&first.blob, &sha256_hex(b"one")).expect("first backup verifies");
    backup::verify(&second.blob, &sha256_hex(b"two")).expect("second backup verifies");
}
