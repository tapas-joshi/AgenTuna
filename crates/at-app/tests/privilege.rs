//! Shell-level behaviour of the Global scope: resolved path, read-only gate, elevation and locks.
//!
//! Sandboxed like every other test: injected home, temp Global store, recording elevator. No test
//! opens a real authorization dialog or touches a machine-wide directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use at_app::app::privilege::{GlobalStoreService, LockReadError};
use at_fs::WriteRequest;
use at_fs::privilege::{
    ElevationError, Elevator, EnableError, EnableOutcome, GlobalStoreStatus, GlobalWriteError,
    StoreDirectoryRequest,
};
use at_model::Lock;
use at_store::paths::{AppPaths, GlobalStoreSource, HomeSource, PathEnvironment, PathOverrides};

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|error| panic!("chmod {mode:o} {}: {error}", path.display()));
}

fn running_as_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

fn paths_with_global(sandbox: &Path, global: PathBuf) -> AppPaths {
    AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(sandbox.join("home")),
        overrides: PathOverrides {
            global_store: Some(global),
            ..PathOverrides::default()
        },
        project_root: None,
    })
    .unwrap()
}

fn lock(reason: &str) -> Lock {
    serde_json::from_value(serde_json::json!({
        "scope": "global",
        "item_id": at_model::ItemId::new().to_string(),
        "reason": reason,
        "locked_by": "cli",
        "locked_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Cancel,
    CreateDirectory,
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
            Behaviour::CreateDirectory => {
                fs::create_dir_all(&request.path).unwrap();
                set_mode(&request.path, 0o755);
                Ok(())
            }
        }
    }
}

#[test]
fn service_uses_the_global_store_override_from_the_environment() {
    let sandbox = tempfile::tempdir().unwrap();
    let global = sandbox.path().join("tenant").join("global");
    let paths = paths_with_global(sandbox.path(), global.clone());
    let service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );

    assert_eq!(service.path(), global);
    assert_eq!(service.source(), GlobalStoreSource::Environment);
    match service.status() {
        GlobalStoreStatus::Unavailable { path, .. } => assert_eq!(path, global),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn settings_global_store_wins_over_the_default_and_is_probed() {
    let sandbox = tempfile::tempdir().unwrap();
    let environment = PathEnvironment {
        home: HomeSource::Injected(sandbox.path().join("home")),
        ..PathEnvironment::default()
    };
    let paths = AppPaths::resolve(&environment).unwrap();
    let default = paths.global_store().to_path_buf();
    let support = paths.support_dir().to_path_buf();
    let mut service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );
    assert!(service.apply_settings_global_store(Some(Path::new("tenant/global"))));

    let expected = support.join("tenant/global");
    assert_ne!(expected, default);
    assert_eq!(service.source(), GlobalStoreSource::Settings);
    assert_eq!(service.path(), expected);
    match service.status() {
        GlobalStoreStatus::Unavailable { path, .. } => assert_eq!(path, expected),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn environment_override_beats_the_settings_value() {
    let sandbox = tempfile::tempdir().unwrap();
    let global = sandbox.path().join("from-environment");
    let paths = paths_with_global(sandbox.path(), global.clone());
    let mut service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );

    assert!(!service.apply_settings_global_store(Some(Path::new("tenant/global"))));
    assert_eq!(service.path(), global);
    assert_eq!(service.source(), GlobalStoreSource::Environment);
}

#[test]
fn overview_lists_locks_from_the_reader_even_when_the_store_is_read_only() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    // A lock-shaped file in the store must never be read: the Global store is not a lock source.
    fs::write(store.join("locks.json"), b"{\"reason\":\"from a file\"}").unwrap();
    set_mode(&store, 0o555);

    let paths = paths_with_global(sandbox.path(), store.clone());
    let service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );
    let reader = || -> Result<Vec<Lock>, LockReadError> { Ok(vec![lock("from the locks table")]) };

    let overview = service.overview(&reader).unwrap();
    assert!(matches!(
        overview.status,
        GlobalStoreStatus::ReadOnly { .. }
    ));
    assert!(overview.needs_elevation());
    assert_eq!(overview.path, store);
    assert_eq!(overview.locks.len(), 1);
    assert_eq!(overview.locks[0].reason, "from the locks table");
    assert_eq!(
        fs::read_to_string(store.join("locks.json")).unwrap(),
        "{\"reason\":\"from a file\"}"
    );
    set_mode(&store, 0o755);
}

