//! TF-053: journaled multi-file ChangeSet apply — operation history and crash recovery.
//!
//! The success test uses a real SQLite history database and asserts one operation row with one
//! `operation_files` row and one catalogued backup per file. The crash test runs the applying
//! process in a child, aborts it before the third operation and checks that the next-start
//! recovery restores the pre-apply state from the journal.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use at_db::Database;
use at_fs::failpoint::{FailInjector, Fault, FaultPoint, with_injector};
use at_fs::recovery::{
    JournalEntry, PendingJournal, RecoveryCallbacks, RecoveryPolicy, recover_journals,
};
use at_fs::{
    ApplyContext, ApplyRoots, DbOperationRecorder, OperationRecorder, apply_with_roots, sha256_hex,
};
use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};
use tempfile::TempDir;

const BEFORE: [&str; 5] = ["before-0", "before-1", "before-2", "before-3", "before-4"];
const AFTER: [&str; 5] = ["after-0", "after-1", "after-2", "after-3", "after-4"];

fn seed_files(root: &Path) {
    fs::create_dir_all(root.join("files")).expect("files directory");
    for (index, before) in BEFORE.iter().enumerate() {
        fs::write(root.join(format!("files/f{index}.txt")), before).expect("seed file");
    }
}

fn five_write_set(root: &Path) -> ChangeSet {
    let ops = (0..5)
        .map(|index| FileOp::Write {
            path: root.join(format!("files/f{index}.txt")),
            bytes: AFTER[index].as_bytes().to_vec(),
            expected: ExpectedState::hash(sha256_hex(BEFORE[index].as_bytes()))
                .expect("valid hash"),
            follow_symlink: false,
            backup: true,
        })
        .collect();
    ChangeSet {
        id: String::from("set-tf053"),
        preview_id: String::from("preview-tf053"),
        operation_kind: OperationKind::parse("edit_skill").expect("known operation kind"),
        scope: Some(Scope::User),
        project_id: None,
        summary: String::from("TF-053 five-file apply"),
        ops,
        created_at_unix_seconds: 1_700_000_000,
    }
}

fn backup_root(root: &Path) -> PathBuf {
    root.join("backups")
}

