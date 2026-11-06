//! Behaviour tests for the startup gate, the activation channel and record cleanup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use at_store::instance::InstanceLock;
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use tempfile::TempDir;

use super::*;

fn paths_for(temp: &Path) -> AppPaths {
    let environment = PathEnvironment::default()
        .with_home(temp.join("home"))
        .with_overrides(PathOverrides {
            home: Some(temp.join("support")),
            ..PathOverrides::default()
        });
    AppPaths::resolve(&environment).expect("paths")
}

fn socket_path(paths: &AppPaths) -> PathBuf {
    InstanceLock::new(paths.support_dir()).socket_path()
}

#[test]
fn the_first_launch_becomes_primary_and_leaves_a_record_until_shutdown() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());

    let primary = match begin(&paths) {
        Startup::Primary(primary) => primary,
        other => panic!("expected Primary, got {other:?}"),
    };
    let record = primary.record().expect("record written").clone();
    assert_eq!(record.pid, std::process::id());

    let state = primary.into_state();
    assert!(paths.support_dir().join("instance.lock").exists());
    state.shutdown();
    assert!(!paths.support_dir().join("instance.lock").exists());
    assert_eq!(
        InstanceLock::new(paths.support_dir()).read_record(),
        at_store::instance::RecordState::Missing
    );
}

#[test]
fn a_second_launch_is_already_running_and_activation_focuses_the_first_within_500ms() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());

    let primary = match begin(&paths) {
        Startup::Primary(primary) => primary,
        other => panic!("expected Primary, got {other:?}"),
    };
    let state = Arc::new(primary.into_state());
    let (sender, receiver) = mpsc::channel();
    state
        .start_activation(move || {
            let _ = sender.send(());
        })
        .expect("activation listener");

    let started = Instant::now();
    match begin(&paths) {
        Startup::AlreadyRunning {
            holder: Some(holder),
        } => assert_eq!(holder.pid, std::process::id()),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    assert!(activate_existing(&paths), "activation message delivered");
    receiver
        .recv_timeout(Duration::from_millis(500))
        .expect("the first instance is activated within 500 ms");
    assert!(started.elapsed() < Duration::from_millis(5_000));

    state.shutdown();
    assert!(
        !activate_existing(&paths),
        "the socket is gone after shutdown"
    );
}

#[test]
fn activation_without_a_running_instance_delivers_nothing() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    assert!(!activate_existing(&paths));
}

#[test]
fn a_stale_socket_file_is_replaced_by_the_listener() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let primary = match begin(&paths) {
        Startup::Primary(primary) => primary,
        other => panic!("expected Primary, got {other:?}"),
    };
    let state = primary.into_state();
    let stale = socket_path(&paths);
    std::fs::write(&stale, b"left behind by a dead process").expect("stale socket file");

    state
        .start_activation(|| {})
        .expect("stale socket replaced");
    assert!(stale.exists());
    assert!(activate_existing(&paths));

    state.shutdown();
    assert!(!stale.exists());
}

#[test]
fn starting_the_listener_twice_keeps_one_listener() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let primary = match begin(&paths) {
        Startup::Primary(primary) => primary,
        other => panic!("expected Primary, got {other:?}"),
    };
    let state = primary.into_state();
    state.start_activation(|| {}).expect("first");
    state.start_activation(|| {}).expect("second is a no-op");
    assert!(activate_existing(&paths));
    state.shutdown();
}

#[test]
fn activating_a_second_process_never_removes_the_first_record() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let primary = match begin(&paths) {
        Startup::Primary(primary) => primary,
        other => panic!("expected Primary, got {other:?}"),
    };
    let record = primary.record().expect("record").clone();
    let state = primary.into_state();

    // A second launch that decides to exit must not touch the record.
    match begin(&paths) {
        Startup::AlreadyRunning { .. } => {}
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    assert_eq!(
        InstanceLock::new(paths.support_dir()).read_record(),
        at_store::instance::RecordState::Valid(record)
    );
    state.shutdown();
}
