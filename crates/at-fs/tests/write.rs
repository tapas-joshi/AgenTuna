//! Behaviour of the guarded write pipeline against real temp directories.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use at_fs::atomic::{Stage, apply_with_hook};
use at_fs::backup::{self, BackupError};
use at_fs::{
    BackupPolicy, ExpectedState, FsError, MAX_FILE_BYTES, WriteMode, WriteRequest, apply,
    sha256_hex,
};
use tempfile::TempDir;

fn write_file(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn replace_req(path: &Path, old: &str, new: &str) -> WriteRequest {
    WriteRequest::replace(
        path,
        new.as_bytes().to_vec(),
        ExpectedState::Hash(sha256_hex(old.as_bytes())),
    )
}

#[test]
fn replaces_file_with_diff_backup_and_hashes() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    let file = dir.path().join("config.json");
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
    write_file(&file, old);

    let mut req = replace_req(&file, old, new);
    req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());
    let out = apply(&req).unwrap();

    assert_eq!(fs::read_to_string(&file).unwrap(), new);
    assert_eq!(
        out.before_hash.as_deref(),
        Some(sha256_hex(old.as_bytes()).as_str())
    );
    assert_eq!(out.after_hash, sha256_hex(new.as_bytes()));
    assert_eq!(out.bytes, new.len() as u64);
    assert!(!out.wrote_through_symlink());
    // 3 lines of context each side of the change: a,b,c .. f,g,h, but not 'a' beyond it.
    assert!(out.diff.contains("-e\n+E\n"), "{}", out.diff);
    assert!(out.diff.contains(" b\n c\n d\n"), "{}", out.diff);
    assert!(out.diff.contains(" f\n g\n h\n"), "{}", out.diff);
    assert!(!out.diff.contains(" a\n"), "{}", out.diff);
    assert!(!out.diff.contains(" i\n"), "{}", out.diff);

    let blob = out.backup_path.unwrap();
    assert_eq!(blob.parent().unwrap(), ops.path());
    assert_eq!(fs::read_to_string(&blob).unwrap(), old);
    let sidecar = backup::verify(&blob, out.before_hash.as_deref().unwrap()).unwrap();
    assert_eq!(sidecar.original_path, file);
    assert_eq!(
        names(dir.path()),
        ["config.json"],
        "no temp file left behind"
    );
}

#[test]
fn hash_mismatch_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "changed externally");
    let mut req = replace_req(&file, "what the caller saw", "new");
    req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());

    let err = apply(&req).unwrap_err();
    match err {
        FsError::ConcurrentModification {
            expected, actual, ..
        } => {
            assert_eq!(expected, sha256_hex(b"what the caller saw"));
            assert_eq!(actual, sha256_hex(b"changed externally"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), "changed externally");
    assert!(
        names(ops.path()).is_empty(),
        "no backup for a refused write"
    );
    assert_eq!(names(dir.path()), ["f"]);
}

#[test]
fn external_write_before_rename_is_caught_and_rolled_back() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "v1");
    let mut req = replace_req(&file, "v1", "mine");
    req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());

    let err = apply_with_hook(&req, &mut |stage| {
        if stage == Stage::BeforeRename {
            write_file(&file, "theirs");
        }
    })
    .unwrap_err();

    assert!(
        matches!(err, FsError::ConcurrentModification { .. }),
        "{err:?}"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "theirs",
        "external edit survives"
    );
    assert_eq!(names(dir.path()), ["f"], "temp removed");
    assert!(
        names(ops.path()).is_empty(),
        "backup of a write that never happened is removed"
    );
}

#[test]
fn unwritable_directory_is_permission_denied_without_leftovers() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "old");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let mut req = replace_req(&file, "old", "new");
    req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());

    let result = apply(&req);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        matches!(result, Err(FsError::PermissionDenied { .. })),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(names(dir.path()), ["f"]);
    assert!(names(ops.path()).is_empty(), "no backup orphan");
}

