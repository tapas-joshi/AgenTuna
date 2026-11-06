//! Elevation and Global-write behaviour of the shell's Global scope.
//!
//! TA-098: cancel, deny, success and read-only states, with the TF-010 temp-HOME fixture and a
//! recording authorization runner. Nothing here opens a real authorization dialog, writes a
//! machine-wide directory or touches the real home.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use at_app::app::bridge::OperationsLogError;
use at_app::app::privilege::{GlobalOperationLog, GlobalStoreService, GlobalWriteOperation};
use at_fs::WriteRequest;
use at_fs::privilege::{
    ElevationError, Elevator, EnableError, EnableOutcome, GlobalStoreStatus, GlobalWriteError,
    StoreDirectoryRequest,
};
use at_fs::testkit::TestHome;
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|error| panic!("chmod {mode:o} {}: {error}", path.display()));
}

fn running_as_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

/// Resolved application paths inside the fixture home, with the Global store beside it.
fn global_paths(home: &TestHome, store: &str) -> AppPaths {
    home.paths_with(|root| {
        AppPaths::resolve(&PathEnvironment {
            home: HomeSource::Injected(root.join("home")),
            overrides: PathOverrides {
                global_store: Some(root.join(store)),
                ..PathOverrides::default()
            },
            project_root: None,
        })
        .expect("resolve fixture paths")
    })
}

