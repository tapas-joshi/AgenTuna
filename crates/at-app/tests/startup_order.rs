//! Startup ordering and the truthfulness of what startup reports.
//!
//! Two invariants the shell depends on:
//!
//! 1. The handle it manages is only produced after the index is built: a current index is not
//!    rebuilt, and a rebuilt one is marked built in the same call. No command can therefore read
//!    or write a half-built index.
//! 2. The report it logs names the rows the index actually holds. The rebuild's own counter is
//!    taken before the follow-up native refresh writes discoveries, so it must be replaced by the
//!    real count rather than logged as `native_items=0`.

mod support;

use std::fs;
use std::time::Instant;

use at_app::app::startup::open_database;
use at_db::Database;
use support::{Sandbox, TestResult};

/// Writes one unmanaged skill package into an agent root that only the adapter-driven native
/// refresh scans (`~/.codex/skills`); the rebuild's own legacy scan reads `~/.agents/skills`.
/// That is exactly the mismatch that made the rebuild log report zero native rows while the
/// index held every discovery.
fn write_native_skill(sandbox: &Sandbox) -> TestResult<()> {
    let dir = sandbox.home.join(".codex/skills/loose-skill");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: loose-skill\ndescription: sandbox discovery\n---\n\nBody.\n",
    )?;
    Ok(())
}

#[test]
fn a_current_index_is_not_rebuilt_and_stays_cheap() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let paths = sandbox.paths()?;
    {
        let database = Database::open_file(paths.db_path())?;
        database.connection().execute(
            "INSERT INTO settings (key, value_json) VALUES ('startup.derived_built', 'true')",
            [],
        )?;
        database.connection().execute(
            "INSERT INTO items (id, kind, scope, slug, name, state, managed) \
             VALUES ('itm_00000000-0000-7000-8000-000000000001', 'skill', 'user', 'kept', 'Kept', 'active', 1)",
            [],
        )?;
    }

    let started = Instant::now();
    let opened = open_database(&paths);
    let elapsed = started.elapsed();
    println!("open_current_ms={}", elapsed.as_millis());

    let database = opened.database.expect("the index opens");
    assert!(
        opened.rebuild.is_none(),
        "a marked index must not be rebuilt at startup"
    );
    assert!(
        opened.warnings.is_empty(),
        "a healthy index reports nothing"
    );
    let items: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
    assert_eq!(items, 1, "the existing rows survive an unchanged startup");
    // The gate before the first paint: opening and validating a current index is milliseconds,
    // not a scan. The bound is generous so a loaded CI machine does not fail it.
    assert!(
        elapsed.as_millis() < 2_000,
        "opening a current index took {elapsed:?}"
    );
    Ok(())
}

#[test]
fn startup_reports_the_native_rows_the_index_holds_after_the_refresh() -> TestResult<()> {
    let sandbox = Sandbox::new()?;
    let paths = sandbox.paths()?;
    write_native_skill(&sandbox)?;

    let opened = open_database(&paths);
    let database = opened.database.expect("a fresh index is created");
    let report = opened.rebuild.expect("a fresh index is built at startup");
    let indexed = u64::try_from(database.connection().query_row(
        "SELECT COUNT(*) FROM native_items",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(0);
    assert_eq!(
        report.counts.native_items, indexed,
        "the reported native count must match the rows the index holds"
    );
    println!(
        "reported_native_items={} indexed_native_items={indexed}",
        report.counts.native_items
    );

    // The rebuild marked the index as built, so the next start does not scan again: the handle
    // the shell manages never describes a partially built index.
    let again = open_database(&paths);
    assert!(
        again.rebuild.is_none(),
        "the second start must reuse the rebuilt index"
    );
    assert!(again.database.is_some());
    Ok(())
}
