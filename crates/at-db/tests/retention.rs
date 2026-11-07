//! TF-050: retention and pruning, including the backup rules from CONTRACT
//! decisions 6 and 19.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use at_db::Database;
use at_db::retention::{
    KEEP_NEWEST_BACKUPS, MIN_BACKUP_RETENTION_DAYS, PRUNE_BATCH_SIZE, PruneReport, RetentionError,
    RetentionPolicy, UNDO_WINDOW_DAYS, ensure_undoable, prune_log_files, run,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

/// The instant every test prunes at.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// Unix seconds of [`now`], as SQLite bindings use them.
fn now_seconds() -> i64 {
    i64::try_from(
        now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs(),
    )
    .expect("fits")
}

/// A timestamp `days` before [`now`], in the format the database stores.
fn days_ago(connection: &Connection, days: u32) -> String {
    connection
        .query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch', ?2)",
            params![now_seconds(), format!("-{days} days")],
            |row| row.get(0),
        )
        .expect("timestamp formats")
}

fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens")
}

/// Insert a backup `age_days` before [`now`].
fn insert_backup(
    connection: &Connection,
    id: &str,
    original_path: &str,
    age_days: u32,
    pinned: bool,
    operation_id: Option<&str>,
) {
    connection
        .execute(
            "INSERT INTO backups (id, operation_id, original_path, backup_path, created_at, pinned) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                operation_id,
                original_path,
                format!("/backups/{id}"),
                days_ago(connection, age_days),
                i64::from(pinned)
            ],
        )
        .expect("backup inserts");
}

/// Insert a resolved or unresolved drift event `age_days` before [`now`].
fn insert_drift(connection: &Connection, age_days: u32, resolved_days: Option<u32>) {
    connection
        .execute(
            "INSERT INTO drift_events (agent_id, scope, path, detected_at, resolved_at, resolution) \
             VALUES ('omp', 'user', ?1, ?2, ?3, ?4)",
            params![
                format!("/users/me/.omp/skills/drift-{age_days}-{resolved_days:?}"),
                days_ago(connection, age_days),
                resolved_days.map(|days| days_ago(connection, days)),
                resolved_days.map(|_| "restore"),
            ],
        )
        .expect("drift event inserts");
}

/// Insert an operation started `age_days` before [`now`].
fn insert_operation(connection: &Connection, id: &str, age_days: u32, status: &str) {
    connection
        .execute(
            "INSERT INTO operations (id, kind, scope, actor, status, summary, started_at, created_at) \
             VALUES (?1, 'deploy', 'user', 'ui', ?3, 'seeded', ?2, ?2)",
            params![id, days_ago(connection, age_days), status],
        )
        .expect("operation inserts");
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count reads")
}

/// Prune once and hand the report back.
fn prune(database: &Database, days: u32) -> PruneReport {
    run(
        database.connection(),
        &RetentionPolicy::new(days, now()),
        None,
    )
    .expect("pruning succeeds")
}

#[test]
fn backups_survive_by_age_newest_three_and_pin() {
    let database = database();
    let connection = database.connection();

    // Path A: three fresh backups, one 31-day-old outsider, one pinned 400-day-old.
    for (index, age) in [2_u32, 5, 10].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_keep_{index}"),
            "/store/a.md",
            *age,
            false,
            None,
        );
    }
    insert_backup(connection, "bkp_old_a", "/store/a.md", 31, false, None);
    insert_backup(connection, "bkp_pinned", "/store/a.md", 400, true, None);
    // Path B: four backups older than the window, so the newest three stay.
    for (index, age) in [40_u32, 45, 50, 55].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_b_{index}"),
            "/store/b.md",
            *age,
            false,
            None,
        );
    }
    // Path C: 29 days old, inside the minimum retention.
    insert_backup(connection, "bkp_young", "/store/c.md", 29, false, None);

    let report = prune(&database, 30);
    let backups = report.table("backups").expect("backups reported");
    assert_eq!(
        backups.deleted, 2,
        "only the 31-day outsider and bkp_b_3 go"
    );

    let remaining: Vec<String> = {
        let mut statement = connection
            .prepare("SELECT id FROM backups ORDER BY id")
            .expect("statement prepares");
        statement
            .query_map([], |row| row.get(0))
            .expect("rows read")
            .collect::<Result<Vec<_>, _>>()
            .expect("ids are text")
    };
    assert!(!remaining.contains(&"bkp_old_a".to_owned()));
    assert!(!remaining.contains(&"bkp_b_3".to_owned()));
    for kept in [
        "bkp_keep_0",
        "bkp_keep_1",
        "bkp_keep_2",
        "bkp_pinned",
        "bkp_b_0",
        "bkp_b_1",
        "bkp_b_2",
        "bkp_young",
    ] {
        assert!(remaining.contains(&kept.to_owned()), "{kept} must survive");
    }
    assert_eq!(KEEP_NEWEST_BACKUPS, 3);
}