fn listing(home: &TestHome, dir: &Path) -> BTreeMap<String, String> {
    home.snapshot(dir)
        .unwrap_or_else(|error| panic!("snapshot {}: {error}", dir.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// The user dismissed the authorization prompt.
    Cancel,
    /// Authorization was rejected.
    Deny,
    /// Authorization succeeded and the directory was prepared.
    Prepare,
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
}

impl Elevator for RecordingElevator {
    fn elevate(&self, request: &StoreDirectoryRequest) -> Result<(), ElevationError> {
        self.requests.lock().unwrap().push(request.clone());
        match self.behaviour {
            Behaviour::Cancel => Err(ElevationError::Cancelled),
            Behaviour::Deny => Err(ElevationError::NotAuthorized),
            Behaviour::Prepare => {
                fs::create_dir_all(&request.path).unwrap();
                set_mode(&request.path, 0o755);
                Ok(())
            }
        }
    }
}

#[derive(Debug, Default)]
struct RecordingOperationLog {
    rows: Mutex<Vec<GlobalWriteOperation>>,
}

impl RecordingOperationLog {
    fn rows(&self) -> Vec<GlobalWriteOperation> {
        self.rows.lock().unwrap().clone()
    }
}

impl GlobalOperationLog for RecordingOperationLog {
    fn record(&self, operation: &GlobalWriteOperation) -> Result<(), OperationsLogError> {
        self.rows.lock().unwrap().push(operation.clone());
        Ok(())
    }
}

#[test]
fn cancel_leaves_the_global_directory_identical_and_writes_nothing() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let home = TestHome::new().unwrap();
    let paths = global_paths(&home, "global");
    let store = paths.global_store().to_path_buf();
    fs::create_dir_all(&store).unwrap();
    set_mode(&store, 0o555);
    let before = listing(&home, &store);

    let elevator = Arc::new(RecordingElevator::new(Behaviour::Cancel));
    let log = Arc::new(RecordingOperationLog::default());
    let service = GlobalStoreService::new(paths, elevator.clone()).with_operation_log(log.clone());

    match service.enable_global_writes().unwrap_err() {
        EnableError::Cancelled { status } => {
            assert!(matches!(status, GlobalStoreStatus::ReadOnly { .. }));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }

    assert_eq!(elevator.calls(), 1, "one prompt, no retry");
    assert!(matches!(
        service.status(),
        GlobalStoreStatus::ReadOnly { .. }
    ));
    assert_eq!(listing(&home, &store), before, "no temp file, no bytes");
    assert_eq!(fs::read_dir(&store).unwrap().count(), 0);
    assert!(log.rows().is_empty());
    set_mode(&store, 0o755);
}

#[test]
fn deny_surfaces_not_authorized_once_and_records_no_operations_row() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let home = TestHome::new().unwrap();
    let paths = global_paths(&home, "global");
    let store = paths.global_store().to_path_buf();
    fs::create_dir_all(&store).unwrap();
    set_mode(&store, 0o555);
    let before = listing(&home, &store);

    let elevator = Arc::new(RecordingElevator::new(Behaviour::Deny));
    let log = Arc::new(RecordingOperationLog::default());
    let service = GlobalStoreService::new(paths, elevator.clone()).with_operation_log(log.clone());

    match service.enable_global_writes().unwrap_err() {
        EnableError::NotAuthorized { status } => {
            assert!(matches!(status, GlobalStoreStatus::ReadOnly { .. }));
        }
        other => panic!("expected NotAuthorized, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 1, "authorization is never retried");

    // The write intent that follows is refused by the gate, not by another prompt.
    match service
        .write(WriteRequest::create(
            store.join("config.json"),
            b"{}".to_vec(),
        ))
        .unwrap_err()
    {
        GlobalWriteError::ReadOnly { .. } => {}
        other => panic!("expected ReadOnly, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 1);
    assert_eq!(listing(&home, &store), before);
    assert!(log.rows().is_empty(), "a refused write has no row");
    set_mode(&store, 0o755);
}

#[test]
fn success_prepares_the_store_writes_the_file_and_records_exactly_one_row() {
    let home = TestHome::new().unwrap();
    let paths = global_paths(&home, "missing-global");
    let store: PathBuf = paths.global_store().to_path_buf();
    assert!(!store.exists());

    let elevator = Arc::new(RecordingElevator::new(Behaviour::Prepare));
    let log = Arc::new(RecordingOperationLog::default());
    let service = GlobalStoreService::new(paths, elevator.clone()).with_operation_log(log.clone());

    assert!(matches!(
        service.status(),
        GlobalStoreStatus::Unavailable { .. }
    ));
    assert_eq!(
        service.enable_global_writes().unwrap(),
        EnableOutcome::Prepared
    );
    assert!(matches!(service.status(), GlobalStoreStatus::Available));
    assert_eq!(elevator.calls(), 1);
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o755,
        "the prepared directory is owned by the invoking user"
    );

    let target = store.join("config.json");
    let outcome = service
        .write(WriteRequest::create(target.clone(), b"{\"v\":1}".to_vec()))
        .unwrap();
    assert_eq!(outcome.bytes, 7);
    assert_eq!(fs::read(&target).unwrap(), b"{\"v\":1}");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600,
        "a staged file is private to the user"
    );

    let rows = log.rows();
    assert_eq!(rows.len(), 1, "one row per elevated write");
    assert_eq!(rows[0].files, vec![target]);
    assert_eq!(rows[0].root, store);
    assert_eq!(GlobalWriteOperation::KIND, "global_write");
    assert_eq!(GlobalWriteOperation::STATUS, "succeeded");
    assert_eq!(elevator.calls(), 1, "writes never prompt again");
}

#[test]
fn a_read_only_store_refuses_every_intent_before_the_runner_is_invoked() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let home = TestHome::new().unwrap();
    let paths = global_paths(&home, "global");
    let store = paths.global_store().to_path_buf();
    fs::create_dir_all(&store).unwrap();
    set_mode(&store, 0o555);
    let before = listing(&home, &store);

    // `Prepare` is the behaviour that would succeed, so a call would be visible in the count.
    let elevator = Arc::new(RecordingElevator::new(Behaviour::Prepare));
    let log = Arc::new(RecordingOperationLog::default());
    let service = GlobalStoreService::new(paths, elevator.clone()).with_operation_log(log.clone());

    for path in [
        store.join("config.json"),
        store.join("skills").join("alpha").join("SKILL.md"),
    ] {
        match service
            .write(WriteRequest::create(path, b"{}".to_vec()))
            .unwrap_err()
        {
            GlobalWriteError::ReadOnly { .. } => {}
            other => panic!("expected ReadOnly, got {other:?}"),
        }
    }

    assert_eq!(elevator.calls(), 0, "the gate runs before any prompt");
    assert_eq!(listing(&home, &store), before);
    assert!(log.rows().is_empty());
    set_mode(&store, 0o755);
}

#[test]
fn a_writable_store_from_app_paths_writes_without_elevating() {
    let home = TestHome::new().unwrap();
    let paths = global_paths(&home, "global");
    let store = paths.global_store().to_path_buf();
    fs::create_dir_all(&store).unwrap();
    set_mode(&store, 0o755);

    let elevator = Arc::new(RecordingElevator::new(Behaviour::Prepare));
    let log = Arc::new(RecordingOperationLog::default());
    let service = GlobalStoreService::new(paths, elevator.clone()).with_operation_log(log.clone());

    let target = store.join("mcp.json");
    service
        .write(WriteRequest::create(target.clone(), b"{}".to_vec()))
        .unwrap();

    assert_eq!(elevator.calls(), 0, "non-admin setups never elevate");
    assert_eq!(fs::read(&target).unwrap(), b"{}");
    assert_eq!(log.rows().len(), 1);
    assert_eq!(log.rows()[0].files, vec![target]);
}