#[test]
fn overview_lists_locks_when_the_store_is_absent() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("missing-global");
    let paths = paths_with_global(sandbox.path(), store);
    let service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );
    let reader = || -> Result<Vec<Lock>, LockReadError> { Ok(vec![lock("policy")]) };

    let overview = service.overview(&reader).unwrap();
    assert!(matches!(
        overview.status,
        GlobalStoreStatus::Unavailable { .. }
    ));
    assert_eq!(overview.locks.len(), 1);
    assert!(overview.needs_elevation());
}

#[test]
fn overview_propagates_lock_read_failures() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    let paths = paths_with_global(sandbox.path(), store);
    let service = GlobalStoreService::new(
        paths,
        std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel)),
    );
    let reader =
        || -> Result<Vec<Lock>, LockReadError> { Err(LockReadError::new("database is locked")) };

    let error = service.overview(&reader).unwrap_err();
    assert_eq!(error.message, "database is locked");
}

#[test]
fn write_refuses_a_read_only_store_without_elevating_or_staging() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);
    let elevator = std::sync::Arc::new(RecordingElevator::new(Behaviour::CreateDirectory));
    let service = GlobalStoreService::new(
        paths_with_global(sandbox.path(), store.clone()),
        elevator.clone(),
    );

    let error = service
        .write(WriteRequest::create(
            store.join("config.json"),
            b"{}".to_vec(),
        ))
        .unwrap_err();
    assert!(matches!(error, GlobalWriteError::ReadOnly { .. }));
    assert_eq!(elevator.calls(), 0, "writes never prompt for authorization");
    assert_eq!(fs::read_dir(&store).unwrap().count(), 0);
    set_mode(&store, 0o755);
}

#[test]
fn enable_cancel_keeps_the_store_read_only_and_writes_nothing() {
    if running_as_root() {
        eprintln!("skipping: root ignores directory permission bits");
        return;
    }
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    set_mode(&store, 0o555);
    let elevator = std::sync::Arc::new(RecordingElevator::new(Behaviour::Cancel));
    let service = GlobalStoreService::new(
        paths_with_global(sandbox.path(), store.clone()),
        elevator.clone(),
    );

    let error = service.enable_global_writes().unwrap_err();
    match error {
        EnableError::Cancelled { status } => {
            assert!(matches!(status, GlobalStoreStatus::ReadOnly { .. }));
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(elevator.calls(), 1);
    assert_eq!(fs::read_dir(&store).unwrap().count(), 0);
    assert!(matches!(
        service.status(),
        GlobalStoreStatus::ReadOnly { .. }
    ));
    set_mode(&store, 0o755);
}

#[test]
fn enable_success_flips_the_service_to_available() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    let elevator = std::sync::Arc::new(RecordingElevator::new(Behaviour::CreateDirectory));
    let service = GlobalStoreService::new(
        paths_with_global(sandbox.path(), store.clone()),
        elevator.clone(),
    );

    assert_eq!(
        service.enable_global_writes().unwrap(),
        EnableOutcome::Prepared
    );
    assert_eq!(elevator.calls(), 1);
    assert_eq!(service.status(), GlobalStoreStatus::Available);
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn a_writable_store_needs_no_elevation_for_the_same_write() {
    let sandbox = tempfile::tempdir().unwrap();
    let store = sandbox.path().join("global");
    fs::create_dir(&store).unwrap();
    let elevator = std::sync::Arc::new(RecordingElevator::new(Behaviour::CreateDirectory));
    let service = GlobalStoreService::new(
        paths_with_global(sandbox.path(), store.clone()),
        elevator.clone(),
    );

    service
        .write(WriteRequest::create(
            store.join("config.json"),
            b"{\"v\":1}".to_vec(),
        ))
        .unwrap();
    assert_eq!(
        elevator.calls(),
        0,
        "non-admin setups never require elevation"
    );
    assert_eq!(fs::read(store.join("config.json")).unwrap(), b"{\"v\":1}");
}
