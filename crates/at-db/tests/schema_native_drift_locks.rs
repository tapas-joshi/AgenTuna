//! TF-014: schema for native items, locks and drift history.

mod common;

use std::time::{Duration, Instant};

use at_db::DbError;
use at_model::ItemId;
use rusqlite::Connection;
use rusqlite::params;

use common::{ALL_VERSIONS, columns, index_names, open_with};

fn constraint(error: rusqlite::Error) -> DbError {
    DbError::from(error)
}

fn assert_constraint_naming(error: DbError, needle: &str) {
    match error {
        DbError::Constraint { message } => assert!(
            message.to_lowercase().contains(&needle.to_lowercase()),
            "constraint message names {needle}: {message}"
        ),
        other => panic!("expected constraint error, got {other:?}"),
    }
}

fn insert_item(connection: &Connection, slug: &str) -> String {
    let id = ItemId::new().to_string();
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) VALUES (?1, 'skill', 'user', ?2, ?2)",
            params![id, slug],
        )
        .expect("item inserts");
    id
}

/// The nearest lock for an item, or `None` when nothing is locked.
///
/// This is the query a resolution pass runs: any lock row at a scope at or above
/// the resolved scope wins, nearest first.
fn nearest_lock(connection: &Connection, item_id: &str) -> Option<String> {
    connection
        .query_row(
            "SELECT scope FROM locks WHERE item_id = ?1 \
             ORDER BY CASE scope WHEN 'local' THEN 3 WHEN 'project' THEN 2 \
                                WHEN 'user' THEN 1 ELSE 0 END DESC LIMIT 1",
            [item_id],
            |row| row.get(0),
        )
        .ok()
}

#[test]
fn a_rescan_updates_last_seen_and_marks_vanished_items_missing() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    connection
        .execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, content_hash, \
             first_seen_at, last_seen_at) \
             VALUES ('nat_1', 'claude_code', 'user', 'skill', 'Local helper', \
                     '/home/me/.claude/skills/local-helper', 'hash-1', \
                     '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
            [],
        )
        .expect("native item inserts");

    // Second scan, unchanged content: the row is refreshed, not duplicated.
    connection
        .execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, content_hash, \
             first_seen_at, last_seen_at) \
             VALUES ('nat_ignored', 'claude_code', 'user', 'skill', 'Local helper', \
                     '/home/me/.claude/skills/local-helper', 'hash-1', \
                     '2026-11-05T00:00:00.000Z', '2026-11-05T00:00:00.000Z') \
             ON CONFLICT(path) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            [],
        )
        .expect("rescan upserts by path");

    let row: (u32, String, String, String) = connection
        .query_row(
            "SELECT COUNT(*), MAX(id), MAX(first_seen_at), MAX(last_seen_at) FROM native_items",
            [],
            |value| Ok((value.get(0)?, value.get(1)?, value.get(2)?, value.get(3)?)),
        )
        .expect("native rows read back");
    assert_eq!(row.0, 1, "one row per discovered path");
    assert_eq!(row.1, "nat_1", "the original id is kept");
    assert_eq!(row.2, "2026-11-01T00:00:00.000Z", "first_seen_at is kept");
    assert_eq!(row.3, "2026-11-05T00:00:00.000Z", "last_seen_at advances");

    // The folder no longer contains it: the row survives as `missing`.
    connection
        .execute(
            "UPDATE native_items SET status = 'missing' WHERE path = ?1",
            ["/home/me/.claude/skills/local-helper"],
        )
        .expect("mark missing");
    let (status, path): (String, String) = connection
        .query_row("SELECT status, path FROM native_items", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("row reads back");
    assert_eq!(status, "missing");
    assert_eq!(path, "/home/me/.claude/skills/local-helper");

    let error = connection
        .execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, status) \
             VALUES ('nat_2', 'claude_code', 'user', 'skill', 'Other', '/home/me/other', 'gone')",
            [],
        )
        .expect_err("unknown status fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn adoption_records_the_canonical_item_and_preserves_the_original_scan_data() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let canonical = insert_item(connection, "adopted-skill");

    connection
        .execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, content_hash, \
             first_seen_at, last_seen_at) \
             VALUES ('nat_adopt', 'omp', 'project', 'skill', 'Adopted', \
                     '/repo/.agents/skills/adopted', 'hash-adopt', \
                     '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z')",
            [],
        )
        .expect("native item inserts");

    connection
        .execute(
            "UPDATE native_items SET adopted_item_id = ?1, status = 'adopted' WHERE id = 'nat_adopt'",
            [&canonical],
        )
        .expect("adoption updates the row");

    let row: (String, String, String, String) = connection
        .query_row(
            "SELECT adopted_item_id, status, path, first_seen_at FROM native_items WHERE id = 'nat_adopt'",
            [],
            |value| Ok((value.get(0)?, value.get(1)?, value.get(2)?, value.get(3)?)),
        )
        .expect("row reads back");
    assert_eq!(row.0, canonical);
    assert_eq!(row.1, "adopted");
    assert_eq!(
        row.2, "/repo/.agents/skills/adopted",
        "the original path is kept"
    );
    assert_eq!(row.3, "2026-10-01T00:00:00.000Z", "first_seen_at is kept");
}

