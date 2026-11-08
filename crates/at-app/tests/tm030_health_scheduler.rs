//! TM-030: scheduler tests against a process-level fixture server driven by a control file.
//!
//! The fixture (`tests/support/health_fixture_server.rs`, built as
//! `at-app-health-fixture` with the `testkit` feature) reads `pass`, `fail`, `hang` or
//! `partial` from its control file for every request, so one binary models every server
//! behaviour the scheduler must survive. These tests drive the *production* runner
//! (`McpHealthRunner`) and assert the durable rows and the process tree, not just the
//! in-memory due list.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use at_app::app::jobs::{
    Clock, ExecutionContext, HEALTH_CHECK_DEADLINE, HealthScheduler, HealthTarget, HealthTransport,
    Jitter, JobsControl, McpHealthRunner, SchedulePolicy,
};
use at_app::app::network::NetworkState;
use at_db::Database;
use at_db::health::HealthStatus;
use at_db::settings::{BackgroundHealth, HealthInterval};
use at_model::{McpServer, McpServerDef, McpServerId, Scope};
use tempfile::TempDir;

/// The fixture binary Cargo built for this test run.
fn fixture() -> PathBuf {
    option_env!("CARGO_BIN_EXE_at-app-health-fixture")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("run with the at-app `testkit` feature so the fixture builds"))
}

struct FixedClock(i64);
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct ZeroJitter;
impl Jitter for ZeroJitter {
    fn delay_ms(&self, _max_ms: u64) -> u64 {
        0
    }
}

/// Writes `mode` into a control file and returns a target whose server is the fixture.
///
/// The fixture is a test binary: the command is an absolute path, which `StdioLaunch`
/// accepts unchanged, and every argument is a temp path — nothing touches real agent
/// configuration.
fn fixture_target(dir: &Path, id: &str, mode: &str) -> (HealthTarget, PathBuf) {
    let control = dir.join(format!("{id}.control"));
    let report = dir.join(format!("{id}.report"));
    fs::write(&control, mode).expect("control file");
    let transport = at_model::McpTransport::Stdio {
        command: fixture().display().to_string(),
        args: vec![
            "--control".to_owned(),
            control.display().to_string(),
            "--report".to_owned(),
            report.display().to_string(),
            "--child-sleep".to_owned(),
        ],
        cwd: None,
        env: Default::default(),
    };
    let server = McpServer::from_def(
        McpServerId::new(),
        Scope::User,
        McpServerDef {
            display_name: format!("fixture-{id}"),
            description: None,
            transport: transport.clone(),
            extra: Default::default(),
        },
    );
    let item_id = format!("itm_fixture_{id}");
    let target = HealthTarget {
        id: item_id.clone(),
        name: format!("fixture-{id}"),
        item_id: Some(item_id),
        scope: Scope::User,
        project_id: None,
        agent_id: None,
        transport: HealthTransport::Stdio,
        host: None,
        manually_tested: true,
        server,
    };
    (target, report)
}

/// Parameters shared by every check in these tests.
struct Harness {
    database: Mutex<Database>,
    network: NetworkState,
    runner: McpHealthRunner,
    jitter: ZeroJitter,
    cancellation: tokio_util::sync::CancellationToken,
    clock: FixedClock,
}

impl Harness {
    fn new() -> Self {
        let network = NetworkState::default();
        network.set_background_health(at_model::BackgroundHealth::All);
        Self {
            database: Mutex::new(Database::open_in_memory().expect("in-memory database")),
            network,
            runner: McpHealthRunner::default(),
            jitter: ZeroJitter,
            cancellation: tokio_util::sync::CancellationToken::new(),
            clock: FixedClock(300_000),
        }
    }

    fn history(&self, item_id: &str) -> Vec<at_db::health::HealthCheck> {
        let database = self.database.lock().expect("database lock");
        at_db::health::history(database.connection(), item_id, None, None).expect("health history")
    }
}

fn policy() -> SchedulePolicy {
    SchedulePolicy {
        interval: HealthInterval::FiveMinutes,
        background: BackgroundHealth::All,
        online: true,
    }
}

/// Schedules `target` at t=0 and returns its first due check at t=300 s.
fn due_now(scheduler: &mut HealthScheduler, target: &HealthTarget) -> at_app::app::jobs::DueCheck {
    let jitter = ZeroJitter;
    scheduler.due_checks(0, policy(), std::slice::from_ref(target), &jitter);
    scheduler
        .due_checks(300_000, policy(), std::slice::from_ref(target), &jitter)
        .into_iter()
        .next()
        .expect("fixture target is due after the base interval")
}

/// Reads `pid=` / `child=` from a fixture report.
fn read_report(path: &Path) -> (i32, Option<i32>) {
    let text = fs::read_to_string(path).expect("fixture report exists");
    let mut pid = None;
    let mut child = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("child=") {
            child = value.parse().ok();
        }
    }
    (pid.expect("fixture wrote its pid"), child)
}

fn alive(pid: i32) -> bool {
    if rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).expect("pid"))
        .is_err()
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // Linux reports an unreaped orphan zombie as signalable, although it cannot execute.
        // The minimal container init may leave such children until the test process exits.
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            return stat
                .rsplit_once(')')
                .and_then(|(_, fields)| fields.split_whitespace().next())
                != Some("Z");
        }
    }
    true
}

/// Polls until `pid` is gone; panics when it outlives the bound.
async fn wait_until_gone(pid: i32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{what} process {pid} is still alive");
}