#[test]
fn a_larger_backup_retention_extends_the_window() {
    let database = database();
    let connection = database.connection();
    // Four fresh backups keep bkp_old outside the newest three, so only age decides.
    for (index, age) in [2_u32, 5, 10].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_new_{index}"),
            "/store/a.md",
            *age,
            false,
            None,
        );
    }
    insert_backup(connection, "bkp_old", "/store/a.md", 59, false, None);

    let report = prune(&database, 60);
    assert_eq!(
        report.table("backups").expect("backups reported").deleted,
        0,
        "59 days is inside a 60-day window"
    );

    let report = prune(&database, 30);
    assert_eq!(
        report.table("backups").expect("backups reported").deleted,
        1
    );
}

#[test]
fn the_backup_floor_is_thirty_days() {
    let database = database();
    let connection = database.connection();
    for (index, age) in [2_u32, 5, 10].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_new_{index}"),
            "/store/a.md",
            *age,
            false,
            None,
        );
    }
    insert_backup(connection, "bkp_old", "/store/a.md", 31, false, None);

    assert_eq!(
        RetentionPolicy::new(1, now()).backup_retention_days,
        MIN_BACKUP_RETENTION_DAYS
    );
    let report = prune(&database, 1);
    assert_eq!(
        report.table("backups").expect("backups reported").deleted,
        1,
        "a 1-day request still keeps a 30-day window"
    );
}

#[test]
fn drift_events_are_pruned_only_when_resolved_and_old() {
    let database = database();
    let connection = database.connection();
    insert_drift(connection, 100, Some(89));
    insert_drift(connection, 100, Some(91));
    insert_drift(connection, 200, None);
    insert_drift(connection, 300, Some(91));

    let report = prune(&database, 30);
    assert_eq!(
        report
            .table("drift_events")
            .expect("drift reported")
            .deleted,
        2
    );
    let mut statement = connection
        .prepare("SELECT path, resolved_at FROM drift_events ORDER BY path")
        .expect("statement prepares");
    let left: Vec<(String, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("rows read")
        .collect::<Result<Vec<_>, _>>()
        .expect("columns read");
    assert_eq!(left.len(), 2);
    assert_eq!(
        left[0].0, "/users/me/.omp/skills/drift-100-Some(89)",
        "a 89-day-old resolved event survives"
    );
    assert_eq!(
        left[1].0, "/users/me/.omp/skills/drift-200-None",
        "an unresolved event is never pruned"
    );
}

#[test]
fn the_undo_window_is_thirty_days_and_pruning_respects_it() {
    let database = database();
    let connection = database.connection();
    insert_operation(connection, "op_29", 29, "succeeded");
    insert_operation(connection, "op_31", 31, "succeeded");
    insert_operation(connection, "op_400", 400, "succeeded");
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action, after_hash) \
             VALUES ('op_29', '/store/a.md', 'modified', 'hash')",
            [],
        )
        .expect("file inserts");

    let young = at_db::ops::OperationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAA").expect("ULID");
    assert!(matches!(
        ensure_undoable(connection, &young, now()),
        Err(RetentionError::UnknownOperation { .. })
    ));

    let report = prune(&database, 30);
    assert_eq!(
        report
            .table("operations")
            .expect("operations reported")
            .deleted,
        1,
        "only the 400-day operation goes"
    );
    assert_eq!(count(connection, "operations WHERE id = 'op_31'"), 1);
    assert_eq!(count(connection, "operations WHERE id = 'op_29'"), 1);
    assert_eq!(
        count(connection, "operation_files WHERE operation_id = 'op_29'"),
        1,
        "within the undo window a 29-day operation keeps its file rows"
    );
    assert_eq!(UNDO_WINDOW_DAYS, 30);

    // `ensure_undoable` is the refusal path TC-010 calls.
    let old = at_db::ops::OperationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAB").expect("ULID");
    connection
        .execute(
            "INSERT INTO operations (id, kind, status, started_at) VALUES (?1, 'deploy', 'succeeded', ?2)",
            params![old.as_str(), days_ago(connection, 31)],
        )
        .expect("operation inserts");
    let error = ensure_undoable(connection, &old, now()).expect_err("31 days is expired");
    assert!(
        matches!(&error, RetentionError::UndoExpired { days: 30, .. }),
        "{error}"
    );

    let recent = at_db::ops::OperationId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("ULID");
    connection
        .execute(
            "INSERT INTO operations (id, kind, status, started_at) VALUES (?1, 'deploy', 'succeeded', ?2)",
            params![recent.as_str(), days_ago(connection, 29)],
        )
        .expect("operation inserts");
    ensure_undoable(connection, &recent, now()).expect("29 days is still undoable");
}

