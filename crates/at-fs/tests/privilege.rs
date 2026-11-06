//! Behaviour of the Global store probe, the read-only write gate and one-shot elevation.
//!
//! Every test runs in a temporary sandbox. The osascript elevator is replaced by a recording fake,
//! so no test opens an authorization dialog, touches `/Library` or requires administrator rights.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use at_fs::WriteRequest;
use at_fs::privilege::{
    ElevationError, Elevator, EnableError, EnableOutcome, GlobalStore, GlobalStoreStatus,
    GlobalWriteError, StoreDirectoryRequest, StoreOwner, enable_global_writes,
    osascript_invocation,
};

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|error| panic!("chmod {mode:o} {}: {error}", path.display()));
}

fn running_as_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

/// What the fake elevator does when called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Cancel,
    Deny,
    CreateDirectory,
    SucceedWithoutChange,
    Unsupported,
}

#[derive(Debug)]
struct RecordingElevator {
    behaviour: Behaviour,
    requests: Mutex<Vec<StoreDirectoryRequest>>,
}

impl RecordingElevator {
    fn new(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn requests(&self) -> Vec<StoreDirectoryRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Elevator for RecordingElevator {
    fn elevate(&self, request: &StoreDirectoryRequest) -> Result<(), ElevationError> {
        self.requests.lock().unwrap().push(request.clone());
        match self.behaviour {
            Behaviour::Cancel => Err(ElevationError::Cancelled),
            Behaviour::Deny => Err(ElevationError::NotAuthorized),
            Behaviour::Unsupported => Err(ElevationError::Unsupported),
            Behaviour::SucceedWithoutChange => Ok(()),
            Behaviour::CreateDirectory => {
                // Stands in for the privileged mkdir/chown: in the sandbox the test user is the
                // owner, so creating the directory is the whole observable effect.
                fs::create_dir_all(&request.path).unwrap();
                set_mode(&request.path, 0o755);
                Ok(())
            }
        }
    }
}

#[test]
fn write_gate_rejects_a_parent_component_that_climbs_out_of_the_store() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    let escaped = store.join("..").join("escaped.json");

    let request = WriteRequest::create(escaped.clone(), b"{}".to_vec());
    let error = GlobalStore::new(&store).write(request).unwrap_err();
    assert!(matches!(error, GlobalWriteError::OutsideStore { .. }));
    assert!(!sandbox.path().join("escaped.json").exists());
}

#[test]
fn enable_makes_an_existing_read_only_store_writable() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);
    let elevator = RecordingElevator::new(Behaviour::CreateDirectory);

    let outcome = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap();
    assert_eq!(outcome, EnableOutcome::Prepared);
    assert_eq!(elevator.calls(), 1);
    assert_eq!(
        GlobalStoreStatus::probe(&store),
        GlobalStoreStatus::Available
    );
}

#[test]
fn probe_reports_an_absent_directory_as_unavailable_with_the_checked_path() {
    let sandbox = tempfile::tempdir().unwrap();
    let missing = sandbox.path().join("global");
    let status = GlobalStoreStatus::probe(&missing);
    match status {
        GlobalStoreStatus::Unavailable { path, reason } => {
            assert_eq!(path, missing);
            assert!(reason.contains("does not exist"), "reason: {reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn probe_reports_a_writable_directory_as_available() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    assert_eq!(
        GlobalStoreStatus::probe(&store),
        GlobalStoreStatus::Available
    );
    assert!(GlobalStoreStatus::probe(&store).is_available());
}

#[test]
fn probe_reports_a_read_only_directory_with_its_reason() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);
    let status = GlobalStoreStatus::probe(&store);
    match &status {
        GlobalStoreStatus::ReadOnly { path, reason } => {
            assert_eq!(path, &store);
            assert!(!reason.is_empty());
        }
        other => panic!("expected ReadOnly, got {other:?}"),
    }
    assert_eq!(status.state(), "read_only");
    assert!(status.reason().is_some());
    set_mode(&store, 0o755);
}