/// Waits for the fixture to write its report (it is alive by then).
async fn wait_for_report(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fixture never wrote its report at {}", path.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_control_file_switches_the_fixture_between_pass_fail_and_partial() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new();
    let mut scheduler = HealthScheduler::new();
    for (mode, expectation) in [
        ("pass", None),
        ("fail", Some("stdio_check_failed")),
        ("partial", Some("stdio_check_failed")),
    ] {
        let (target, _report) = fixture_target(dir.path(), mode, mode);
        let due = due_now(&mut scheduler, &target);
        let outcome = scheduler
            .execute(
                due,
                &ExecutionContext {
                    clock: &harness.clock,
                    interval: HealthInterval::FiveMinutes,
                    runner: &harness.runner,
                    network: &harness.network,
                    database: &harness.database,
                    cancellation: &harness.cancellation,
                    deadline: Duration::from_secs(10),
                    jitter: &harness.jitter,
                    event_sink: None,
                },
            )
            .await;
        outcome.expect("fixture check is recorded");
        assert!(
            !harness.cancellation.is_cancelled(),
            "a finished check must never cancel the caller's token"
        );

        let item = target.item_id.clone().expect("managed item");
        let history = harness.history(&item);
        let latest = history.first().expect("one row per executed check");
        match expectation {
            None => {
                assert_eq!(latest.status, HealthStatus::Ok, "pass mode: {latest:?}");
                assert_eq!(latest.tools_count, Some(1), "one fixture tool listed");
            }
            Some(code) => {
                assert_eq!(
                    latest.status,
                    HealthStatus::Failed,
                    "{mode} mode: {latest:?}"
                );
                assert_eq!(
                    latest.error_code.as_deref(),
                    Some(code),
                    "{mode} mode: {latest:?}"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_fixture_is_cancelled_at_the_deadline_and_the_next_server_still_runs() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new();
    let mut scheduler = HealthScheduler::new();
    let (hung, hung_report) = fixture_target(dir.path(), "hang", "hang");
    let (healthy, _healthy_report) = fixture_target(dir.path(), "next", "pass");

    let due = due_now(&mut scheduler, &hung);
    let started = Instant::now();
    scheduler
        .execute(
            due,
            &ExecutionContext {
                clock: &harness.clock,
                interval: HealthInterval::FiveMinutes,
                runner: &harness.runner,
                network: &harness.network,
                database: &harness.database,
                cancellation: &harness.cancellation,
                deadline: Duration::from_millis(800),
                jitter: &harness.jitter,
                event_sink: None,
            },
        )
        .await
        .expect("the deadline failure is recorded, not propagated");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(800),
        "the hang fixture must be cancelled at the per-check deadline, took {elapsed:?}"
    );

    let history = harness.history(hung.item_id.as_deref().expect("managed item"));
    let latest = history.first().expect("timeout row");
    assert_eq!(latest.status, HealthStatus::Failed, "{latest:?}");
    assert_eq!(
        latest.error_code.as_deref(),
        Some("check_timeout"),
        "{latest:?}"
    );

    // The hung process tree is gone once the deadline drops the session.
    let (pid, child) = read_report(&hung_report);
    wait_until_gone(pid, "hung fixture").await;
    if let Some(child) = child {
        wait_until_gone(child, "hung fixture child").await;
    }

    // A hung check never blocks the next server: the healthy target still records.
    let due = due_now(&mut scheduler, &healthy);
    scheduler
        .execute(
            due,
            &ExecutionContext {
                clock: &harness.clock,
                interval: HealthInterval::FiveMinutes,
                runner: &harness.runner,
                network: &harness.network,
                database: &harness.database,
                cancellation: &harness.cancellation,
                deadline: Duration::from_secs(10),
                jitter: &harness.jitter,
                event_sink: None,
            },
        )
        .await
        .expect("next server runs after the timeout");
    let history = harness.history(healthy.item_id.as_deref().expect("managed item"));
    assert_eq!(history.first().expect("ok row").status, HealthStatus::Ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_joins_the_running_check_within_five_seconds_and_leaves_no_fixture_process() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new();
    let control = JobsControl::default();
    let mut scheduler = HealthScheduler::new();
    let (target, report) = fixture_target(dir.path(), "quit", "hang");

    let due = due_now(&mut scheduler, &target);
    let cancellation = control.cancellation_token();
    let context = ExecutionContext {
        clock: &harness.clock,
        interval: HealthInterval::FiveMinutes,
        runner: &harness.runner,
        network: &harness.network,
        database: &harness.database,
        cancellation: &cancellation,
        deadline: HEALTH_CHECK_DEADLINE,
        jitter: &harness.jitter,
        event_sink: None,
    };
    // A check far longer than the test: only quit can end it.
    let execute = scheduler.execute(due, &context);
    tokio::pin!(execute);
    tokio::select! {
        result = &mut execute => panic!("the hung check must still be running: {result:?}"),
        () = wait_for_report(&report) => {}
    }

    let started = Instant::now();
    control.shutdown();
    let joined = tokio::time::timeout(Duration::from_secs(5), execute).await;
    assert!(
        joined.is_ok(),
        "quit must join every health task within 5 s (waited {:?})",
        started.elapsed()
    );
    joined
        .expect("joined in time")
        .expect("cancelled check ends without an error");

    let (pid, child) = read_report(&report);
    wait_until_gone(pid, "fixture").await;
    if let Some(child) = child {
        wait_until_gone(child, "fixture child").await;
    }
}
