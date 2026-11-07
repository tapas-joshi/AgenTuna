//! Sandboxed measurement of database/config readiness and the idle headless loop.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use at_db::Database;
use at_store::paths::{AppPaths, HomeSource, PathEnvironment, PathOverrides};
use rusqlite::params;

fn sandbox_paths(root: &Path) -> AppPaths {
    AppPaths::resolve(&PathEnvironment {
        home: HomeSource::Injected(root.join("home")),
        overrides: PathOverrides {
            global_store: Some(root.join("global")),
            ..PathOverrides::default()
        },
        project_root: None,
    })
    .expect("sandbox paths")
}

fn process_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[test]
fn reports_headless_startup_memory_and_idle_cpu_budgets() {
    let root = tempfile::tempdir().expect("temporary home");
    let paths = sandbox_paths(root.path());
    paths.create_dirs().expect("create sandbox directories");
    let startup = Instant::now();
    let db = Database::open_file(paths.db_path()).expect("open and migrate fixture database");
    let config = at_app::app::config::AppConfig::load(&paths, Vec::new());
    assert!(!config.is_degraded(), "sandbox config should load defaults");
    let ready_ms = startup.elapsed().as_millis();

    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS perf_items(id INTEGER PRIMARY KEY, payload TEXT NOT NULL);\
         CREATE TABLE IF NOT EXISTS perf_deployments(id INTEGER PRIMARY KEY, item_id INTEGER NOT NULL);\
         CREATE TABLE IF NOT EXISTS perf_operations(id INTEGER PRIMARY KEY, payload TEXT NOT NULL);",
    ).expect("create deterministic workload tables");
    let tx = db
        .connection()
        .unchecked_transaction()
        .expect("start fixture transaction");
    {
        let mut items = tx
            .prepare_cached("INSERT INTO perf_items(payload) VALUES (?1)")
            .expect("prepare items");
        for index in 0..3_000 {
            items
                .execute(params![format!("item-{index:04}-{}", "x".repeat(64))])
                .expect("seed item");
        }
    }
    {
        let mut rows = tx
            .prepare_cached("INSERT INTO perf_deployments(item_id) VALUES (?1)")
            .expect("prepare deployments");
        for index in 0..9_000 {
            rows.execute(params![index % 3_000])
                .expect("seed deployment");
        }
    }
    {
        let mut rows = tx
            .prepare_cached("INSERT INTO perf_operations(payload) VALUES (?1)")
            .expect("prepare operations");
        for index in 0..10_000 {
            rows.execute(params![format!("operation-{index:05}")])
                .expect("seed operation");
        }
    }
    tx.commit().expect("commit workload");
    let workload_count: i64 = db.connection().query_row(
        "SELECT (SELECT COUNT(*) FROM perf_items) + (SELECT COUNT(*) FROM perf_deployments) + (SELECT COUNT(*) FROM perf_operations)", [], |row| row.get(0),
    ).expect("verify fixture workload");
    assert_eq!(workload_count, 22_000);
    let rss_kib = process_rss_kib();

    let idle_window = Duration::from_secs(2);
    let cpu_before = process_cpu_centiseconds();
    let idle_start = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build headless service runtime");
    let mut wakes = 0_u64;
    runtime.block_on(async {
        while idle_start.elapsed() < idle_window {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM perf_items", [], |row| row.get(0))
                .expect("service-loop read");
            wakes += 1;
        }
    });
    let idle_elapsed_ms = idle_start.elapsed().as_millis();
    let cpu_after = process_cpu_centiseconds();
    let idle_cpu_cs = cpu_after
        .zip(cpu_before)
        .and_then(|(after, before)| after.checked_sub(before));
    let rss_pass = rss_kib.is_some_and(|rss| rss < 307_200);
    let cpu_pass = idle_cpu_cs.is_some_and(|cpu| cpu < 20);
    println!(
        "startup_ready_ms={ready_ms} budget_ms=2000 pass={}",
        ready_ms < 2_000
    );
    println!(
        "rss_kib={} budget_kib=307200 pass={rss_pass}",
        rss_kib.unwrap_or_default()
    );
    println!(
        "idle_cpu_centiseconds={} window_ms={} budget_centiseconds=20 pass={cpu_pass}",
        idle_cpu_cs.unwrap_or_default(),
        idle_elapsed_ms
    );
    println!("headless_service_wakes={wakes} window_ms={idle_elapsed_ms}");
    println!(
        "GUI-only metrics: first frame, webview interactivity, React commits, rendering frame time and native watcher wakeups are not measurable in this headless harness."
    );
    // 2 s is the shipped (release) startup budget. A debug build under background load can
    // take far longer to open and migrate the database, so the debug ceiling only has to catch
    // a pathological regression.
    let ready_budget_ms = if cfg!(debug_assertions) {
        30_000
    } else {
        2_000
    };
    assert!(
        ready_ms < ready_budget_ms,
        "startup ready budget exceeded: {ready_ms} ms (budget {ready_budget_ms} ms)"
    );
    assert!(
        rss_pass,
        "RSS budget exceeded or unavailable: {rss_kib:?} KiB (budget 307200 KiB)"
    );
    assert!(
        cpu_pass,
        "idle CPU budget exceeded or unavailable: {idle_cpu_cs:?} centiseconds in {idle_elapsed_ms} ms"
    );
}

fn process_cpu_centiseconds() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "cputime=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let time = String::from_utf8(output.stdout).ok()?;
    let value = time.trim();
    let (whole, fraction) = value.split_once('.')?;
    let seconds = if let Some((minutes, seconds)) = whole.split_once(':') {
        minutes
            .parse::<u64>()
            .ok()?
            .checked_mul(60)?
            .checked_add(seconds.parse().ok()?)?
    } else {
        whole.parse().ok()?
    };
    seconds
        .checked_mul(100)?
        .checked_add(fraction.chars().take(2).collect::<String>().parse().ok()?)
}
