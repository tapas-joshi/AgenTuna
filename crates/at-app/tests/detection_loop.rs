//! TA-081: the detection loop's observable behaviour.
//!
//! Watcher tests use throwaway, canonicalized trees and allow for platform batch boundaries.
//! The detection scheduler's debounce and coalescing are tested with an injected time instead
//! of assuming that the operating system reports a burst in one callback.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_app::app::detection::{
    DetectionResult, Diagnostic, PlatformWatcher, WatchStarter, external_refresh_event, rescan,
    start_retaining_watchable_roots,
};
use at_db::Database;
use at_fs::{
    RecentWrites, WatchBatch, WatchConfig, WatchError, WatchEvent, WatchRoot, WatchRootKind,
    WatchService,
};
use at_model::{Agent, AgentId};
use tempfile::TempDir;

/// Debounce used by these tests; one batch is expected shortly after the last write.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Bounded wait for one batch; comfortably above [`DEBOUNCE`].
const BATCH_BUDGET: Duration = Duration::from_secs(10);
/// Wall-clock window used to prove that no second event arrives.
const QUIET_WINDOW: Duration = Duration::from_millis(700);

/// Serializes the watcher-based tests.
///
/// Several FSEvents streams in one process compete for the same platform delivery path; running
/// these tests concurrently makes a burst occasionally deliver no batch at all. Each test still
/// builds its own tree and service, so only the platform interaction is serialized.
static WATCH_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn watch_test_lock() -> std::sync::MutexGuard<'static, ()> {
    WATCH_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One temporary directory, resolved once so reported and asserted paths match.
struct Fixture {
    _dir: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temporary directory");
        let root = fs::canonicalize(dir.path()).expect("canonical fixture root");
        Self { _dir: dir, root }
    }

    fn service(&self, roots: Vec<WatchRoot>, registry: Arc<RecentWrites>) -> WatchService {
        let service = WatchService::new(
            WatchConfig::new(roots)
                .with_debounce(DEBOUNCE)
                .with_registry(registry),
        );
        service.start().expect("watch service starts");
        service
    }

    fn root(&self) -> WatchRoot {
        WatchRoot::dir(&self.root, WatchRootKind::SkillsDir)
    }
}

/// Waits for one batch, skipping any event kinds that are not a batch.
fn next_batch(service: &WatchService) -> WatchBatch {
    let deadline = Instant::now() + BATCH_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(event) = service.next_event(remaining) else {
            panic!("no watcher batch within {BATCH_BUDGET:?}");
        };
        if let WatchEvent::Batch(batch) = event {
            return batch;
        }
    }
}

fn assert_quiet(service: &WatchService, what: &str) {
    if let Some(event) = service.next_event(QUIET_WINDOW) {
        panic!("{what}: unexpected follow-up event {event:?}");
    }
}

#[test]
fn external_edit_triggers_exactly_one_rescan_after_the_debounce_window() {
    let _platform = watch_test_lock();
    let fixture = Fixture::new();
    let target = fixture.root.join("settings.json");
    fs::write(&target, b"{\"servers\":{}}\n").expect("seed watched file");
    let service = fixture.service(vec![fixture.root()], Arc::new(RecentWrites::new()));

    fs::write(&target, b"{\"servers\":{\"demo\":{\"command\":\"x\"}}}\n").expect("external edit");
    let batch = next_batch(&service);
    assert!(
        batch.paths.iter().any(|path| path == &target),
        "the externally edited file is named in the batch: {:?}",
        batch.paths
    );

    let refresh = external_refresh_event(&WatchEvent::Batch(batch)).expect("typed refresh event");
    assert!(
        refresh.paths.iter().any(|path| Path::new(path) == target),
        "the Agents/Effective views refresh for the edited file: {refresh:?}"
    );
    assert!(
        !refresh.full_rescan,
        "a named batch does not need a full reload"
    );

    assert_quiet(&service, "one external edit must not queue a second rescan");
    assert_eq!(
        service.stats().batches,
        1,
        "exactly one rescan is queued after the debounce window: {:?}",
        service.stats()
    );
    service.stop().expect("stop the watch service");
}