#[test]
fn locks_are_unique_per_item_and_scope_and_a_global_lock_covers_lower_scopes() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "locked");

    connection
        .execute(
            "INSERT INTO locks (item_id, scope, reason, locked_by) \
             VALUES (?1, 'global', 'Company policy', 'admin')",
            [&item],
        )
        .expect("global lock inserts");
    assert_eq!(nearest_lock(connection, &item).as_deref(), Some("global"));

    let error = connection
        .execute(
            "INSERT INTO locks (item_id, scope) VALUES (?1, 'global')",
            [&item],
        )
        .expect_err("a second lock for the same scope fails");
    assert_constraint_naming(constraint(error), "unique");

    connection
        .execute(
            "INSERT INTO locks (item_id, scope, reason, locked_by) VALUES (?1, 'user', 'Mine', 'me')",
            [&item],
        )
        .expect("user lock inserts");
    assert_eq!(
        nearest_lock(connection, &item).as_deref(),
        Some("user"),
        "the nearest lock wins"
    );

    connection
        .execute("DELETE FROM locks WHERE item_id = ?1", [&item])
        .expect("unlock");
    assert_eq!(nearest_lock(connection, &item), None);
}

#[test]
fn resolving_a_drift_event_sets_the_timestamp_and_resolution() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "drifted");

    connection
        .execute(
            "INSERT INTO drift_events (item_id, agent_id, scope, path, expected_hash, actual_hash) \
             VALUES (?1, 'claude_code', 'user', '/home/me/.claude/skills/d', 'expected', 'actual')",
            [&item],
        )
        .expect("drift event inserts");

    let open: (u32, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT COUNT(*), MAX(resolved_at), MAX(resolution) FROM drift_events \
             WHERE item_id = ?1 AND resolved_at IS NULL",
            [&item],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("open event reads back");
    assert_eq!(open, (1, None, None));

    connection
        .execute(
            "UPDATE drift_events SET resolved_at = '2026-11-05T10:00:00.000Z', resolution = 'restore', \
             backup_id = 'bkp_1' WHERE item_id = ?1",
            [&item],
        )
        .expect("resolve event");
    let resolved: (Option<String>, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT resolved_at, resolution, backup_id FROM drift_events WHERE item_id = ?1",
            [&item],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("resolved event reads back");
    assert_eq!(
        resolved,
        (
            Some("2026-11-05T10:00:00.000Z".to_owned()),
            Some("restore".to_owned()),
            Some("bkp_1".to_owned())
        )
    );

    let error = connection
        .execute(
            "INSERT INTO drift_events (agent_id, scope, path, resolution) \
             VALUES ('omp', 'user', '/x', 'merge')",
            [],
        )
        .expect_err("unknown resolution fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn unresolved_drift_events_are_newest_first_under_10_ms_with_100k_history_rows() {
    let mut database = open_with(ALL_VERSIONS);
    let items: Vec<String> = (0..100)
        .map(|index| {
            let id = ItemId::new().to_string();
            database
                .connection()
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) \
                     VALUES (?1, 'skill', 'user', ?2, ?2)",
                    params![id, format!("drift-{index}")],
                )
                .expect("seed item");
            id
        })
        .collect();

    let transaction = database
        .connection_mut()
        .transaction()
        .expect("begin seeding");
    {
        let mut event = transaction
            .prepare(
                "INSERT INTO drift_events (item_id, agent_id, scope, path, expected_hash, actual_hash, \
                 detected_at, resolved_at, resolution) \
                 VALUES (?1, 'claude_code', 'user', '/home/me/skill', 'expected', 'actual', ?2, ?3, ?4)",
            )
            .expect("prepare drift insert");
        for item in &items {
            for index in 0..1_000_u32 {
                let detected_at = format!("2026-11-01T00:00:00.{index:03}Z");
                let resolved = (index % 2 == 0).then_some(detected_at.clone());
                let resolution = resolved.as_ref().map(|_| "ignore");
                event
                    .execute(params![item, detected_at, resolved, resolution])
                    .expect("seed drift event");
            }
        }
    }
    transaction.commit().expect("commit seeding");

    let sql = "SELECT detected_at FROM drift_events \
               WHERE item_id = ?1 AND resolved_at IS NULL \
               ORDER BY detected_at DESC LIMIT 10";
    let connection = database.connection();
    let mut plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("plan prepares");
    let details: Vec<String> = plan
        .query_map([&items[0]], |row| row.get::<_, String>(3))
        .expect("plan runs")
        .collect::<Result<_, _>>()
        .expect("plan rows are text");
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("idx_drift_events_open")),
        "the unresolved query uses its index: {details:?}"
    );

    let start = Instant::now();
    let rows: Vec<String> = connection
        .prepare(sql)
        .expect("prepare drift query")
        .query_map([&items[0]], |row| row.get(0))
        .expect("run drift query")
        .collect::<Result<_, _>>()
        .expect("events read back");
    let elapsed = start.elapsed();
    assert_eq!(rows.len(), 10);
    let mut sorted = rows.clone();
    sorted.sort();
    sorted.reverse();
    assert_eq!(rows, sorted, "newest first");
    assert!(
        elapsed < Duration::from_millis(10),
        "the unresolved query took {elapsed:?} with 100k history rows"
    );

    let total: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM drift_events", [], |row| row.get(0))
        .expect("count history");
    assert_eq!(total, 100_000);
}

