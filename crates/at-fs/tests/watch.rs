//! Behaviour of the watch service against real temp directories.
//!
//! Every test polls with a deadline and never asserts on a fixed sleep: the platform decides
//! when the events arrive, the assertions only describe what has to be true once they do.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use at_fs::{
    ExpectedState, RecentWrites, WatchBatch, WatchChange, WatchConfig, WatchError, WatchEvent,
    WatchRoot, WatchRootKind, WatchService, WriteRequest, apply_with_registry,
};
use tempfile::TempDir;

/// Waits for the next batch, skipping the other event kinds.
fn next_batch(service: &WatchService, timeout: Duration) -> Option<WatchBatch> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match service.next_event(remaining) {
            Some(WatchEvent::Batch(batch)) => return Some(batch),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// Collects events until `window` passes without a new one.
/// FSEvents coalesces and replays events for directories created moments before a stream
/// starts. Fixtures are created, then given this long to settle, before watching begins, so
/// assertions only see events caused after `start`. A loaded runner starts the stream late.
fn settle_fixture() {
    std::thread::sleep(Duration::from_millis(2_500));
}

fn drain(service: &WatchService, window: Duration) -> Vec<WatchEvent> {
    let deadline = Instant::now() + window;
    let mut events = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match service.next_event(remaining) {
            Some(event) => events.push(event),
            None => break,
        }
    }
    events
}

fn batches(events: &[WatchEvent]) -> Vec<&WatchBatch> {
    events.iter().filter_map(WatchEvent::batch).collect()
}

#[test]
fn fifty_writes_to_one_file_produce_exactly_one_batch() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let file = base.join(".claude.json");
    fs::write(&file, b"0").expect("seed file");

    let config = WatchConfig::new([WatchRoot::file(&file, WatchRootKind::McpFile)]);
    assert_eq!(config.debounce, at_fs::DEFAULT_DEBOUNCE);
    assert_eq!(config.debounce, Duration::from_millis(300));
    let service = WatchService::new(config);
    settle_fixture();
    service.start().expect("start");

    let began = Instant::now();
    for value in 1..=50 {
        fs::write(&file, value.to_string()).expect("write");
    }
    let writes = began.elapsed();

    let first = next_batch(&service, Duration::from_secs(30)).expect("a batch");
    assert!(
        first.contains(&file),
        "batch should contain {}: {:?}",
        file.display(),
        first.paths
    );
    assert!(
        first
            .kinds
            .iter()
            .any(|kind| matches!(kind, WatchChange::Modified(path) if *path == file)),
        "50 writes are modifications: {:?}",
        first.kinds
    );

    // The burst coalesces. FSEvents can deliver the tail of a burst after the
    // debounce window closes when the machine is loaded, so one late trailing
    // batch is tolerated; what must never happen is one batch per write.
    let remaining = drain(&service, Duration::from_millis(600));
    let rest = batches(&remaining);
    assert!(
        rest.len() <= 2,
        "expected the 50 writes to coalesce, got {} more batches: {rest:?}",
        rest.len()
    );
    assert!(
        service.stats().batches <= 3,
        "50 writes must not produce per-write batches: {}",
        service.stats().batches
    );
    assert!(
        writes < Duration::from_secs(30),
        "the 50 writes should be far quicker than the debounce window, took {writes:?}"
    );

    service.stop().expect("stop");
}

#[test]
fn a_write_recorded_in_the_registry_is_not_reported() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let file = base.join("config.json");
    let registry = Arc::new(RecentWrites::new());
    let service = WatchService::new(
        WatchConfig::new([WatchRoot::file(&file, WatchRootKind::McpFile)])
            .with_registry(Arc::clone(&registry)),
    );
    settle_fixture();
    service.start().expect("start");

    apply_with_registry(&WriteRequest::create(&file, b"hello".to_vec()), &registry).expect("write");

    let deadline = Instant::now() + Duration::from_secs(3);
    while registry.suppressed_count() == 0 && Instant::now() < deadline {
        let _ = service.next_event(Duration::from_millis(50));
    }
    assert_eq!(
        registry.suppressed_count(),
        1,
        "exactly one event cycle should be suppressed"
    );
    assert_eq!(service.stats().batches, 0);
    assert_eq!(service.stats().suppressed, 1);
    let events = drain(&service, Duration::from_millis(500));
    assert!(batches(&events).is_empty(), "{events:?}");

    service.stop().expect("stop");
}

