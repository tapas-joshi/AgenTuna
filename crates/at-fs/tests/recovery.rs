//! Behaviour of the leftovers an interrupted write leaves: detection, classification, repair.

use std::fs::{self, File};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration as StdDuration, SystemTime};

use at_fs::FsError;
use at_fs::backup::BackupMeta;
use at_fs::recovery::{
    EntryState, JOURNAL_VERSION, JournalEntry, PendingJournal, RecoveryCallbacks, RecoveryError,
    RecoveryPolicy, RestoreRequest, assess_entry, assess_journal, discard_pending,
    journal_file_name, journal_operation_id, purge_expired_journals, read_journal,
    recover_journals, restore_file, scan_journals, scan_temp_files, sweep_stale_temp_files,
    temp_target_name, write_journal,
};
use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};
use tempfile::TempDir;
use time::{Duration, OffsetDateTime};

fn age(path: &Path, days: i64) {
    let file = File::options().write(true).open(path).unwrap();
    let when = SystemTime::now() - StdDuration::from_secs(u64::try_from(days * 86_400).unwrap());
    file.set_modified(when).unwrap();
}

fn journal(operation_id: &str, entries: Vec<JournalEntry>) -> PendingJournal {
    PendingJournal {
        journal_version: JOURNAL_VERSION,
        operation_id: operation_id.to_owned(),
        started_at: OffsetDateTime::now_utc(),
        entries,
        temp_files: Vec::new(),
        change_set: None,
        completed_ops: Vec::new(),
        created_dirs: Vec::new(),
        committed: false,
        authorized_roots: None,
        backup_root: None,
    }
}

#[test]
fn only_staged_names_are_recognised_as_debris() {
    assert_eq!(
        temp_target_name(".config.json.agentuna-tmp-4711-0"),
        Some("config.json")
    );
    assert_eq!(
        temp_target_name(".SKILL.md.agentuna-tmp-9-12"),
        Some("SKILL.md")
    );
    // No leading dot, no counter, non-numeric pid, empty original name.
    assert_eq!(temp_target_name("config.json.agentuna-tmp-4711-0"), None);
    assert_eq!(temp_target_name(".config.json.agentuna-tmp-4711"), None);
    assert_eq!(temp_target_name(".config.json.agentuna-tmp-pid-0"), None);
    assert_eq!(temp_target_name(".agentuna-tmp-4711-0"), None);
    assert_eq!(temp_target_name("config.json"), None);
    assert_eq!(temp_target_name("..agentuna-tmp-1-1"), None);
}

#[test]
fn scan_reports_debris_only_and_sorts_by_path() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let skills = root.join("skills").join("alpha");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join("SKILL.md"), b"content").unwrap();
    let staged = skills.join(".SKILL.md.agentuna-tmp-4242-3");
    fs::write(&staged, b"partial").unwrap();
    // Look-alikes that must never be reported: a real file, a missing counter, a hidden file.
    fs::write(skills.join("SKILL.md.bak"), b"backup").unwrap();
    fs::write(skills.join(".SKILL.md.agentuna-tmp-4242"), b"no counter").unwrap();
    fs::write(skills.join(".gitignore"), b"*").unwrap();

    let found = scan_temp_files(
        std::slice::from_ref(&root.to_path_buf()),
        OffsetDateTime::now_utc(),
        Duration::days(7),
    );

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, staged);
    assert_eq!(found[0].target, "SKILL.md");
    assert!(!found[0].stale);
    assert!(found[0].modified.is_some());
}

#[test]
fn sweep_removes_expired_debris_and_leaves_everything_else_alone() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    fs::create_dir_all(root).unwrap();
    let old = root.join(".config.json.agentuna-tmp-1-0");
    let fresh = root.join(".config.json.agentuna-tmp-2-0");
    fs::write(&old, b"old").unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    fs::write(root.join("config.json"), b"real").unwrap();
    age(&old, 9);

    let report = sweep_stale_temp_files(
        std::slice::from_ref(&root.to_path_buf()),
        OffsetDateTime::now_utc(),
        Duration::days(7),
    );

    assert_eq!(report.removed, vec![old.clone()]);
    assert_eq!(report.retained, vec![fresh.clone()]);
    assert!(report.failed.is_empty());
    assert!(!old.exists());
    assert!(fresh.exists());
    assert_eq!(fs::read(root.join("config.json")).unwrap(), b"real");
}

