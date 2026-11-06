//! Behaviour tests for the advisory instance lock and CLI coexistence.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::*;

fn lock_dir() -> (TempDir, InstanceLock) {
    let temp = TempDir::new().expect("temp dir");
    let lock = InstanceLock::new(temp.path());
    (temp, lock)
}

fn record(pid: u32, mode: Mode) -> InstanceRecord {
    InstanceRecord {
        pid,
        version: LOCK_VERSION.to_owned(),
        started_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .expect("format timestamp"),
        mode,
    }
}

fn write_record_file(path: &Path, record: &InstanceRecord) {
    fs::write(path, serde_json::to_string(record).expect("serialize")).expect("write lock");
}

fn quarantine_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(QUARANTINE_PREFIX))
        })
        .collect()
}

#[test]
fn the_lock_file_contains_pid_version_started_at_and_mode() {
    let (_temp, lock) = lock_dir();
    let guard = lock.try_acquire(Mode::App).expect("acquire");

    let text = fs::read_to_string(lock.lock_path()).expect("read lock");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    assert_eq!(value["pid"], serde_json::json!(std::process::id()));
    assert_eq!(value["version"], LOCK_VERSION);
    assert_eq!(value["mode"], "app");
    let started_at = value["started_at"].as_str().expect("started_at string");
    assert!(OffsetDateTime::parse(started_at, &Rfc3339).is_ok());

    guard.release();
    assert!(
        !lock.lock_path().exists(),
        "clean release removes its record"
    );
}

#[test]
fn a_second_exclusive_acquire_reports_the_holder_pid_and_version() {
    let (_temp, lock) = lock_dir();
    let guard = lock.try_acquire(Mode::CliWrite).expect("first write");

    match lock.try_acquire(Mode::CliWrite) {
        Err(LockError::Busy {
            holder: Some(holder),
        }) => {
            assert_eq!(holder.pid, std::process::id());
            assert_eq!(holder.version, LOCK_VERSION);
            assert_eq!(holder.mode, Mode::CliWrite);
        }
        other => panic!("expected Busy with a holder, got {other:?}"),
    }

    guard.release();
    assert!(lock.try_acquire(Mode::CliWrite).is_ok());
}