/// Child half of the crash test: aborts (no destructors) right before the rename.
#[test]
fn crash_child() {
    let Ok(dir) = std::env::var("AT_FS_CRASH_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let mut req = replace_req(&dir.join("target"), "original", "replacement");
    req.backup = BackupPolicy::OperationFolder(dir.join("ops"));
    let _ = apply_with_hook(&req, &mut |stage| {
        if stage == Stage::BeforeRename {
            std::process::abort();
        }
    });
}

#[test]
fn kill_before_rename_leaves_original_and_recoverable_artifacts() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("ops")).unwrap();
    write_file(&root.path().join("target"), "original");

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture"])
        .env("AT_FS_CRASH_DIR", root.path())
        .status()
        .unwrap();
    assert!(!status.success(), "child must have aborted");

    assert_eq!(
        fs::read_to_string(root.path().join("target")).unwrap(),
        "original"
    );
    let leftovers = names(root.path());
    assert!(
        leftovers
            .iter()
            .any(|n| n.starts_with(".target.agentuna-tmp-")),
        "temp is discoverable for recovery: {leftovers:?}"
    );
    let ops = names(&root.path().join("ops"));
    assert_eq!(
        ops,
        ["target", "target.json"],
        "backup blob and sidecar exist"
    );
    backup::verify(&root.path().join("ops/target"), &sha256_hex(b"original")).unwrap();
}

#[test]
fn symlink_is_refused_by_default_and_nothing_changes() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("real");
    let link = dir.path().join("link");
    write_file(&target, "old");
    symlink(&target, &link).unwrap();

    let err = apply(&replace_req(&link, "old", "new")).unwrap_err();

    assert!(matches!(err, FsError::SymlinkRefused { .. }), "{err:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn following_a_symlink_writes_the_target_and_keeps_the_link() {
    let dir = TempDir::new().unwrap();
    let target = fs::canonicalize(dir.path()).unwrap().join("real");
    let link = dir.path().join("link");
    write_file(&target, "old");
    symlink(&target, &link).unwrap();
    let mut req = replace_req(&link, "old", "new");
    req.follow_symlink = true;

    let out = apply(&req).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(out.wrote_through_symlink());
    let sym = out.symlink.unwrap();
    assert_eq!(sym.link_path, link);
    assert_eq!(sym.resolved_path, target);
}

#[test]
fn dangling_symlink_is_reported_even_when_following() {
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("link");
    symlink(dir.path().join("missing"), &link).unwrap();
    let mut req = WriteRequest::create(&link, b"x".to_vec());
    req.follow_symlink = true;
    assert!(matches!(apply(&req), Err(FsError::DanglingSymlink { .. })));
}

#[test]
fn create_is_exclusive_and_new_files_are_private() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("new");

    let out = apply(&WriteRequest::create(&file, b"hello\n".to_vec())).unwrap();
    assert_eq!(out.before_hash, None);
    assert!(
        out.diff.contains("--- /dev/null") && out.diff.contains("+hello"),
        "{}",
        out.diff
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(names(dir.path()), ["new"]);

    let again = apply(&WriteRequest::create(&file, b"other".to_vec()));
    assert!(
        matches!(again, Err(FsError::AlreadyExists { .. })),
        "{again:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello\n");
}

#[test]
fn expecting_absent_but_file_appeared_is_a_conflict() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "surprise");
    let mut req = WriteRequest::replace(&file, b"x".to_vec(), ExpectedState::Absent);
    req.mode = WriteMode::Replace;
    assert!(matches!(
        apply(&req),
        Err(FsError::ConcurrentModification { .. })
    ));
    assert_eq!(fs::read_to_string(&file).unwrap(), "surprise");
}