#[test]
fn journal_round_trips_and_rejects_an_unknown_version() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("pending");
    let entry = JournalEntry {
        path: sandbox.path().join("config.json"),
        before_hash: Some("aa".to_owned()),
        after_hash: "bb".to_owned(),
    };
    let original = journal("op-7", vec![entry.clone()]);

    let path = write_journal(&dir, &original).unwrap();
    assert_eq!(path, dir.join(journal_file_name("op-7")));
    assert_eq!(journal_operation_id(&path).as_deref(), Some("op-7"));
    assert_eq!(scan_journals(&dir).unwrap(), vec![path.clone()]);
    assert_eq!(read_journal(&path).unwrap(), original);

    let mut newer = original;
    newer.journal_version = JOURNAL_VERSION + 1;
    write_journal(&dir, &newer).unwrap();
    assert!(matches!(
        read_journal(&path).unwrap_err(),
        RecoveryError::JournalVersionUnsupported { .. }
    ));
}

#[test]
fn version_one_journals_remain_readable_after_the_v2_upgrade() {
    let sandbox = TempDir::new().expect("sandbox");
    let mut legacy = journal("op-v1", Vec::new());
    legacy.journal_version = 1;
    let path = write_journal(&sandbox.path().join("journal"), &legacy).expect("write v1 journal");
    assert_eq!(read_journal(&path).expect("read legacy journal"), legacy);
}

#[test]
fn classification_distinguishes_before_after_and_diverged() {
    let sandbox = tempfile::tempdir().unwrap();
    let path = sandbox.path().join("config.json");
    let before = at_fs::sha256_hex(b"before");
    let after = at_fs::sha256_hex(b"after");

    fs::write(&path, b"before").unwrap();
    let entry = JournalEntry {
        path: path.clone(),
        before_hash: Some(before.clone()),
        after_hash: after.clone(),
    };
    assert_eq!(assess_entry(&entry).unwrap(), EntryState::Before);

    fs::write(&path, b"after").unwrap();
    assert_eq!(assess_entry(&entry).unwrap(), EntryState::After);

    fs::write(&path, b"someone else").unwrap();
    match assess_entry(&entry).unwrap() {
        EntryState::Diverged { observed } => {
            assert_eq!(observed, Some(at_fs::sha256_hex(b"someone else")));
        }
        other => panic!("expected Diverged, got {other:?}"),
    }

    // A creation that never happened is still "before", not a divergence.
    let creation = JournalEntry {
        path: path.clone(),
        before_hash: None,
        after_hash: after,
    };
    fs::remove_file(&path).unwrap();
    assert_eq!(assess_entry(&creation).unwrap(), EntryState::Before);
    assert_eq!(assess_entry(&creation).unwrap().label(), "before");

    // The file disappearing after a recorded before hash is a divergence a human must settle.
    let delete_after = JournalEntry {
        path: path.clone(),
        before_hash: Some(before),
        after_hash: at_fs::sha256_hex(b"never"),
    };
    assert!(matches!(
        assess_entry(&delete_after).unwrap(),
        EntryState::Diverged { observed: None }
    ));
}

#[test]
fn a_journal_assessment_lists_every_entry_and_flags_manual_review() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("pending");
    let written = sandbox.path().join("written.json");
    let diverged = sandbox.path().join("diverged.json");
    fs::write(&written, b"after").unwrap();
    fs::write(&diverged, b"third party").unwrap();
    let pending = journal(
        "op-9",
        vec![
            JournalEntry {
                path: written.clone(),
                before_hash: Some(at_fs::sha256_hex(b"before")),
                after_hash: at_fs::sha256_hex(b"after"),
            },
            JournalEntry {
                path: diverged.clone(),
                before_hash: Some(at_fs::sha256_hex(b"before")),
                after_hash: at_fs::sha256_hex(b"after"),
            },
        ],
    );
    let path = write_journal(&dir, &pending).unwrap();

    let assessment = assess_journal(&path).unwrap();
    assert_eq!(assessment.entries.len(), 2);
    assert!(assessment.needs_manual_review());
    assert_eq!(assessment.restorable().len(), 1);
    assert_eq!(assessment.restorable()[0].entry.path, written);
}