#[test]
fn an_agentuna_self_write_produces_zero_events() {
    let _platform = watch_test_lock();
    let fixture = Fixture::new();
    let target = fixture.root.join("settings.json");
    fs::write(&target, b"{\"servers\":{}}\n").expect("seed watched file");
    let registry = Arc::new(RecentWrites::new());
    let service = fixture.service(vec![fixture.root()], Arc::clone(&registry));

    let written = b"{\"servers\":{\"demo\":{\"command\":\"y\"}}}\n";
    registry.record(&target, at_fs::sha256_hex(written));
    fs::write(&target, written).expect("AgenTuna's own write");

    assert!(
        service.next_event(QUIET_WINDOW).is_none(),
        "a write recorded in the shared registry must not surface as a drift event"
    );
    assert!(
        service.stats().suppressed >= 1,
        "the event was suppressed and counted, not lost: {:?}",
        service.stats()
    );
    assert_eq!(registry.len(), 1, "exactly one self-write is recorded");
    service.stop().expect("stop the watch service");
}

#[test]
fn twenty_writes_are_reported_across_bounded_watcher_batches() {
    let _platform = watch_test_lock();
    let fixture = Fixture::new();
    let service = fixture.service(vec![fixture.root()], Arc::new(RecentWrites::new()));

    let mut written = BTreeSet::new();
    for index in 0..20 {
        let path = fixture.root.join(format!("rule-{index:02}.md"));
        fs::write(&path, b"# rule\n").expect("burst write");
        written.insert(path);
    }

    // Per-path debounce timers and OS scheduling can split the writes across callbacks.
    // Collect every affected path instead of requiring one particular batch boundary.
    let deadline = Instant::now() + BATCH_BUDGET;
    let mut reported = BTreeSet::new();
    let mut batch_count = 0;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let wait = if reported == written {
            remaining.min(QUIET_WINDOW)
        } else {
            remaining
        };
        let Some(event) = service.next_event(wait) else {
            break;
        };
        match event {
            WatchEvent::Batch(batch) => {
                let unique: BTreeSet<_> = batch.paths.iter().collect();
                assert_eq!(
                    unique.len(),
                    batch.paths.len(),
                    "duplicate path in {batch:?}"
                );
                assert!(
                    batch.paths.len() <= at_fs::DEFAULT_MAX_BATCH_PATHS,
                    "the watcher batch must stay bounded: {batch:?}"
                );
                reported.extend(
                    batch
                        .paths
                        .into_iter()
                        .filter(|path| written.contains(path)),
                );
                batch_count += 1;
                assert!(service.queue_len() <= at_fs::DEFAULT_QUEUE_CAPACITY);
            }
            other => panic!("unexpected watcher event in the 20-file burst: {other:?}"),
        }
    }
    assert_eq!(reported, written, "every written path must be reported");
    assert!(
        batch_count > 0,
        "the watcher must report at least one batch"
    );
    assert_eq!(service.stats().batches, batch_count as u64);
    service.stop().expect("stop the watch service");
}