#[test]
fn health_checks_are_never_touched() {
    let database = database();
    let connection = database.connection();
    for index in 0..10 {
        connection
            .execute(
                "INSERT INTO health_checks (agent_id, server_name, scope, status, trigger, checked_at) \
                 VALUES ('omp', 'server', 'user', 'ok', 'manual', ?1)",
                [now_seconds() - i64::from(index) * 30 * 86_400],
            )
            .expect("health check inserts");
        connection
            .execute(
                "INSERT INTO health_checks (agent_id, server_name, scope, status, trigger, checked_at) \
                 VALUES ('omp', 'server', 'user', 'ok', 'manual', ?1)",
                [now_seconds()],
            )
            .expect("health check inserts");
    }
    insert_drift(connection, 100, Some(91));
    let before = count(connection, "health_checks");
    let report = prune(&database, 30);
    assert_eq!(count(connection, "health_checks"), before);
    assert_eq!(count(connection, "health_checks"), 20);
    assert!(report.table("health_checks").is_none());
    assert_eq!(report.total_deleted(), 1, "the drift event was pruned");
}

#[test]
fn pruning_runs_in_batches_of_five_hundred() {
    let database = database();
    let connection = database.connection();
    let transaction = connection
        .unchecked_transaction()
        .expect("transaction opens");
    for index in 0..1_200 {
        transaction
            .execute(
                "INSERT INTO drift_events (agent_id, scope, path, detected_at, resolved_at, resolution) \
                 VALUES ('omp', 'user', ?1, ?2, ?2, 'ignore')",
                params![format!("/path/{index}"), days_ago(connection, 100)],
            )
            .expect("drift event inserts");
    }
    transaction.commit().expect("transaction commits");

    let report = prune(&database, 30);
    let drift = report.table("drift_events").expect("drift reported");
    assert_eq!(drift.deleted, 1_200);
    assert_eq!(drift.batches, 3, "500 + 500 + 200 rows");
    assert_eq!(PRUNE_BATCH_SIZE, 500);
    assert_eq!(count(connection, "drift_events"), 0);
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(100)
    };
    assert!(
        drift.longest_batch <= budget,
        "a single delete batch stayed under {budget:?} (took {:?})",
        drift.longest_batch
    );
    assert!(report.longest_batch >= drift.longest_batch);
}

#[test]
fn pruning_defers_while_an_operation_is_active() {
    let database = database();
    let connection = database.connection();
    insert_drift(connection, 100, Some(91));
    insert_operation(connection, "op_active", 0, "running");

    let report = prune(&database, 30);
    assert!(report.deferred);
    assert_eq!(report.active_operations, 1);
    assert_eq!(report.total_deleted(), 0);
    assert_eq!(count(connection, "drift_events"), 1);

    // A running row older than the grace period is a dead process, not an
    // active operation.
    connection
        .execute(
            "UPDATE operations SET started_at = ?1 WHERE id = 'op_active'",
            [days_ago(connection, 1)],
        )
        .expect("operation ages");
    let report = prune(&database, 30);
    assert!(!report.deferred);
    assert_eq!(
        report
            .table("drift_events")
            .expect("drift reported")
            .deleted,
        1
    );
}

