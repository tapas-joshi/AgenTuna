//! Startup recovery end to end: session marker, temp debris, pending journals, repair actions.
//!
//! Every test runs in a temp HOME with a temp Global store and injected database seams. No test
//! reads the real process table for a live pid, touches a real store or opens the database.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, SystemTime};

use at_app::app::recovery::{
    IntegrityReport, InterruptedOperation, PreviousExit, RecoveryError, RecoveryLocations,
    RecoveryOperations, RecoveryOutcome, RecoveryService, SessionFile, SessionMarker,
};
use at_fs::recovery::{EntryState, JOURNAL_VERSION, JournalEntry, PendingJournal, write_journal};
use at_store::instance::{ProcessProbe, ProcessState};
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};
use time::OffsetDateTime;

/// Paths for a throwaway home, with the Global store inside the sandbox instead of
/// `/Library/Application Support`.
fn sandbox_paths(root: &Path) -> AppPaths {
    AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(root.join("home")),
        overrides: PathOverrides {
            global_store: Some(root.join("global")),
            ..PathOverrides::default()
        },
        project_root: None,
    })
    .expect("resolve sandboxed paths")
}

/// A probe that reports only `live` as alive, so no test depends on the real process table.
#[derive(Debug, Clone, Copy, Default)]
struct FakeProbe {
    live: Option<u32>,
}

impl ProcessProbe for FakeProbe {
    fn inspect(&self, pid: u32) -> ProcessState {
        if self.live == Some(pid) {
            ProcessState::Alive { started_at: None }
        } else {
            ProcessState::Dead
        }
    }
}

#[derive(Debug, Default)]
struct RecordingOperations {
    interrupted: Mutex<Vec<InterruptedOperation>>,
    restores: Mutex<Vec<Vec<PathBuf>>>,
}

impl RecoveryOperations for RecordingOperations {
    fn mark_interrupted(&self, operation: &InterruptedOperation) -> Result<(), RecoveryError> {
        self.interrupted.lock().unwrap().push(operation.clone());
        Ok(())
    }

    fn record_restore(&self, files: &[PathBuf]) -> Result<String, RecoveryError> {
        let mut restores = self.restores.lock().unwrap();
        restores.push(files.to_vec());
        Ok(format!("restore-{}", restores.len()))
    }
}

fn write_marker(support: &Path, marker: &SessionMarker) {
    fs::create_dir_all(support).unwrap();
    fs::write(
        support.join("session.json"),
        serde_json::to_vec_pretty(marker).unwrap(),
    )
    .unwrap();
}

fn marker(pid: u32, clean_exit: bool) -> SessionMarker {
    SessionMarker {
        pid,
        started_at: "2026-11-05T00:00:00Z".to_owned(),
        clean_exit,
    }
}

fn age(path: &Path, days: u64) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - StdDuration::from_secs(days * 86_400))
        .unwrap();
}

fn pending_journal(paths: &AppPaths, operation_id: &str, entries: Vec<JournalEntry>) -> PathBuf {
    write_journal(
        &paths.cache_dir().join("pending"),
        &PendingJournal {
            journal_version: JOURNAL_VERSION,
            operation_id: operation_id.to_owned(),
            started_at: OffsetDateTime::now_utc(),
            entries,
            temp_files: Vec::new(),
            change_set: None,
            completed_ops: Vec::new(),
            created_dirs: Vec::new(),
            committed: false,
        },
    )
    .unwrap()
}

fn service(
    paths: &AppPaths,
    operations: &std::sync::Arc<RecordingOperations>,
    probe: FakeProbe,
) -> RecoveryService {
    RecoveryService::new(
        RecoveryLocations::from_paths(paths),
        operations.clone(),
        std::sync::Arc::new(|| -> Result<IntegrityReport, RecoveryError> {
            Ok(IntegrityReport::healthy())
        }),
    )
    .with_probe(std::sync::Arc::new(probe))
}

#[test]
fn a_clean_exit_shows_no_sheet_and_removes_only_expired_debris() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(9_999_999, true));
    let root = paths.support_dir().to_path_buf();
    let expired = root.join(".config.json.agentuna-tmp-1-0");
    let fresh = root.join(".config.json.agentuna-tmp-2-0");
    fs::write(&expired, b"old").unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    fs::write(root.join("config.json"), b"real").unwrap();
    age(&expired, 9);

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let outcome = service(&paths, &operations, FakeProbe::default())
        .run()
        .unwrap();

    match &outcome {
        RecoveryOutcome::Clean { swept, duration } => {
            assert_eq!(swept, &vec![expired.clone()]);
            assert!(*duration < StdDuration::from_secs(60));
        }
        other => panic!("expected a clean run, got {other:?}"),
    }
    assert!(outcome.sheet().is_none(), "a clean exit has no sheet");
    assert!(!expired.exists());
    assert!(fresh.exists(), "fresh debris waits for a decision");
    assert_eq!(fs::read(root.join("config.json")).unwrap(), b"real");
    assert!(operations.interrupted.lock().unwrap().is_empty());
}