#[test]
fn restore_writes_the_before_bytes_and_backs_up_the_current_ones() {
    let sandbox = tempfile::tempdir().unwrap();
    let dest = sandbox.path().join("config.json");
    fs::write(&dest, b"after").unwrap();
    let before = b"before".to_vec();
    let before_hash = at_fs::sha256_hex(&before);
    let after_hash = at_fs::sha256_hex(b"after");
    let folder = sandbox.path().join("backups").join("restore-op");

    let request = RestoreRequest {
        path: dest.clone(),
        before_bytes: before.clone(),
        before_hash: before_hash.clone(),
        expected_current_hash: after_hash.clone(),
        operation_folder: folder,
        backup_meta: BackupMeta::default(),
    };
    let outcome = restore_file(&request).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), before);
    assert_eq!(outcome.before_hash.as_deref(), Some(after_hash.as_str()));
    assert_eq!(outcome.after_hash, before_hash);
    let blob = outcome
        .backup_path
        .expect("restore keeps the replaced bytes");
    assert_eq!(fs::read(blob).unwrap(), b"after");
}

#[test]
fn restore_refuses_wrong_bytes_and_a_changed_destination() {
    let sandbox = tempfile::tempdir().unwrap();
    let dest = sandbox.path().join("config.json");
    fs::write(&dest, b"after").unwrap();
    let request = RestoreRequest {
        path: dest.clone(),
        before_bytes: b"not the before bytes".to_vec(),
        before_hash: at_fs::sha256_hex(b"before"),
        expected_current_hash: at_fs::sha256_hex(b"after"),
        operation_folder: sandbox.path().join("backups").join("restore-op"),
        backup_meta: BackupMeta::default(),
    };
    assert!(matches!(
        restore_file(&request).unwrap_err(),
        RecoveryError::BackupMismatch { .. }
    ));
    assert_eq!(fs::read(&dest).unwrap(), b"after");

    // Right bytes, but the destination moved on: the guard refuses instead of overwriting.
    let good = RestoreRequest {
        before_bytes: b"before".to_vec(),
        ..request
    };
    fs::write(&dest, b"changed meanwhile").unwrap();
    match restore_file(&good).unwrap_err() {
        RecoveryError::Write(FsError::ConcurrentModification { path, .. }) => {
            assert_eq!(path, dest);
        }
        other => panic!("expected ConcurrentModification, got {other:?}"),
    }
    assert_eq!(fs::read(&dest).unwrap(), b"changed meanwhile");
}

#[test]
fn discard_removes_the_journal_and_its_debris_but_not_the_destination() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("pending");
    let dest = sandbox.path().join("config.json");
    let staged = sandbox.path().join(".config.json.agentuna-tmp-77-0");
    fs::write(&dest, b"after").unwrap();
    fs::write(&staged, b"partial").unwrap();
    let mut pending = journal(
        "op-11",
        vec![JournalEntry {
            path: dest.clone(),
            before_hash: Some(at_fs::sha256_hex(b"before")),
            after_hash: at_fs::sha256_hex(b"after"),
        }],
    );
    pending.temp_files = vec![staged.clone()];
    let path = write_journal(&dir, &pending).unwrap();

    let outcome = discard_pending(&path, &pending).unwrap();

    assert_eq!(outcome.removed, vec![path.clone(), staged.clone()]);
    assert!(outcome.skipped.is_empty());
    assert!(!path.exists());
    assert!(!staged.exists());
    assert_eq!(fs::read(&dest).unwrap(), b"after");
}

#[test]
fn discard_never_deletes_a_path_that_is_not_staged_debris() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("pending");
    let dest = sandbox.path().join("config.json");
    let user_file = sandbox.path().join("notes.txt");
    fs::write(&dest, b"after").unwrap();
    fs::write(&user_file, b"mine").unwrap();
    // A hand-edited journal naming a user file must not turn Discard into a delete.
    let mut pending = journal("op-12", Vec::new());
    pending.temp_files = vec![user_file.clone()];
    let path = write_journal(&dir, &pending).unwrap();

    let outcome = discard_pending(&path, &pending).unwrap();

    assert_eq!(outcome.removed, vec![path]);
    assert_eq!(outcome.skipped, vec![user_file.clone()]);
    assert_eq!(fs::read(&user_file).unwrap(), b"mine");
}

#[test]
fn only_journals_older_than_the_retention_window_are_purged() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("pending");
    let old = write_journal(&dir, &journal("op-old", Vec::new())).unwrap();
    let fresh = write_journal(&dir, &journal("op-fresh", Vec::new())).unwrap();
    age(&old, 9);

    let removed =
        purge_expired_journals(&dir, OffsetDateTime::now_utc(), Duration::days(7)).unwrap();

    assert_eq!(removed, vec![old.clone()]);
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn scan_of_a_missing_directory_is_empty() {
    let sandbox = tempfile::tempdir().unwrap();
    let missing: PathBuf = sandbox.path().join("nope");
    assert!(scan_journals(&missing).unwrap().is_empty());
}