#[test]
fn native_items_are_marked_missing_after_ninety_days() {
    let database = database();
    let connection = database.connection();
    for (id, age, status) in [
        ("n_old_new", 100, "new"),
        ("n_old_adopted", 100, "adopted"),
        ("n_old_missing", 100, "missing"),
        ("n_recent", 10, "new"),
        ("n_boundary", 89, "new"),
    ] {
        connection
            .execute(
                "INSERT INTO native_items (id, agent_id, scope, kind, name, path, first_seen_at, last_seen_at, status) \
                 VALUES (?1, 'omp', 'user', 'skill', ?1, ?2, ?3, ?3, ?4)",
                params![id, format!("/path/{id}"), days_ago(connection, age), status],
            )
            .expect("native item inserts");
    }

    let report = prune(&database, 30);
    let native = report.table("native_items").expect("native items reported");
    assert_eq!(native.marked, 2, "new and adopted become missing");
    assert_eq!(native.deleted, 0, "rows are marked, never deleted");
    assert_eq!(
        count(connection, "native_items WHERE status = 'missing'"),
        3
    );
    assert_eq!(count(connection, "native_items"), 5);
}

#[test]
fn old_operations_survive_when_a_pinned_backup_references_them() {
    let database = database();
    let connection = database.connection();
    insert_operation(connection, "op_plain", 400, "succeeded");
    insert_operation(connection, "op_direct_pin", 400, "succeeded");
    insert_operation(connection, "op_file_pin", 400, "succeeded");
    insert_operation(connection, "op_recent", 30, "succeeded");
    insert_backup(
        connection,
        "bkp_pin",
        "/store/a.md",
        400,
        true,
        Some("op_direct_pin"),
    );
    insert_backup(connection, "bkp_pin_file", "/store/b.md", 400, true, None);
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action, backup_id) \
             VALUES ('op_file_pin', '/store/b.md', 'modified', 'bkp_pin_file')",
            [],
        )
        .expect("file inserts");
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) \
             VALUES ('op_plain', '/store/c.md', 'modified')",
            [],
        )
        .expect("file inserts");

    let report = prune(&database, 30);
    assert_eq!(
        report
            .table("operations")
            .expect("operations reported")
            .deleted,
        1
    );
    assert_eq!(count(connection, "operations WHERE id = 'op_plain'"), 0);
    assert_eq!(
        count(
            connection,
            "operation_files WHERE operation_id = 'op_plain'"
        ),
        0,
        "the cascade removes the pruned operation's file rows"
    );
    assert_eq!(
        count(connection, "operations WHERE id = 'op_direct_pin'"),
        1
    );
    assert_eq!(count(connection, "operations WHERE id = 'op_file_pin'"), 1);
    assert_eq!(count(connection, "operations WHERE id = 'op_recent'"), 1);
}

#[test]
fn vacuum_runs_only_above_the_free_page_threshold() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("index.sqlite");
    let database = Database::open_file(&path).expect("database opens");
    let connection = database.connection();
    let transaction = connection
        .unchecked_transaction()
        .expect("transaction opens");
    for index in 0..3_000 {
        transaction
            .execute(
                "INSERT INTO drift_events (agent_id, scope, path, detected_at, resolved_at, resolution) \
                 VALUES ('omp', 'user', ?1, ?2, ?2, 'restore')",
                params![format!("/path/{index}"), days_ago(connection, 120)],
            )
            .expect("drift event inserts");
    }
    transaction.commit().expect("transaction commits");

    let report = prune(&database, 30);
    assert_eq!(
        report
            .table("drift_events")
            .expect("drift reported")
            .deleted,
        3_000
    );
    assert!(
        report.vacuumed,
        "3k deleted rows leave more than 20% free pages"
    );

    let report = prune(&database, 30);
    assert!(!report.vacuumed, "a run that deletes nothing never vacuums");
}

#[test]
fn log_files_older_than_thirty_days_are_deleted() {
    let dir = TempDir::new().expect("temp dir");
    let old = dir.path().join("agentuna.1.log");
    let fresh = dir.path().join("agentuna.log");
    std::fs::write(&old, b"old").expect("fixture writes");
    std::fs::write(&fresh, b"fresh").expect("fixture writes");
    for (path, age_days) in [(&old, 40_u64), (&fresh, 1)] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("file opens")
            .set_modified(
                now()
                    .checked_sub(Duration::from_secs(age_days * 86_400))
                    .expect("representable"),
            )
            .expect("mtime sets");
    }
    std::fs::create_dir(dir.path().join("nested")).expect("directory creates");

    let deleted = prune_log_files(dir.path(), now(), 30).expect("logs prune");
    assert_eq!(deleted, 1);
    assert!(!old.exists());
    assert!(fresh.exists());
    assert!(
        dir.path().join("nested").is_dir(),
        "directories are left alone"
    );

    let missing = dir.path().join("no-such-dir");
    assert_eq!(
        prune_log_files(&missing, now(), 30).expect("missing dir is fine"),
        0
    );
}