#[test]
fn an_unclean_exit_reports_one_operation_and_restore_returns_the_before_bytes() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(4_242, false));

    let dest = paths.user_store().join("rules").join("AGENTS.md");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"after").unwrap();
    let before = b"before".to_vec();
    let after_hash = at_fs::sha256_hex(b"after");
    let journal = pending_journal(
        &paths,
        "op-1",
        vec![JournalEntry {
            path: dest.clone(),
            before_hash: Some(at_fs::sha256_hex(&before)),
            after_hash: after_hash.clone(),
        }],
    );

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let service = service(&paths, &operations, FakeProbe::default());
    let outcome = service.run().unwrap();

    let sheet = outcome.sheet().expect("an unclean exit produces a sheet");
    assert_eq!(sheet.interrupted.len(), 1, "one journal, one operation");
    assert_eq!(sheet.interrupted[0].id, "op-1");
    assert_eq!(sheet.interrupted[0].paths, vec![dest.clone()]);
    assert_eq!(sheet.files.len(), 1);
    assert_eq!(sheet.files[0].path, dest);
    assert_eq!(sheet.files[0].state, EntryState::After);
    assert_eq!(sheet.files[0].journal_path, journal);
    assert!(sheet.manual_review.is_empty());
    assert!(sheet.duration < StdDuration::from_secs(60));
    assert_eq!(operations.interrupted.lock().unwrap().len(), 1);
    assert!(
        operations.interrupted.lock().unwrap()[0]
            .started_at
            .is_some()
    );

    let blob = sandbox.path().join("before.bin");
    fs::write(&blob, &before).unwrap();
    let report = service.restore(&sheet.files[0], &blob).unwrap();

    assert_eq!(fs::read(&dest).unwrap(), before);
    assert_eq!(report.operation_id, "restore-1");
    assert_eq!(report.path, dest);
    assert_eq!(operations.restores.lock().unwrap().len(), 1);
    assert_eq!(operations.restores.lock().unwrap()[0], vec![dest]);
    // The replaced bytes are kept, so the restore is itself undoable.
    let kept = report.outcome.backup_path.expect("restore backs up first");
    assert_eq!(fs::read(kept).unwrap(), b"after");
}

#[test]
fn discard_removes_the_debris_keeps_the_bytes_and_keeps_the_audit_row() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(4_242, false));

    let dest = paths.user_store().join("mcp.json");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"after").unwrap();
    let staged = dest.parent().unwrap().join(".mcp.json.agentuna-tmp-4242-0");
    fs::write(&staged, b"partial").unwrap();
    let journal = PendingJournal {
        journal_version: JOURNAL_VERSION,
        operation_id: "op-2".to_owned(),
        started_at: OffsetDateTime::now_utc(),
        entries: vec![JournalEntry {
            path: dest.clone(),
            before_hash: Some(at_fs::sha256_hex(b"before")),
            after_hash: at_fs::sha256_hex(b"after"),
        }],
        temp_files: vec![staged.clone()],
        change_set: None,
        completed_ops: Vec::new(),
        created_dirs: Vec::new(),
        committed: false,
    };
    let journal_path = write_journal(&paths.cache_dir().join("pending"), &journal).unwrap();

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let service = service(&paths, &operations, FakeProbe::default());
    let outcome = service.run().unwrap();
    assert_eq!(
        outcome.sheet().expect("sheet").interrupted.len(),
        1,
        "the operation is on the sheet before discarding"
    );

    let discarded = service.discard("op-2").unwrap();

    assert_eq!(
        discarded.removed,
        vec![journal_path.clone(), staged.clone()]
    );
    assert!(!journal_path.exists());
    assert!(!staged.exists());
    assert_eq!(
        fs::read(&dest).unwrap(),
        b"after",
        "discard never touches the destination"
    );
    assert_eq!(
        operations.interrupted.lock().unwrap().len(),
        1,
        "the interrupted row stays for the audit trail"
    );
    assert!(matches!(
        service.discard("op-2").unwrap_err(),
        RecoveryError::UnknownOperation { .. }
    ));
}

#[test]
fn a_diverged_file_needs_manual_review_and_is_never_auto_written() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(4_242, false));

    let dest = paths.user_store().join("skills").join("a").join("SKILL.md");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, b"edited by hand").unwrap();
    pending_journal(
        &paths,
        "op-3",
        vec![JournalEntry {
            path: dest.clone(),
            before_hash: Some(at_fs::sha256_hex(b"before")),
            after_hash: at_fs::sha256_hex(b"after"),
        }],
    );

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let service = service(&paths, &operations, FakeProbe::default());
    let outcome = service.run().unwrap();

    let sheet = outcome.sheet().expect("sheet");
    assert!(sheet.files.is_empty(), "nothing is offered for restoring");
    assert_eq!(sheet.manual_review.len(), 1);
    assert_eq!(sheet.manual_review[0].path.as_deref(), Some(dest.as_path()));
    assert_eq!(
        sheet.manual_review[0].observed.as_deref(),
        Some(at_fs::sha256_hex(b"edited by hand").as_str())
    );
    assert_eq!(fs::read(&dest).unwrap(), b"edited by hand");

    // Even a hand-built offer for the same file is refused rather than written.
    let blob = sandbox.path().join("before.bin");
    fs::write(&blob, b"before").unwrap();
    let diverged = at_app::app::recovery::InterruptedFile {
        operation_id: "op-3".to_owned(),
        path: dest.clone(),
        state: EntryState::Diverged {
            observed: Some(at_fs::sha256_hex(b"edited by hand")),
        },
        before_hash: Some(at_fs::sha256_hex(b"before")),
        after_hash: at_fs::sha256_hex(b"after"),
        journal_path: paths.cache_dir().join("pending").join("op-3.journal"),
    };
    assert!(matches!(
        service.restore(&diverged, &blob).unwrap_err(),
        RecoveryError::NotRestorable { .. }
    ));
    assert_eq!(fs::read(&dest).unwrap(), b"edited by hand");
    assert!(operations.restores.lock().unwrap().is_empty());
}