/// Operation recorder that supplies a fixed id; the crash happens before it sees a file.
struct CrashRecorder {
    operation_id: String,
}

impl at_fs::OperationRecorder for CrashRecorder {
    fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
        Ok(self.operation_id.clone())
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

/// Panics where a process dies: right after a delete moved its original into the backup folder.
struct CrashAfterDeleteRename;

impl at_fs::failpoint::FailInjector for CrashAfterDeleteRename {
    fn fault(
        &self,
        point: at_fs::failpoint::FaultPoint,
        _: &Path,
    ) -> Option<at_fs::failpoint::Fault> {
        if point == at_fs::failpoint::FaultPoint::DeleteBackupRename {
            panic!("simulated crash after the delete rename");
        }
        None
    }
}

#[derive(Default)]
struct RecoveryLog {
    finished: Vec<(String, bool, bool)>,
}

impl RecoveryCallbacks for RecoveryLog {
    fn before_bytes(
        &mut self,
        _: &PendingJournal,
        _: usize,
        _: &JournalEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String> {
        self.finished
            .push((operation_id.to_owned(), succeeded, manual_review));
        Ok(())
    }
}

/// Crashes a delete between the rename into the configured backup root and the operation
/// history record, then recovers the original from a fresh process.
fn recover_delete_after_rename_crash(backup_root_relative: &Path) {
    let sandbox = TempDir::new().unwrap();
    let store = sandbox.path().join("store");
    let backup_root = sandbox.path().join("support").join(backup_root_relative);
    let folder = store.join("skills/review");
    fs::create_dir_all(&folder).unwrap();
    let skill = b"---\nname: review\n---\ncanonical bytes\n";
    fs::write(folder.join("SKILL.md"), skill).unwrap();
    let before = at_fs::hash_node(&folder).unwrap().expect("folder hash");
    let operation_id = "01JAZZZZZZZZZZZZZZZZZZZZZZ";
    let set = ChangeSet {
        id: "cs-crash".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("delete").unwrap(),
        scope: Some(Scope::Project),
        project_id: None,
        summary: "delete review".to_owned(),
        ops: vec![FileOp::Delete {
            path: PathBuf::from("skills/review"),
            expected: ExpectedState::hash(before).unwrap(),
        }],
        created_at_unix_seconds: 0,
    };
    let mut recorder = CrashRecorder {
        operation_id: operation_id.to_owned(),
    };
    let mut context = at_fs::ApplyContext {
        store_root: &store,
        backup_folder: &backup_root,
        recorder: &mut recorder,
    };

    let crashed = catch_unwind(AssertUnwindSafe(|| {
        at_fs::failpoint::with_injector(Arc::new(CrashAfterDeleteRename), || {
            at_fs::apply_changeset(&set, &mut context)
        })
    }));
    assert!(crashed.is_err(), "the injector must simulate the crash");
    assert!(
        !folder.exists(),
        "the delete moved the folder into the backup root"
    );
    let journal_dir = store.join(".state/journal");
    assert_eq!(scan_journals(&journal_dir).unwrap().len(), 1);

    let mut callbacks = RecoveryLog::default();
    let policy = RecoveryPolicy::new(&store, [store.clone()], [backup_root.clone()]);
    let report = recover_journals(&journal_dir, &policy, &mut callbacks).expect("recovery runs");

    assert!(
        report.manual_review.is_empty(),
        "a backup under the configured root is found: {:?}",
        report.manual_review
    );
    assert_eq!(report.rolled_back.len(), 1);
    assert_eq!(
        callbacks.finished,
        [(operation_id.to_owned(), false, false)]
    );
    assert_eq!(fs::read(folder.join("SKILL.md")).unwrap(), skill);
    assert!(scan_journals(&journal_dir).unwrap().is_empty());
}

#[test]
fn delete_backup_under_a_migration_backup_root_is_recovered_after_a_rename_crash() {
    recover_delete_after_rename_crash(Path::new("backups/migration"));
}

#[test]
fn delete_backup_under_the_drift_backup_root_is_recovered_after_a_rename_crash() {
    recover_delete_after_rename_crash(Path::new("backups"));
}