#[test]
fn a_full_run_reports_every_table_it_visited() {
    let dir = TempDir::new().expect("temp dir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir(&log_dir).expect("log directory creates");
    let old_log = log_dir.join("old.log");
    std::fs::write(&old_log, b"old").expect("fixture writes");
    std::fs::File::options()
        .write(true)
        .open(&old_log)
        .expect("file opens")
        .set_modified(
            now()
                .checked_sub(Duration::from_secs(31 * 86_400))
                .expect("representable"),
        )
        .expect("mtime sets");

    let database = database();
    let connection = database.connection();
    insert_drift(connection, 100, Some(91));
    for (index, age) in [2_u32, 5, 10].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_new_{index}"),
            "/store/a.md",
            *age,
            false,
            None,
        );
    }
    insert_backup(connection, "bkp_old", "/store/a.md", 90, false, None);
    insert_operation(connection, "op_old", 400, "succeeded");

    let report = run(
        database.connection(),
        &RetentionPolicy::new(30, now()),
        Some(&log_dir),
    )
    .expect("pruning succeeds");

    for table in [
        "drift_events",
        "backups",
        "operations",
        "operation_files",
        "native_items",
        "log_files",
    ] {
        assert!(report.table(table).is_some(), "{table} is reported");
    }
    assert_eq!(report.table("log_files").expect("logs").deleted, 1);
    assert_eq!(report.total_deleted(), 4);
    assert!(!report.deferred);
    assert!(!old_log.exists());
}

#[test]
fn a_deferred_run_skips_the_log_pass() {
    let dir = TempDir::new().expect("temp dir");
    let log_dir = dir.path().join("logs");
    std::fs::create_dir(&log_dir).expect("log directory creates");
    let old_log = log_dir.join("old.log");
    std::fs::write(&old_log, b"old").expect("fixture writes");
    std::fs::File::options()
        .write(true)
        .open(&old_log)
        .expect("file opens")
        .set_modified(
            now()
                .checked_sub(Duration::from_secs(40 * 86_400))
                .expect("representable"),
        )
        .expect("mtime sets");

    let database = database();
    let connection = database.connection();
    insert_operation(connection, "op_active", 0, "running");

    let report = run(
        database.connection(),
        &RetentionPolicy::new(30, now()),
        Some(&log_dir),
    )
    .expect("pruning defers");
    assert!(report.deferred);
    assert!(report.tables.is_empty());
    assert!(old_log.exists(), "log pruning is part of the deferred run");
}

#[test]
fn pruning_an_empty_database_is_a_no_op() {
    let database = database();
    let report = prune(&database, 30);
    assert!(!report.deferred);
    assert_eq!(report.total_deleted(), 0);
    assert_eq!(report.total_marked(), 0);
    assert!(!report.vacuumed);
    for table in [
        "drift_events",
        "backups",
        "operations",
        "operation_files",
        "native_items",
    ] {
        let entry = report.table(table).expect("table is reported");
        assert_eq!(entry.batches, 0, "{table} needs no batch");
    }
}

#[test]
fn pruning_keeps_the_database_valid_for_its_readers() {
    let database = database();
    let connection = database.connection();
    for (index, age) in [2_u32, 5, 10].iter().enumerate() {
        insert_backup(
            connection,
            &format!("bkp_new_{index}"),
            "/store/a.md",
            *age,
            false,
            None,
        );
    }
    insert_backup(connection, "bkp_old", "/store/a.md", 90, false, None);
    insert_operation(connection, "op_recent", 5, "succeeded");
    connection
        .execute(
            "INSERT INTO operation_files (operation_id, path, action) \
             VALUES ('op_recent', '/store/a.md', 'modified')",
            [],
        )
        .expect("file inserts");

    let report = prune(&database, 30);
    assert_eq!(report.table("backups").expect("backups").deleted, 1);
    let detail = at_db::ops::operation_detail(connection, "op_recent")
        .expect("detail reads")
        .expect("operation survives");
    assert_eq!(detail.files.len(), 1);
    let timeline = at_db::ops::recent_changes(connection, &at_db::ops::RecentChangesQuery::new(10))
        .expect("timeline reads");
    assert_eq!(timeline.len(), 1);
    assert!(matches!(
        at_db::ops::operation_detail(connection, "op_missing"),
        Ok(None)
    ));
}