#[test]
fn a_live_instance_owns_recovery_and_a_second_process_skips_it() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(7, false));
    pending_journal(
        &paths,
        "op-4",
        vec![JournalEntry {
            path: paths.user_store().join("mcp.json"),
            before_hash: None,
            after_hash: at_fs::sha256_hex(b"{}"),
        }],
    );

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let outcome = service(&paths, &operations, FakeProbe { live: Some(7) })
        .run()
        .unwrap();

    assert_eq!(outcome, RecoveryOutcome::Skipped { pid: 7 });
    assert!(operations.interrupted.lock().unwrap().is_empty());
    let stored = SessionFile::new(paths.support_dir())
        .read()
        .expect("the live instance's marker is untouched");
    assert_eq!(stored.pid, 7);
    assert!(!stored.clean_exit);
}

#[test]
fn a_failed_integrity_check_stops_recovery_before_any_repair() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(4_242, false));

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let service = RecoveryService::new(
        RecoveryLocations::from_paths(&paths),
        operations.clone(),
        std::sync::Arc::new(|| -> Result<IntegrityReport, RecoveryError> {
            Ok(IntegrityReport {
                ok: false,
                detail: "page 3 is corrupt".to_owned(),
            })
        }),
    )
    .with_probe(std::sync::Arc::new(FakeProbe::default()));

    match service.run().unwrap_err() {
        RecoveryError::Integrity { detail } => assert_eq!(detail, "page 3 is corrupt"),
        other => panic!("expected Integrity, got {other:?}"),
    }
    assert!(operations.interrupted.lock().unwrap().is_empty());
}

#[test]
fn the_session_marker_round_trips_and_only_our_own_exit_is_clean() {
    let sandbox = tempfile::tempdir().unwrap();
    let support = sandbox.path().join("support");
    fs::create_dir_all(&support).unwrap();
    let session = SessionFile::new(&support);

    assert!(session.read().is_none());
    assert_eq!(
        session.classify(&FakeProbe::default()),
        PreviousExit::Unknown
    );

    // A dead process leaves an unclean marker behind; the next start records itself.
    write_marker(&support, &marker(4_242, false));
    assert!(matches!(
        session.start_with(&FakeProbe::default()).unwrap(),
        PreviousExit::Unclean(_)
    ));
    let ours = session.read().unwrap();
    assert_eq!(ours.pid, std::process::id());
    assert!(!ours.clean_exit);

    assert!(session.mark_clean().unwrap());
    assert!(session.read().unwrap().clean_exit);

    // A second start sees the clean exit and stays clean.
    assert!(matches!(
        session.start_with(&FakeProbe::default()).unwrap(),
        PreviousExit::Clean(_)
    ));

    // Another instance's marker is never rewritten.
    write_marker(&support, &marker(7, false));
    assert!(!session.mark_clean().unwrap());
    assert_eq!(session.read().unwrap().pid, 7);
    assert!(matches!(
        session.start_with(&FakeProbe { live: Some(7) }).unwrap(),
        PreviousExit::Running(_)
    ));
    assert_eq!(session.read().unwrap().pid, 7);
}

#[test]
fn a_malformed_journal_is_reported_for_manual_review_not_skipped() {
    let sandbox = tempfile::tempdir().unwrap();
    let paths = sandbox_paths(sandbox.path());
    paths.create_dirs().unwrap();
    write_marker(paths.support_dir(), &marker(4_242, false));
    let dir = paths.cache_dir().join("pending");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("op-bad.journal"), b"{not json").unwrap();

    let operations = std::sync::Arc::new(RecordingOperations::default());
    let outcome = service(&paths, &operations, FakeProbe::default())
        .run()
        .unwrap();

    let sheet = outcome.sheet().expect("sheet");
    assert!(sheet.files.is_empty());
    assert_eq!(sheet.manual_review.len(), 1);
    assert_eq!(sheet.manual_review[0].operation_id, "op-bad");
    assert!(sheet.manual_review[0].path.is_none());
    assert_eq!(
        operations.interrupted.lock().unwrap().len(),
        1,
        "the unreadable journal still marks its operation interrupted"
    );
}