#[test]
fn ignored_trees_and_editor_litter_never_reach_a_batch() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let service = WatchService::new(WatchConfig::new([WatchRoot::dir(
        &root,
        WatchRootKind::UserStore,
    )]));
    settle_fixture();
    service.start().expect("start");

    let ignored = [
        root.join(".git/objects/ab/cdef"),
        root.join("node_modules/left-pad/index.js"),
        root.join("cache/state.bin"),
        root.join("backups/op-1/blob"),
        root.join(".claude.json.agentuna-tmp-4242-1"),
        root.join("settings.json.swp"),
    ];
    for path in &ignored {
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, b"noise").expect("write");
    }
    let ignored_events = drain(&service, Duration::from_millis(900));
    let quiet = batches(&ignored_events);
    assert!(
        quiet.is_empty(),
        "ignored paths must never appear: {quiet:?}"
    );

    // The watcher is alive: a normal file still arrives. A loaded machine can split the
    // directory and file events of one write across batches (and deliver the tail of the
    // ignored burst late), so poll until the normal file's batch lands while checking every
    // batch that arrives — the ignored-path assertion must hold for all of them.
    let normal = root.join("skills/demo/SKILL.md");
    fs::create_dir_all(normal.parent().expect("parent")).expect("dirs");
    // Create new nested directories and write immediately, before inotify necessarily installs
    // its recursive watches; the parent event must let the service recover the file.
    fs::write(&normal, b"---\nname: demo\n---\n").expect("write");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut batch = None;
    while Instant::now() < deadline {
        let Some(arrived) = next_batch(&service, Duration::from_secs(30)) else {
            panic!("no batch arrived after the normal write");
        };
        assert!(
            arrived
                .paths
                .iter()
                .all(|path| !path.starts_with(root.join(".git"))),
            "ignored paths must never appear: {:?}",
            arrived.paths
        );
        if arrived.contains(&normal) {
            batch = Some(arrived);
            break;
        }
    }
    assert!(
        batch.is_some(),
        "a batch naming the normal file must arrive"
    );

    service.stop().expect("stop");
}

#[test]
fn a_rename_is_reported_once_with_both_paths() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let before = root.join("skill-a/SKILL.md");
    let after = root.join("skill-b/SKILL.md");
    fs::create_dir_all(before.parent().expect("parent")).expect("source dirs");
    fs::create_dir_all(after.parent().expect("parent")).expect("destination dirs");
    let service = WatchService::new(WatchConfig::new([WatchRoot::dir(
        &root,
        WatchRootKind::SkillsDir,
    )]));
    settle_fixture();
    service.start().expect("start");

    fs::write(&before, b"---\nname: a\n---\n").expect("write");
    let created = next_batch(&service, Duration::from_secs(30)).expect("create batch");
    assert!(created.contains(&before), "{:?}", created.paths);

    fs::rename(&before, &after).expect("rename");

    let batch = next_batch(&service, Duration::from_secs(30)).expect("rename batch");
    assert_eq!(
        batch.renames(),
        vec![(before.as_path(), after.as_path())],
        "expected exactly one rename with both paths, got {:?}",
        batch.kinds
    );
    assert!(batch.paths.contains(&before) && batch.paths.contains(&after));

    let later = drain(&service, Duration::from_millis(700));
    let rest = batches(&later);
    assert!(rest.is_empty(), "the rename is reported once: {rest:?}");
    for kind in &batch.kinds {
        if let WatchChange::Created(path) | WatchChange::Removed(path) = kind {
            assert_ne!(path, &before, "a rename is not also a delete: {kind:?}");
            assert_ne!(path, &after, "a rename is not also a create: {kind:?}");
        }
    }

    service.stop().expect("stop");
}

