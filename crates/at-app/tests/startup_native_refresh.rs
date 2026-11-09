//! The startup native-findings refresh: an existing index heals, and startup never waits for it.
//!
//! The upgrade in question: 0.1.1 stored every skill an agent folder holds as `native_items` rows,
//! then a rescan the app never ran left them `missing`; schema v10 added `discovery_state` as NULL
//! for those rows. `open_database` only rescans a fresh or quarantined index, so without the
//! background job the Skills screen stayed empty after upgrading. These tests build that exact
//! shape, run the app-level startup plus job over it, and check the rows and the startup timing.
//!
//! Unix-only: the fixture installs skills as symbolic links.

#![cfg(unix)]

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use at_app::app::jobs::JobsControl;
use at_app::app::native_scan::refresh_native_findings;
use at_app::app::startup::{open_database, spawn_native_refresh};
use at_db::Database;
use at_engine::native_index::NativeRefreshReport;
use at_migrate::testkit::SymlinkedSkillsHome;
use at_model::AgentId;
use at_store::paths::AppPaths;
use rusqlite::Connection;
use support::{Sandbox, TestResult};

/// How long a test waits for the background worker before failing loudly.
const REFRESH_DEADLINE: Duration = Duration::from_secs(60);

/// Inserts the `native_items` rows the way the pre-fix release left them: `missing`, with the
/// schema-v10 backfill's NULL discovery state.
fn seed_stale_missing_rows(
    database: &Database,
    fixture: &SymlinkedSkillsHome,
    count: usize,
) -> TestResult<Vec<String>> {
    let mut paths: Vec<String> = fixture
        .links()
        .iter()
        .filter(|((agent, _), _)| *agent == AgentId::ClaudeCode)
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .take(count)
        .collect();
    paths.sort();
    assert_eq!(
        paths.len(),
        count,
        "the fixture provides enough linked skills"
    );
    for (index, path) in paths.iter().enumerate() {
        let name = std::path::Path::new(path)
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        database.connection().execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, status) \
             VALUES (?1, 'claude_code', 'user', 'skill', ?2, ?3, 'missing')",
            rusqlite::params![format!("nat_stale_{index}"), name, path],
        )?;
    }
    Ok(paths)
}

#[test]
fn schema_v10_missing_skill_rows_heal_on_the_first_start() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let paths = sandbox.paths()?;
    let fixture = SymlinkedSkillsHome::install(&sandbox.home, sandbox.root(), &sandbox.project)?;

    let stale = {
        let database = Database::open_file(paths.db_path())?;
        // The index belongs to a previous launch: `open_database` must treat it as current and
        // must not rebuild it, which is exactly the path that never rescanned before this fix.
        database.connection().execute(
            "INSERT INTO settings (key, value_json) VALUES ('startup.derived_built', 'true')",
            [],
        )?;
        let stale = seed_stale_missing_rows(&database, &fixture, 6)?;
        let missing: i64 = database.connection().query_row(
            "SELECT COUNT(*) FROM native_items WHERE status = 'missing' \
               AND discovery_state IS NULL AND kind = 'skill'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(missing, 6, "the fixture starts in the 0.1.1 shape");
        stale
    };

    let opened = open_database(&paths);
    assert!(
        opened.rebuild.is_none(),
        "an existing index is used as-is; the background job is what refreshes it"
    );
    let database = Arc::new(Mutex::new(
        opened.database.expect("the existing index opens"),
    ));

    let (finished_tx, finished_rx) = mpsc::channel();
    let spawned = spawn_native_refresh(
        paths.clone(),
        Arc::clone(&database),
        &JobsControl::default(),
        refresh_native_findings,
        move |report: NativeRefreshReport| {
            let _ = finished_tx.send(report);
        },
    );
    let handle = spawned.expect("the app is running");
    assert!(
        !handle.inner().is_finished(),
        "the job starts on a worker, not inline"
    );
    let report = finished_rx
        .recv_timeout(REFRESH_DEADLINE)
        .expect("the startup refresh reports within the deadline");
    tauri::async_runtime::block_on(handle).expect("the startup worker completes cleanly");
    println!(
        "upserted={} marked_missing={} resurrected={}",
        report.upserted, report.marked_missing, report.resurrected
    );
    assert!(
        report.resurrected >= 6,
        "every stale row is brought back, not only re-marked: {report:?}"
    );

    let database = database.lock().expect("the database lock is not poisoned");
    let remaining: i64 = database.connection().query_row(
        "SELECT COUNT(*) FROM native_items WHERE status = 'missing' AND kind = 'skill'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(remaining, 0, "no skill row stays missing after the start");
    for path in &stale {
        let (status, discovery_state, link_target) = database.connection().query_row(
            "SELECT status, discovery_state, link_target FROM native_items WHERE path = ?1",
            [path],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        assert_eq!(status, "new", "{path} is a current finding again");
        assert_eq!(
            discovery_state.as_deref(),
            Some("valid"),
            "{path} carries the state the schema-v10 backfill left NULL"
        );
        assert!(
            link_target.is_some(),
            "{path} records the folder its link resolved to"
        );
    }
    let indexed: i64 = database.connection().query_row(
        "SELECT COUNT(*) FROM native_items WHERE kind = 'skill' AND status = 'new'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        indexed > 6,
        "the scan indexes every agent's findings, not just the seeded paths"
    );
    Ok(())
}

#[test]
fn startup_does_not_wait_for_the_refresh_to_finish() -> TestResult<()> {
    static FINISHED: AtomicBool = AtomicBool::new(false);

    /// A refresh slow enough that a caller waiting for it could not return this fast.
    fn slow_refresh(_paths: &AppPaths, _connection: &Connection) -> Option<NativeRefreshReport> {
        std::thread::sleep(Duration::from_millis(400));
        FINISHED.store(true, Ordering::SeqCst);
        Some(NativeRefreshReport::default())
    }

    let sandbox = Sandbox::new()?;
    let paths = sandbox.paths()?;
    let database = Arc::new(Mutex::new(Database::open_file(paths.db_path())?));

    let (finished_tx, finished_rx) = mpsc::channel();
    let spawned_at = Instant::now();
    let handle = spawn_native_refresh(
        paths.clone(),
        Arc::clone(&database),
        &JobsControl::default(),
        slow_refresh,
        move |report: NativeRefreshReport| {
            let _ = finished_tx.send(report);
        },
    )
    .expect("the app is running");
    let spawn_elapsed = spawned_at.elapsed();
    assert!(
        spawn_elapsed < Duration::from_millis(250),
        "starting the job took {spawn_elapsed:?}; the scan must not run on the startup path"
    );
    assert!(
        !FINISHED.load(Ordering::SeqCst),
        "the caller returned while the slow refresh still runs"
    );
    finished_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the slow refresh finishes on its worker");
    tauri::async_runtime::block_on(handle).expect("the worker task completes cleanly");
    assert!(FINISHED.load(Ordering::SeqCst));
    Ok(())
}

#[test]
fn a_shutting_down_app_does_not_start_the_refresh() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let paths = sandbox.paths()?;
    let database = Arc::new(Mutex::new(Database::open_file(paths.db_path())?));
    let jobs = JobsControl::default();
    jobs.shutdown();
    let spawned =
        spawn_native_refresh(paths, database, &jobs, refresh_native_findings, |_report| {
            panic!("a cancelled app must not report a refresh")
        });
    assert!(spawned.is_none(), "shutdown wins over the startup scan");
    Ok(())
}