#[test]
fn replace_preserves_existing_permissions() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "old");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    apply(&replace_req(&file, "old", "new")).unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn mtime_expectation_rejects_a_touched_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "same");
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    let hash = sha256_hex(b"same");

    let stale = mtime - std::time::Duration::from_secs(5);
    let req = WriteRequest::replace(
        &file,
        b"new".to_vec(),
        ExpectedState::HashAndMtime {
            hash: hash.clone(),
            mtime: stale,
        },
    );
    assert!(matches!(
        apply(&req),
        Err(FsError::ConcurrentModification { .. })
    ));

    let req = WriteRequest::replace(
        &file,
        b"new".to_vec(),
        ExpectedState::HashAndMtime { hash, mtime },
    );
    apply(&req).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
}

#[test]
fn directories_are_refused_unless_empty_and_allowed() {
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("d");
    fs::create_dir(&dest).unwrap();

    let err = apply(&WriteRequest::create(&dest, b"x".to_vec()));
    assert!(
        matches!(err, Err(FsError::DestinationIsDirectory { .. })),
        "{err:?}"
    );

    let mut req = WriteRequest::create(&dest, b"x".to_vec());
    req.allow_empty_dir = true;
    apply(&req).unwrap();
    assert_eq!(fs::read_to_string(&dest).unwrap(), "x");

    let full = dir.path().join("full");
    fs::create_dir(&full).unwrap();
    write_file(&full.join("inner"), "keep");
    let mut req = WriteRequest::create(&full, b"x".to_vec());
    req.allow_empty_dir = true;
    assert!(matches!(
        apply(&req),
        Err(FsError::DestinationIsDirectory { .. })
    ));
    assert_eq!(fs::read_to_string(full.join("inner")).unwrap(), "keep");
}

#[test]
fn oversize_content_and_missing_parent_are_rejected() {
    let dir = TempDir::new().unwrap();
    let big = vec![0u8; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
    let err = apply(&WriteRequest::create(dir.path().join("big"), big));
    assert!(matches!(err, Err(FsError::TooLarge { .. })));

    let err = apply(&WriteRequest::create(
        dir.path().join("no/such/dir/f"),
        b"x".to_vec(),
    ));
    assert!(matches!(err, Err(FsError::NotFound { .. })), "{err:?}");
}

#[test]
fn backup_names_do_not_collide_inside_one_operation_folder() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    for (sub, content) in [("a", "one"), ("b", "two")] {
        fs::create_dir(dir.path().join(sub)).unwrap();
        let file = dir.path().join(sub).join("settings.json");
        write_file(&file, content);
        let mut req = replace_req(&file, content, "new");
        req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());
        apply(&req).unwrap();
    }
    assert_eq!(
        names(ops.path()),
        [
            "settings.json",
            "settings.json.1",
            "settings.json.1.json",
            "settings.json.json"
        ]
    );
}

#[test]
fn verify_detects_tampering_and_missing_sidecar() {
    let dir = TempDir::new().unwrap();
    let ops = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "old");
    let mut req = replace_req(&file, "old", "new");
    req.backup = BackupPolicy::OperationFolder(ops.path().to_path_buf());
    let blob = apply(&req).unwrap().backup_path.unwrap();
    let hash = sha256_hex(b"old");

    assert!(backup::verify(&blob, &hash).is_ok());
    assert!(matches!(
        backup::verify(&blob, &sha256_hex(b"other")),
        Err(BackupError::Corrupt { .. })
    ));

    write_file(&blob, "tampered");
    assert!(matches!(
        backup::verify(&blob, &hash),
        Err(BackupError::Corrupt { .. })
    ));

    fs::remove_file(blob.with_file_name("f.json")).unwrap();
    assert!(matches!(
        backup::verify(&blob, &hash),
        Err(BackupError::Corrupt { .. })
    ));
}

#[test]
fn missing_backup_folder_fails_before_touching_the_destination() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("f");
    write_file(&file, "old");
    let mut req = replace_req(&file, "old", "new");
    req.backup = BackupPolicy::OperationFolder(dir.path().join("nope"));

    let err = apply(&req).unwrap_err();

    assert!(
        matches!(err, FsError::Backup(BackupError::FolderMissing(_))),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    assert_eq!(names(dir.path()), ["f"], "temp cleaned up");
}