#[test]
fn a_second_app_launch_reports_already_running_and_never_steals_the_live_record() {
    let (_temp, lock) = lock_dir();
    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();

    match lock.try_acquire(Mode::App) {
        Err(LockError::AlreadyRunning { holder }) => assert_eq!(holder, app_record),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    assert_eq!(lock.read_record(), RecordState::Valid(app_record.clone()));
    assert!(lock.release_record(&app_record));
    assert!(!lock.lock_path().exists());
}

#[test]
fn a_cli_write_while_the_app_is_idle_succeeds_serially() {
    let (_temp, lock) = lock_dir();
    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();

    let writer = lock
        .try_acquire(Mode::CliWrite)
        .expect("idle app allows a write");
    assert!(writer.is_locked());
    assert_eq!(writer.mode(), Mode::CliWrite);
    assert!(writer.ensure_writable().is_ok());
    assert_eq!(lock.read_record(), RecordState::Valid(app_record.clone()));
    writer.release();

    // The CLI never took over the app's record: the app remains detectable.
    assert_eq!(lock.read_record(), RecordState::Valid(app_record.clone()));
    assert!(lock.release_record(&app_record));
}

#[test]
fn shared_readers_coexist_and_never_touch_the_app_record() {
    let (_temp, lock) = lock_dir();
    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();

    let first = lock.try_acquire(Mode::CliRead).expect("first reader");
    let second = lock.try_acquire(Mode::CliRead).expect("second reader");
    assert!(first.is_locked());
    assert!(second.is_locked());
    assert_eq!(lock.read_record(), RecordState::Valid(app_record.clone()));
    drop(first);
    drop(second);
    assert_eq!(lock.read_record(), RecordState::Valid(app_record.clone()));
    assert!(lock.release_record(&app_record));
}

#[test]
fn a_shared_reader_blocks_the_exclusive_writer_until_released() {
    let (_temp, lock) = lock_dir();
    let reader = lock.try_acquire(Mode::ReadOnly).expect("reader");
    // The reader created no file, so there is nothing to block; create one first.
    drop(reader);

    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();

    let reader = lock.try_acquire(Mode::CliRead).expect("reader");
    match lock.try_acquire(Mode::CliWrite) {
        Err(LockError::Busy { .. }) => {}
        other => panic!("expected Busy while a reader holds the lock, got {other:?}"),
    }
    drop(reader);
    let writer = lock
        .try_acquire(Mode::CliWrite)
        .expect("writer after the reader left");
    writer.release();
    assert!(lock.release_record(&app_record));
}

#[test]
fn the_system_probe_sees_a_live_child_and_a_reaped_pid() {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    match SystemProbe.inspect(pid) {
        ProcessState::Alive { started_at } => {
            assert!(started_at.is_some(), "ps reports an elapsed time");
        }
        other => panic!("expected Alive, got {other:?}"),
    }
    child.kill().expect("kill child");
    child.wait().expect("reap child");
    assert_eq!(SystemProbe.inspect(pid), ProcessState::Dead);
}

#[test]
fn a_live_foreign_process_blocks_a_second_app() {
    let (_temp, lock) = lock_dir();
    let mut child = Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("spawn child");
    let foreign = record(child.id(), Mode::App);
    write_record_file(&lock.lock_path(), &foreign);

    match lock.try_acquire(Mode::App) {
        Err(LockError::AlreadyRunning { holder }) => assert_eq!(holder.pid, child.id()),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    assert_eq!(lock.read_record(), RecordState::Valid(foreign));

    child.kill().expect("kill child");
    child.wait().expect("reap child");
}

#[test]
fn a_dead_record_is_reclaimed_and_replaced() {
    let (_temp, lock) = lock_dir();
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn child");
    let dead_pid = child.id();
    child.wait().expect("reap child");
    write_record_file(&lock.lock_path(), &record(dead_pid, Mode::App));

    let guard = lock.try_acquire(Mode::App).expect("stale record reclaimed");
    assert_eq!(guard.record().pid, std::process::id());
    assert!(
        lock.read_record()
            .matches_valid(|record| record.pid == std::process::id())
    );
    assert!(
        quarantine_files(lock.support_dir()).is_empty(),
        "a dead record is replaced, not quarantined"
    );
    guard.release();
}

#[test]
fn pid_reuse_is_detected_from_the_process_start_time() {
    let (_temp, lock) = lock_dir();
    let recorded = OffsetDateTime::now_utc() - Duration::from_secs(600);
    let stale = InstanceRecord {
        pid: 4242,
        version: LOCK_VERSION.to_owned(),
        started_at: recorded.format(&Rfc3339).expect("format"),
        mode: Mode::App,
    };
    write_record_file(&lock.lock_path(), &stale);
    // The pid exists, but the process started long after the record was written.
    let probe = FixedProbe::alive(
        4242,
        Some(OffsetDateTime::now_utc() - Duration::from_secs(5)),
    );

    let guard = lock
        .acquire_with(Mode::App, LockPolicy::NONBLOCKING, &probe)
        .expect("a reused pid is stale");
    assert_eq!(guard.record().pid, std::process::id());
    guard.release();
}

#[test]
fn an_unknown_probe_never_steals_a_record() {
    let (_temp, lock) = lock_dir();
    write_record_file(&lock.lock_path(), &record(4242, Mode::App));
    match lock.acquire_with(Mode::App, LockPolicy::NONBLOCKING, &FixedProbe::unknown()) {
        Err(LockError::AlreadyRunning { holder }) => assert_eq!(holder.pid, 4242),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
}

#[test]
fn a_corrupt_lock_file_is_quarantined_and_startup_continues() {
    let (_temp, lock) = lock_dir();
    fs::write(lock.lock_path(), b"not json at all").expect("write garbage");

    let guard = lock.try_acquire(Mode::App).expect("startup continues");
    assert!(
        lock.read_record()
            .matches_valid(|record| record.pid == std::process::id())
    );
    let quarantined = quarantine_files(lock.support_dir());
    assert_eq!(
        quarantined.len(),
        1,
        "one corrupt copy is kept: {quarantined:?}"
    );
    guard.release();
}

#[test]
fn read_only_never_creates_or_modifies_the_lock_file() {
    let (_temp, lock) = lock_dir();
    assert!(!lock.lock_path().exists());

    let guard = lock.try_acquire(Mode::ReadOnly).expect("no lock needed");
    assert!(!guard.is_locked());
    assert!(
        !lock.lock_path().exists(),
        "read-only must not create the file"
    );
    assert!(matches!(guard.ensure_writable(), Err(LockError::ReadOnly)));

    // With an existing app record the read-only run leaves the bytes untouched.
    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();
    let before = fs::read_to_string(lock.lock_path()).expect("read lock");

    let reader = lock.try_acquire(Mode::ReadOnly).expect("read-only reader");
    assert!(reader.is_locked());
    assert!(matches!(reader.ensure_writable(), Err(LockError::ReadOnly)));
    drop(reader);
    assert_eq!(
        fs::read_to_string(lock.lock_path()).expect("read lock"),
        before
    );
    assert!(lock.release_record(&app_record));
}

#[test]
fn release_never_removes_another_instances_record() {
    let (_temp, lock) = lock_dir();
    let guard = lock.try_acquire(Mode::CliWrite).expect("acquire");
    // Another process replaces the record while this guard runs (same file, new content).
    let replacement = record(4242, Mode::App);
    write_record_file(&lock.lock_path(), &replacement);

    guard.release();
    assert_eq!(lock.read_record(), RecordState::Valid(replacement));
}

#[test]
fn a_waiting_policy_times_out_with_the_lock_holder() {
    let (_temp, lock) = lock_dir();
    let holder = lock.try_acquire(Mode::CliWrite).expect("holder");

    let started = Instant::now();
    match lock.acquire_with(
        Mode::CliWrite,
        LockPolicy::waiting(Duration::from_millis(150)),
        &SystemProbe,
    ) {
        Err(LockError::Busy {
            holder: Some(holder),
        }) => {
            assert_eq!(holder.pid, std::process::id());
            assert_eq!(holder.version, LOCK_VERSION);
        }
        other => panic!("expected Busy with a holder, got {other:?}"),
    }
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "the policy waits before giving up"
    );
    drop(holder);
}

#[test]
fn status_reports_free_recorded_and_corrupt() {
    let (_temp, lock) = lock_dir();
    assert_eq!(lock.status(&SystemProbe), LockStatus::Free);

    let app = lock.try_acquire(Mode::App).expect("app acquires");
    let app_record = app.record().clone();
    app.release_keeping_record();
    assert_eq!(
        lock.status(&SystemProbe),
        LockStatus::Recorded {
            record: app_record.clone(),
            alive: true,
        }
    );
    assert!(lock.release_record(&app_record));

    fs::write(lock.lock_path(), b"{").expect("write partial json");
    match lock.status(&SystemProbe) {
        LockStatus::Corrupt { reason, .. } => assert!(!reason.is_empty()),
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn elapsed_time_parsing_handles_every_ps_shape() {
    assert_eq!(
        parse_elapsed_time("05:07\n"),
        Some(Duration::from_secs(307))
    );
    assert_eq!(
        parse_elapsed_time("01:02:03"),
        Some(Duration::from_secs(3_723))
    );
    assert_eq!(
        parse_elapsed_time("2-03:04:05"),
        Some(Duration::from_secs(2 * 86_400 + 3 * 3_600 + 4 * 60 + 5))
    );
    assert_eq!(parse_elapsed_time(""), None);
    assert_eq!(parse_elapsed_time("nonsense"), None);
    assert_eq!(parse_elapsed_time("1:2:3:4"), None);
}

#[test]
fn the_record_round_trips_through_json() {
    let original = record(4711, Mode::CliWrite);
    let text = serde_json::to_string(&original).expect("serialize");
    let parsed: InstanceRecord = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(parsed, original);
    assert_eq!(parsed.mode.as_str(), "cli_write");
    assert!(parsed.started_at().is_some());
}

/// Assertion helper: matches [`RecordState::Valid`] with a predicate.
trait RecordStateExt {
    fn matches_valid(&self, predicate: impl FnOnce(&InstanceRecord) -> bool) -> bool;
}

impl RecordStateExt for RecordState {
    fn matches_valid(&self, predicate: impl FnOnce(&InstanceRecord) -> bool) -> bool {
        match self {
            RecordState::Valid(record) => predicate(record),
            _ => false,
        }
    }
}