#[test]
fn five_file_changeset_records_one_operation_five_file_rows_and_a_backup_each() {
    let sandbox = TempDir::new().expect("sandbox");
    let root = sandbox.path().canonicalize().expect("canonical root");
    seed_files(&root);
    let backups = backup_root(&root);
    fs::create_dir_all(&backups).expect("backup root");
    let database = Database::open_file(root.join("history.sqlite")).expect("history database");
    let set = five_write_set(&root);
    let roots = ApplyRoots::new([root.clone()]).expect("authorized roots");

    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: &root,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let outcome = apply_with_roots(&set, &mut context, &roots).expect("apply all five files");
    assert_eq!(outcome.completed, 5);

    let operation_count: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
        .expect("count operations");
    assert_eq!(operation_count, 1, "five file ops share one operation");
    let status: String = database
        .connection()
        .query_row(
            "SELECT status FROM operations WHERE id = ?1",
            [&outcome.operation_id],
            |row| row.get(0),
        )
        .expect("operation status");
    assert_eq!(status, "succeeded");
    let file_rows: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM operation_files WHERE operation_id = ?1",
            [&outcome.operation_id],
            |row| row.get(0),
        )
        .expect("count operation files");
    assert_eq!(file_rows, 5, "one operation_files row per file op");

    for index in 0..5 {
        let path = root.join(format!("files/f{index}.txt"));
        let disk = fs::read(&path).expect("written bytes");
        assert_eq!(disk, AFTER[index].as_bytes());
        let path_text = path.to_string_lossy();
        let (before, after, action, backup_id): (
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ) = database
            .connection()
            .query_row(
                "SELECT before_hash, after_hash, action, backup_id \
                   FROM operation_files WHERE operation_id = ?1 AND path = ?2",
                [outcome.operation_id.as_str(), path_text.as_ref()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("operation file row");
        let before_hash = sha256_hex(BEFORE[index].as_bytes());
        let after_hash = sha256_hex(&disk);
        assert_eq!(before.as_deref(), Some(before_hash.as_str()));
        assert_eq!(after.as_deref(), Some(after_hash.as_str()));
        assert_eq!(action, "modified");

        let backup_id = backup_id.expect("every changed file has a backup");
        let (backup_path, content_hash): (String, String) = database
            .connection()
            .query_row(
                "SELECT backup_path, content_hash FROM backups WHERE id = ?1",
                [&backup_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("backup catalogue row");
        assert_eq!(
            content_hash,
            sha256_hex(BEFORE[index].as_bytes()),
            "the catalogue stores the pre-write hash"
        );
        assert_eq!(
            fs::read(&backup_path).expect("backup blob"),
            BEFORE[index].as_bytes(),
            "the backup holds the original bytes"
        );

        // TC-014 layout: backups/<utc-ts>-<operation_id>/<scope>/<relpath> plus a sidecar.
        let backup_file = PathBuf::from(&backup_path);
        let relative = backup_file
            .strip_prefix(&backups)
            .expect("backup lives under the backup root");
        let mut parts = relative.components();
        let folder = parts
            .next()
            .and_then(|part| part.as_os_str().to_str())
            .expect("operation folder");
        let suffix = format!("-{}", outcome.operation_id);
        let timestamp = folder
            .strip_suffix(&suffix)
            .expect("the folder is timestamped and names the operation");
        assert_eq!(timestamp.len(), 16, "UTC timestamp: {timestamp}");
        assert!(
            timestamp.ends_with('Z') && timestamp.as_bytes()[8] == b'T',
            "UTC timestamp shape: {timestamp}"
        );
        assert_eq!(
            parts.next().and_then(|part| part.as_os_str().to_str()),
            Some(Scope::User.to_string().as_str()),
            "the scope is a path segment"
        );
        assert_eq!(
            parts.next().and_then(|part| part.as_os_str().to_str()),
            Some("files")
        );
        assert_eq!(
            parts.next().and_then(|part| part.as_os_str().to_str()),
            Some(format!("f{index}.txt").as_str())
        );
        let expected_hash = sha256_hex(BEFORE[index].as_bytes());
        let sidecar = at_fs::backup::verify(&backup_file, &expected_hash)
            .expect("the blob and sidecar verify against the recorded hash");
        assert_eq!(sidecar.original_path, path);
        assert_eq!(
            sidecar.operation_id.as_deref(),
            Some(outcome.operation_id.as_str())
        );
        assert_eq!(sidecar.content_hash, expected_hash);
    }
}

struct NullRecorder;

impl OperationRecorder for NullRecorder {
    fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
        Ok(String::from("tf053-crash-operation"))
    }

    fn file_completed(
        &mut self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&Path>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

struct AbortBeforeThirdOperation(AtomicUsize);

impl FailInjector for AbortBeforeThirdOperation {
    fn fault(&self, point: FaultPoint, _: &Path) -> Option<Fault> {
        if point == FaultPoint::ChangeSetOperation && self.0.fetch_add(1, Ordering::SeqCst) == 2 {
            std::process::abort();
        }
        None
    }
}

#[test]
fn crash_child_for_tf053() {
    let Ok(root) = std::env::var("AT_FS_TF053_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let backups = backup_root(&root);
    fs::create_dir_all(&backups).expect("backup root");
    let set = five_write_set(&root);
    let roots = ApplyRoots::new([root.clone()]).expect("authorized roots");
    let mut recorder = NullRecorder;
    let mut context = ApplyContext {
        store_root: &root,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let injector = Arc::new(AbortBeforeThirdOperation(AtomicUsize::new(0)));
    let _ = with_injector(injector, || apply_with_roots(&set, &mut context, &roots));
}

struct RecoveryBeforeBytes;

impl RecoveryCallbacks for RecoveryBeforeBytes {
    fn before_bytes(
        &mut self,
        journal: &PendingJournal,
        _: usize,
        entry: &JournalEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(backup_root) = journal.backup_root.as_deref() else {
            return Ok(None);
        };
        if journal
            .change_set
            .as_ref()
            .and_then(|set| set.scope)
            .is_none()
        {
            return Ok(None);
        }
        let name = entry
            .path
            .file_name()
            .ok_or_else(|| String::from("journal entry has no file name"))?;
        Ok(find_backup(backup_root, name, entry.before_hash.as_deref()))
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Depth-first lookup of the saved original by file name and pre-write hash.
fn find_backup(root: &Path, name: &OsStr, before_hash: Option<&str>) -> Option<Vec<u8>> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.file_name() != Some(name)
                || path.extension().and_then(OsStr::to_str) == Some("json")
            {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if before_hash.is_none_or(|hash| sha256_hex(&bytes) == hash) {
                return Some(bytes);
            }
        }
    }
    None
}

#[test]
fn abort_during_the_third_operation_leaves_the_journal_and_startup_recovery_restores() {
    let sandbox = TempDir::new().expect("sandbox");
    let root = sandbox.path().canonicalize().expect("canonical root");
    seed_files(&root);
    let backups = backup_root(&root);
    fs::create_dir_all(&backups).expect("backup root");

    let status = Command::new(std::env::current_exe().expect("current test binary"))
        .args(["--exact", "crash_child_for_tf053", "--nocapture"])
        .env("AT_FS_TF053_ROOT", &root)
        .status()
        .expect("child process starts");
    assert!(!status.success(), "the child aborts while op 3 runs");

    let journal_dir = root.join(".state/journal");
    let journals: Vec<PathBuf> = fs::read_dir(&journal_dir)
        .expect("journal directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("journal entries")
        .into_iter()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(journals.len(), 1, "the crash leaves exactly one journal");

    for index in 0..5 {
        let disk = fs::read(root.join(format!("files/f{index}.txt"))).expect("file bytes");
        let expected: &[u8] = if index < 2 {
            AFTER[index].as_bytes()
        } else {
            BEFORE[index].as_bytes()
        };
        assert_eq!(disk, expected, "operation {index} state before recovery");
    }

    let policy = RecoveryPolicy::new(root.clone(), [root.clone()], [backups.clone()]);
    let mut callbacks = RecoveryBeforeBytes;
    let report = recover_journals(&journal_dir, &policy, &mut callbacks).expect("recovery pass");
    assert!(report.manual_review.is_empty(), "{report:?}");
    assert_eq!(report.rolled_back, journals);

    for (index, before) in BEFORE.iter().enumerate() {
        assert_eq!(
            fs::read(root.join(format!("files/f{index}.txt"))).expect("restored file"),
            before.as_bytes(),
            "operation {index} is restored to its pre-apply bytes"
        );
    }
    assert_eq!(
        fs::read_dir(&journal_dir)
            .expect("journal directory")
            .count(),
        0,
        "the rolled-back journal is removed"
    );
}