#[test]
fn an_unmanaged_agent_is_detected_and_shown_but_never_rescanned_into_deployments() {
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex root");
    fs::write(home.join(".codex/config.toml"), b"model = \"gpt-5\"\n").expect("codex config");
    fs::create_dir_all(home.join(".claude")).expect("claude root");
    fs::write(home.join(".claude/settings.json"), b"{}\n").expect("claude settings");

    let database = Database::open_file(fixture.root.join("index.db")).expect("index database");
    {
        let connection = database.connection();
        at_db::agents::upsert_agent(
            connection,
            &Agent {
                id: AgentId::Codex,
                display_name: String::from("Codex"),
                detected: true,
                version: None,
            },
        )
        .expect("existing agent row");
        at_db::agents::set_agent_managed(connection, &AgentId::Codex, false).expect("opt out");
    }

    let registry = Arc::new(AgentAdapterRegistry::register());
    let environment = AgentEnvironment {
        home: home.clone(),
        project: Some(fixture.root.join("project")),
        variables: BTreeMap::new(),
    };
    let database = Arc::new(Mutex::new(database));
    let pass = rescan(&registry, &environment, &database).expect("detection pass");

    assert!(
        pass.detected.contains(&AgentId::Codex),
        "an unmanaged agent is still detected and shown: {:?}",
        pass.detected
    );
    assert!(
        pass.results.contains(&DetectionResult::SkippedUnmanaged {
            agent: AgentId::Codex
        }),
        "the pass records SkippedUnmanaged for the opted-out agent: {:?}",
        pass.results
    );
    assert!(
        pass.results.contains(&DetectionResult::Refreshed {
            agent: AgentId::ClaudeCode
        }),
        "a managed agent is refreshed in the same pass: {:?}",
        pass.results
    );
    assert_eq!(pass.skipped_unmanaged(), vec![String::from("codex")]);

    let database = database.lock().expect("database lock");
    let rows = at_db::agents::list_agents_on_mac(database.connection()).expect("agent rows");
    let codex = rows
        .iter()
        .find(|row| row.id == AgentId::Codex)
        .expect("the agent row stays visible");
    assert!(
        !codex.managed,
        "the user's opt-out survives the detection pass"
    );
    assert!(
        codex.display_name.is_some(),
        "the agent stays listed on the Agents screen: {codex:?}"
    );
    let deployments: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM deployments", [], |row| row.get(0))
        .expect("deployment count");
    assert_eq!(
        deployments, 0,
        "no deployment is rescanned for either agent"
    );
}

/// Refuses one named root and starts the real platform watcher for everything else.
struct RefusingStarter {
    refused: PathBuf,
    registry: Arc<RecentWrites>,
}

impl WatchStarter for RefusingStarter {
    fn start(&mut self, roots: Vec<WatchRoot>) -> Result<WatchService, WatchError> {
        if roots.iter().any(|root| root.path == self.refused) {
            return Err(WatchError::WatchFailed {
                path: self.refused.clone(),
                source: notify::Error::generic("operation not permitted"),
            });
        }
        PlatformWatcher::new(Arc::clone(&self.registry)).start(roots)
    }
}

#[test]
fn an_unwatchable_root_is_reported_and_remaining_roots_keep_working() {
    let _platform = watch_test_lock();
    let fixture = Fixture::new();
    let refused = fixture.root.join("refused");
    let healthy = fixture.root.join("healthy");
    fs::create_dir_all(&refused).expect("refused root");
    fs::create_dir_all(&healthy).expect("healthy root");

    let mut starter = RefusingStarter {
        refused: refused.clone(),
        registry: Arc::new(RecentWrites::new()),
    };
    let roots = vec![
        WatchRoot::dir(&refused, WatchRootKind::SkillsDir),
        WatchRoot::dir(&healthy, WatchRootKind::SkillsDir),
    ];
    let (service, diagnostics) = start_retaining_watchable_roots(roots, &mut starter)
        .expect("the healthy root still starts");

    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        matches!(
            &diagnostics[0],
            Diagnostic::WatchFailed { path, os_error }
                if Path::new(path) == refused && os_error == "operation not permitted"
        ),
        "the refusal is emitted as Diagnostic::WatchFailed: {diagnostics:?}"
    );
    let watched: Vec<PathBuf> = service
        .roots()
        .iter()
        .map(|root| root.path.clone())
        .collect();
    assert_eq!(
        watched,
        vec![healthy.clone()],
        "the refused root is dropped from the watch set"
    );

    let edited = healthy.join("new.md");
    fs::write(&edited, b"# new rule\n").expect("edit the healthy root");
    let batch = next_batch(&service);
    assert!(
        batch.paths.iter().any(|path| path == &edited),
        "the remaining root keeps delivering events: {:?}",
        batch.paths
    );
    service.stop().expect("stop the watch service");
}