#[test]
fn probe_rejects_relative_parent_and_control_character_paths() {
    let sandbox = tempfile::tempdir().unwrap();
    let relative = PathBuf::from("global");
    match GlobalStoreStatus::probe(&relative) {
        GlobalStoreStatus::Unavailable { reason, .. } => {
            assert!(reason.contains("not absolute"), "reason: {reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }

    let parent = sandbox.path().join("a").join("..").join("global");
    match GlobalStoreStatus::probe(&parent) {
        GlobalStoreStatus::Unavailable { reason, .. } => {
            assert!(reason.contains("parent"), "reason: {reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }

    let control = sandbox.path().join("bad\u{7}name");
    match GlobalStoreStatus::probe(&control) {
        GlobalStoreStatus::Unavailable { reason, .. } => {
            assert!(reason.contains("control"), "reason: {reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn write_gate_refuses_a_read_only_store_before_staging_anything() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);

    let request = WriteRequest::create(store.join("config.json"), b"{\"v\":1}".to_vec());
    let error = GlobalStore::new(&store).write(request).unwrap_err();
    match &error {
        GlobalWriteError::ReadOnly { path, .. } => assert_eq!(path, &store),
        other => panic!("expected ReadOnly, got {other:?}"),
    }
    let entries: Vec<_> = fs::read_dir(&store).unwrap().collect();
    assert!(
        entries.is_empty(),
        "a refused write must not stage a temp file: {entries:?}"
    );
    set_mode(&store, 0o755);
}

#[test]
fn write_gate_rejects_paths_outside_the_store_root() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    let outside = sandbox.path().join("elsewhere");
    fs::create_dir(&store).unwrap();
    fs::create_dir(&outside).unwrap();

    let request = WriteRequest::create(outside.join("config.json"), b"{}".to_vec());
    let error = GlobalStore::new(&store).write(request).unwrap_err();
    match error {
        GlobalWriteError::OutsideStore { path, root } => {
            assert_eq!(path, outside.join("config.json"));
            assert_eq!(root, store);
        }
        other => panic!("expected OutsideStore, got {other:?}"),
    }
    assert!(!outside.join("config.json").exists());
}

#[test]
fn write_gate_applies_an_atomic_write_inside_an_available_store() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    let file = store.join("config.json");

    let outcome =
        GlobalStore::new(&store).write(WriteRequest::create(&file, b"{\"v\":1}".to_vec()));
    let outcome = outcome.unwrap();
    assert_eq!(outcome.bytes, 7);
    assert!(!outcome.after_hash.is_empty());
    assert_eq!(fs::read(&file).unwrap(), b"{\"v\":1}");
}

#[test]
fn enable_skips_elevation_when_the_store_is_already_writable() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    let elevator = RecordingElevator::new(Behaviour::Cancel);

    let outcome = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap();
    assert_eq!(outcome, EnableOutcome::AlreadyWritable);
    assert_eq!(
        elevator.calls(),
        0,
        "no prompt for an already writable store"
    );
}

#[test]
fn enable_cancelled_leaves_a_read_only_store_untouched() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);
    let before = fs::read_dir(&store).unwrap().count();
    let elevator = RecordingElevator::new(Behaviour::Cancel);

    let error = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap_err();
    match error {
        EnableError::Cancelled { status } => {
            assert!(matches!(status, GlobalStoreStatus::ReadOnly { .. }));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 1, "exactly one authorization prompt");
    assert_eq!(fs::read_dir(&store).unwrap().count(), before);
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o555
    );
    set_mode(&store, 0o755);
}

#[test]
fn enable_denied_reports_not_authorized_once() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("missing-global");
    let elevator = RecordingElevator::new(Behaviour::Deny);

    let error = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap_err();
    assert!(matches!(error, EnableError::NotAuthorized { .. }));
    assert_eq!(elevator.calls(), 1, "denial is not retried");
    assert!(!store.exists(), "a denied elevation creates nothing");
}

#[test]
fn enable_success_prepares_the_directory_with_one_request() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    let elevator = RecordingElevator::new(Behaviour::CreateDirectory);
    let owner = StoreOwner::new(501, 20);

    let outcome = enable_global_writes(&store, owner, &elevator).unwrap();
    assert_eq!(outcome, EnableOutcome::Prepared);
    assert_eq!(
        GlobalStoreStatus::probe(&store),
        GlobalStoreStatus::Available
    );
    let requests = elevator.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, store);
    assert_eq!(requests[0].owner, owner);
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn enable_success_without_a_writable_directory_is_not_effective() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    let elevator = RecordingElevator::new(Behaviour::SucceedWithoutChange);

    let error = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap_err();
    match error {
        EnableError::NotEffective { status } => {
            assert!(matches!(status, GlobalStoreStatus::Unavailable { .. }));
        }
        other => panic!("expected NotEffective, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 1);
}

#[test]
fn enable_unsupported_platform_is_reported_without_a_second_attempt() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    let elevator = RecordingElevator::new(Behaviour::Unsupported);

    let error = enable_global_writes(&store, StoreOwner::new(501, 20), &elevator).unwrap_err();
    assert!(matches!(error, EnableError::Unsupported));
    assert_eq!(elevator.calls(), 1);
}

#[test]
fn enable_rejects_an_invalid_path_without_prompting() {
    let elevator = RecordingElevator::new(Behaviour::CreateDirectory);
    let error = enable_global_writes(
        Path::new("relative/global"),
        StoreOwner::new(501, 20),
        &elevator,
    )
    .unwrap_err();
    match error {
        EnableError::InvalidPath { reason, .. } => {
            assert!(reason.contains("not absolute"), "reason: {reason}");
        }
        other => panic!("expected InvalidPath, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 0, "an invalid path is never elevated");
}

#[test]
fn osascript_invocation_passes_the_path_as_its_own_argument() {
    let path = PathBuf::from("/Library/Application Support/AgenTuna/global's test");
    let request = StoreDirectoryRequest::new(&path, StoreOwner::new(501, 20));
    let invocation = osascript_invocation(&request);

    assert_eq!(invocation.executable, Path::new("/usr/bin/osascript"));
    let script = invocation.arguments[0].to_str().unwrap();
    assert_eq!(script, "-e");
    let apple_script = invocation.arguments[1].to_str().unwrap();
    assert!(apple_script.contains("with administrator privileges"));
    assert!(apple_script.contains("quoted form of"));
    assert!(
        !apple_script.contains("global's test"),
        "the path must not be interpolated into the AppleScript source"
    );
    let separator = invocation
        .arguments
        .iter()
        .position(|argument| argument.to_str() == Some("--"))
        .expect("argument separator");
    assert_eq!(
        invocation.arguments[separator + 1].as_os_str(),
        path.as_os_str()
    );
    assert_eq!(invocation.arguments[separator + 2].to_str(), Some("501:20"));
    assert_eq!(invocation.arguments.len(), separator + 3);
}