#[test]
fn deleting_a_canonical_item_leaves_native_and_drift_history_dangling() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "forced-delete");

    connection
        .execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, adopted_item_id, status) \
             VALUES ('nat_kept', 'codex', 'user', 'skill', 'Kept', '/home/me/kept', ?1, 'adopted')",
            [&item],
        )
        .expect("native item inserts");
    connection
        .execute(
            "INSERT INTO drift_events (item_id, agent_id, scope, path) \
             VALUES (?1, 'codex', 'user', '/home/me/kept')",
            [&item],
        )
        .expect("drift event inserts");

    connection
        .execute("DELETE FROM items WHERE id = ?1", [&item])
        .expect("force delete");

    let native: (u32, Option<String>) = connection
        .query_row(
            "SELECT COUNT(*), MAX(adopted_item_id) FROM native_items",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("native row survives");
    assert_eq!(native.0, 1, "the native row is not deleted");
    assert_eq!(
        native.1.as_deref(),
        Some(item.as_str()),
        "the dangling adoption reference is left as it was"
    );

    let drift: u32 = connection
        .query_row("SELECT COUNT(*) FROM drift_events", [], |row| row.get(0))
        .expect("drift history survives");
    assert_eq!(drift, 1);
}

#[test]
fn native_drift_and_lock_columns_and_indexes_match_the_story() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    assert_eq!(
        columns(connection, "native_items"),
        vec![
            "id",
            "agent_id",
            "scope",
            "kind",
            "name",
            "path",
            "content_hash",
            "first_seen_at",
            "last_seen_at",
            "adopted_item_id",
            "status",
        ]
    );
    assert_eq!(
        columns(connection, "locks"),
        vec!["id", "item_id", "scope", "reason", "locked_by", "locked_at"]
    );
    assert_eq!(
        columns(connection, "drift_events"),
        vec![
            "id",
            "item_id",
            "agent_id",
            "scope",
            "path",
            "expected_hash",
            "actual_hash",
            "detected_at",
            "resolved_at",
            "resolution",
            "backup_id",
        ]
    );

    let mut native = index_names(connection, "native_items");
    native.sort();
    assert_eq!(
        native,
        vec![
            "idx_native_items_agent_status",
            "idx_native_items_kind",
            "idx_native_items_last_seen_at",
        ]
    );
    let mut drift = index_names(connection, "drift_events");
    drift.sort();
    assert_eq!(
        drift,
        vec![
            "idx_drift_events_item_detected",
            "idx_drift_events_open",
            "idx_drift_events_resolved_at",
        ]
    );
    // `locks` has no explicit index: UNIQUE(item_id, scope) already serves the
    // per-item lookup.
    assert!(index_names(connection, "locks").is_empty());
}