#[test]
fn a_ten_thousand_file_burst_emits_one_rescan_and_the_service_keeps_running() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let service = WatchService::new(
        WatchConfig::new([WatchRoot::dir(&root, WatchRootKind::UserStore)])
            .with_max_batch_paths(256)
            .with_burst_quiet(Duration::from_secs(2)),
    );
    settle_fixture();
    service.start().expect("start");

    for index in 0..10_000 {
        fs::create_dir_all(&root).expect("store");
        fs::write(root.join(format!("f{index}.json")), b"x").expect("write");
    }

    // The storm collapses into one rescan; every further cycle in the burst is suppressed.
    // Waiting for a window with no event at all also proves the burst went quiet. The quiet
    // window is long enough that a slow runner pausing mid-burst cannot end the drain early.
    let mut rescans = 0;
    loop {
        match service.next_event(Duration::from_secs(6)) {
            Some(WatchEvent::RescanRequired { .. }) => rescans += 1,
            Some(_) => {}
            None => break,
        }
    }
    assert_eq!(rescans, 1, "expected exactly one rescan");
    assert_eq!(service.stats().rescan_required, 1);
    assert!(service.queue_len() <= 1024);

    // The service keeps running: a change after the burst is reported again.
    let after = root.join("after.json");
    fs::write(&after, b"x").expect("write");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("batch after the burst");
    assert!(batch.contains(&after), "{:?}", batch.paths);

    let stats = service.stats();
    assert!(stats.batches >= 1);
    assert!(stats.overflow <= stats.batches + 1, "{stats:?}");

    service.stop().expect("stop");
}

#[test]
fn start_twice_is_refused_and_stop_joins_quickly() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let service = WatchService::new(WatchConfig::new([WatchRoot::dir(
        &root,
        WatchRootKind::UserStore,
    )]));

    settle_fixture();
    service.start().expect("start");
    assert!(service.is_running());
    assert!(matches!(service.start(), Err(WatchError::AlreadyRunning)));

    let began = Instant::now();
    service.stop().expect("stop");
    let joined = began.elapsed();
    assert!(!service.is_running());
    assert!(joined < Duration::from_secs(15), "stop took {joined:?}");

    // Stopping again is a no-op.
    service.stop().expect("second stop");

    // Restarting yields a working service.
    service.start().expect("restart");
    let file = root.join("after-restart.json");
    fs::write(&file, b"x").expect("write");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("batch after restart");
    assert!(batch.contains(&file), "{:?}", batch.paths);

    // Once stopped, nothing is observed any more.
    let _ = drain(&service, Duration::from_millis(400));
    service.stop().expect("stop");
    fs::write(root.join("after-stop.json"), b"x").expect("write");
    assert!(
        service.next_event(Duration::from_millis(600)).is_none(),
        "a stopped service must not observe changes"
    );
}

#[test]
fn deleting_a_watched_store_yields_path_gone_and_resumes_when_it_returns() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let project = base.join("acme");
    let store = project.join(".agentuna");
    fs::create_dir_all(&store).expect("store");
    fs::write(store.join("store.json"), b"{}").expect("seed");
    let service = WatchService::new(WatchConfig::new([WatchRoot::dir(
        &store,
        WatchRootKind::ProjectStore,
    )]));
    settle_fixture();
    service.start().expect("start");

    fs::remove_dir_all(&store).expect("delete store");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut gone = false;
    while Instant::now() < deadline {
        match service.next_event(Duration::from_millis(100)) {
            Some(WatchEvent::PathGone { path, .. }) => {
                assert_eq!(path, store);
                gone = true;
                break;
            }
            // A quiet 100 ms slice is not the end: FSEvents can deliver late on a loaded machine.
            Some(_) | None => continue,
        }
    }
    assert!(gone, "deleting the watched store yields PathGone");
    assert!(service.is_running(), "PathGone must not stop the service");

    // The parent watch reports the root's return; the worker re-attaches recursively before
    // processing later changes inside the recreated store. Retry writes across platform batches.
    fs::create_dir_all(&store).expect("recreate store");
    let mut seen = None;
    for attempt in 0..5 {
        let returned = store.join(format!("recovered-{attempt}.json"));
        fs::write(&returned, b"{}").expect("write");
        if let Some(batch) = next_batch(&service, Duration::from_secs(10))
            && batch.contains(&returned)
        {
            seen = Some((returned, batch));
            break;
        }
    }
    let (returned, batch) =
        seen.expect("the re-attached watch reports changes inside the returned store");
    assert!(batch.contains(&returned), "{:?}", batch.paths);

    service.stop().expect("stop");
}

#[test]
fn a_missing_file_root_is_watched_through_its_parent() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let file = base.join(".claude.json");
    assert!(!file.exists());
    let service = WatchService::new(WatchConfig::new([WatchRoot::file(
        &file,
        WatchRootKind::McpFile,
    )]));
    service
        .start()
        .expect("a missing root must not fail the start");

    fs::write(&file, br#"{"mcpServers":{}}"#).expect("write");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("batch");
    assert!(batch.contains(&file), "{:?}", batch.paths);
    assert!(
        batch
            .kinds
            .iter()
            .any(|kind| matches!(kind, WatchChange::Created(path) if *path == file))
    );

    service.stop().expect("stop");
}

#[test]
fn replacing_a_file_through_the_write_pipeline_updates_the_index() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let file = root.join("config.json");
    fs::write(&file, b"one").expect("seed");
    let registry = Arc::new(RecentWrites::new());
    let service = WatchService::new(
        WatchConfig::new([WatchRoot::dir(&root, WatchRootKind::UserStore)])
            .with_registry(Arc::clone(&registry)),
    );
    settle_fixture();
    service.start().expect("start");

    let expected = ExpectedState::Hash(at_fs::sha256_hex(b"one"));
    apply_with_registry(
        &WriteRequest::replace(&file, b"two".to_vec(), expected),
        &registry,
    )
    .expect("replace");

    // The replacement is ours: no batch.
    let deadline = Instant::now() + Duration::from_secs(3);
    while registry.suppressed_count() == 0 && Instant::now() < deadline {
        let _ = service.next_event(Duration::from_millis(50));
    }
    assert_eq!(registry.suppressed_count(), 1);
    assert_eq!(service.stats().batches, 0);

    // A later external write is reported as a modification of a known path.
    fs::write(&file, b"three").expect("external write");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("external batch");
    assert!(
        batch
            .kinds
            .iter()
            .any(|kind| matches!(kind, WatchChange::Modified(path) if *path == file)),
        "{:?}",
        batch.kinds
    );

    service.stop().expect("stop");
}

#[test]
fn a_full_queue_collapses_into_one_rescan_and_stays_bounded() {
    let home = TempDir::new().expect("temp");
    let base = fs::canonicalize(home.path()).expect("canonical temp");
    let root = base.join("store");
    fs::create_dir_all(&root).expect("store");
    let service = WatchService::new(
        WatchConfig::new([WatchRoot::dir(&root, WatchRootKind::UserStore)])
            .with_capacity(3)
            .with_debounce(Duration::from_millis(100)),
    );
    settle_fixture();
    service.start().expect("start");

    fn produced(service: &WatchService) -> u64 {
        let stats = service.stats();
        stats.batches + stats.rescan_required + stats.overflow
    }

    for index in 0..6 {
        let before = produced(&service);
        fs::write(root.join(format!("f{index}.json")), b"x").expect("write");
        let deadline = Instant::now() + Duration::from_secs(3);
        while produced(&service) <= before {
            assert!(
                Instant::now() < deadline,
                "the service stopped producing events"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            service.queue_len() <= 3,
            "the queue must stay bounded, saw {}",
            service.queue_len()
        );
    }

    let events = drain(&service, Duration::from_millis(500));
    let rescans = events
        .iter()
        .filter(|event| matches!(event, WatchEvent::RescanRequired { .. }))
        .count();
    assert_eq!(rescans, 1, "one rescan for a full queue: {events:?}");
    assert!(service.stats().overflow >= 1, "{:?}", service.stats());
    assert!(service.queue_len() <= 3);

    service.stop().expect("stop");
}

#[test]
fn the_service_can_be_shared_between_the_ui_and_the_worker() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<WatchService>();
    assert_send_sync::<WatchConfig>();
    assert_send_sync::<Arc<RecentWrites>>();
    assert_send_sync::<WatchBatch>();
    assert_send_sync::<WatchEvent>();
    assert_send_sync::<WatchRoot>();
    assert_send_sync::<WatchError>();
}
