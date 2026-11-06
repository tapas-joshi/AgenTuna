//! TF-015: schema for MCP health checks and item sources.

mod common;

use std::time::{Duration, Instant};

use at_db::DbError;
use at_model::{ErrorKind, ErrorReport, ItemId, REDACTION_MASK};
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
            "INSERT INTO items (id, kind, scope, slug, name) \
             VALUES (?1, 'mcp_server', 'user', ?2, ?2)",
            params![id, slug],
        )
        .expect("item inserts");
    id
}

#[test]
fn every_status_trigger_and_transport_value_is_accepted() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "health-matrix");

    let mut checked_at = 0_i64;
    for (index, status) in ["ok", "failed", "unknown", "skipped_offline"]
        .into_iter()
        .enumerate()
    {
        for (trigger_index, trigger) in ["manual", "scheduled", "startup"].into_iter().enumerate() {
            for transport in ["stdio", "http", "sse"] {
                checked_at += 1;
                connection
                    .execute(
                        "INSERT INTO health_checks (item_id, scope, server_name, transport, \
                         checked_at, status, trigger) \
                         VALUES (?1, 'user', ?2, ?3, ?4, ?5, ?6)",
                        params![
                            item,
                            format!("server-{index}-{trigger_index}-{transport}"),
                            transport,
                            checked_at,
                            status,
                            trigger
                        ],
                    )
                    .expect("known values are accepted");
            }
        }
    }

    let error = connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, status, trigger) \
             VALUES (?1, 'user', 's', 'stdio', 0, 'timeout', 'manual')",
            [&item],
        )
        .expect_err("status 'timeout' fails");
    assert_constraint_naming(constraint(error), "check");

    let error = connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, status, trigger) \
             VALUES (?1, 'user', 's', 'stdio', 0, 'ok', 'cron')",
            [&item],
        )
        .expect_err("trigger 'cron' fails");
    assert_constraint_naming(constraint(error), "check");

    let error = connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, status, trigger) \
             VALUES (?1, 'user', 's', 'websocket', 0, 'ok', 'manual')",
            [&item],
        )
        .expect_err("unknown transport fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn failed_ok_and_skipped_rows_store_the_fields_their_status_needs() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "health-fields");

    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, \
             status, trigger, error_code, error_text_redacted) \
             VALUES (?1, 'user', 'fs', 'stdio', 1_700_000_000_000, 'failed', 'manual', \
                     'auth_failed', 'connect failed: ***')",
            [&item],
        )
        .expect("failed check inserts");
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, \
             status, trigger, latency_ms, tools_count) \
             VALUES (?1, 'user', 'fs', 'stdio', 1_700_000_001_000, 'ok', 'manual', 42, 7)",
            [&item],
        )
        .expect("ok check inserts");
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, \
             status, trigger) \
             VALUES (?1, 'user', 'fs', 'http', 1_700_000_002_000, 'skipped_offline', 'scheduled')",
            [&item],
        )
        .expect("skipped check inserts");

    let failed: (Option<i64>, Option<i64>, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT latency_ms, tools_count, error_code, error_text_redacted FROM health_checks \
             WHERE status = 'failed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("failed row reads back");
    assert_eq!(
        failed,
        (
            None,
            None,
            Some("auth_failed".to_owned()),
            Some("connect failed: ***".to_owned())
        )
    );

    let ok: (Option<i64>, Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT latency_ms, tools_count, error_text_redacted FROM health_checks WHERE status = 'ok'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("ok row reads back");
    assert_eq!(ok, (Some(42), Some(7), None));

    let skipped: (Option<i64>, Option<i64>, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT latency_ms, tools_count, error_code, error_text_redacted FROM health_checks \
             WHERE status = 'skipped_offline'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("skipped row reads back");
    assert_eq!(skipped, (None, None, None, None));

    // `checked_at` is unix milliseconds, so a text timestamp is refused by STRICT.
    let error = connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, status, trigger) \
             VALUES (?1, 'user', 's', 'stdio', '2026-11-05T00:00:00.000Z', 'ok', 'manual')",
            [&item],
        )
        .expect_err("checked_at is unix milliseconds");
    let message = format!("{error}");
    assert!(
        message.contains("INTEGER"),
        "STRICT names the expected type: {message}"
    );
}

#[test]
fn stored_error_text_is_the_redacted_column_only() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    let all_columns = columns(connection, "health_checks");
    assert!(all_columns.contains(&"error_text_redacted".to_owned()));
    assert!(
        !all_columns.contains(&"error_text".to_owned()),
        "there is no column for raw error text"
    );
    for column in &all_columns {
        assert!(
            !column.contains("stderr") && !column.contains("output") && !column.contains("raw"),
            "unexpected text-bearing column {column}"
        );
    }

    // The writer redacts with the session redactor before inserting; the
    // sentinel is a value that must never reach the row.
    const SENTINEL: &str = "sk-live-SENTINEL-do-not-store";
    let report = ErrorReport::new(
        ErrorKind::Auth,
        format!("connect failed: OPENAI_API_KEY={SENTINEL} exited with status 1"),
    )
    .redacted_with([SENTINEL]);
    assert!(!report.message.contains(SENTINEL));
    assert!(report.message.contains(REDACTION_MASK));

    let item = insert_item(connection, "redaction");
    connection
        .execute(
            "INSERT INTO health_checks (item_id, scope, server_name, transport, checked_at, \
             status, trigger, error_code, error_text_redacted) \
             VALUES (?1, 'user', 'fs', 'stdio', 1_700_000_000_000, 'failed', 'manual', 'auth', ?2)",
            params![item, report.message],
        )
        .expect("redacted text inserts");

    let stored: String = connection
        .query_row(
            "SELECT error_text_redacted FROM health_checks WHERE item_id = ?1",
            [&item],
            |row| row.get(0),
        )
        .expect("stored text reads back");
    assert!(!stored.contains(SENTINEL), "no secret value is stored");
    assert!(
        !stored.contains("sk-live-"),
        "no fragment of the value is stored"
    );
    assert!(stored.contains(REDACTION_MASK));
}

#[test]
fn the_latest_check_query_uses_the_item_checked_index_and_stays_under_10_ms_with_100k_rows() {
    let mut database = open_with(ALL_VERSIONS);
    let transaction = database
        .connection_mut()
        .transaction()
        .expect("begin seeding");
    let items: Vec<String> = (0..100)
        .map(|index| {
            let id = ItemId::new().to_string();
            transaction
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) \
                     VALUES (?1, 'mcp_server', 'user', ?2, ?2)",
                    params![id, format!("server-{index}")],
                )
                .expect("seed item");
            id
        })
        .collect();
    {
        let mut check = transaction
            .prepare(
                "INSERT INTO health_checks (item_id, scope, agent_id, server_name, transport, \
                 checked_at, status, trigger, latency_ms, tools_count) \
                 VALUES (?1, 'user', 'claude_code', 'fs', 'stdio', ?2, 'ok', 'manual', 10, 3)",
            )
            .expect("prepare health insert");
        for item in &items {
            for index in 0..1_000_i64 {
                check
                    .execute(params![item, index])
                    .expect("seed health check");
            }
        }
    }
    transaction.commit().expect("commit seeding");

    let sql = "SELECT status, latency_ms, tools_count FROM health_checks \
               WHERE item_id = ?1 ORDER BY checked_at DESC LIMIT 1";
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
            .any(|detail| detail.contains("idx_health_checks_item_checked")),
        "the latest-check query uses (item_id, checked_at DESC): {details:?}"
    );

    let start = Instant::now();
    let row: (String, Option<i64>, Option<i64>) = connection
        .query_row(sql, [&items[0]], |value| {
            Ok((value.get(0)?, value.get(1)?, value.get(2)?))
        })
        .expect("latest check reads back");
    let elapsed = start.elapsed();
    assert_eq!(row.0, "ok");
    assert_eq!(row.1, Some(10));
    assert_eq!(row.2, Some(3));
    assert!(
        elapsed < Duration::from_millis(10),
        "the latest-check query took {elapsed:?} with 100k history rows"
    );

    let total: u32 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM health_checks", [], |row| row.get(0))
        .expect("count history");
    assert_eq!(total, 100_000);
}

#[test]
fn git_folder_and_marketplace_sources_store_their_metadata() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "sourced");

    connection
        .execute(
            "INSERT INTO sources (id, item_id, kind, url, subpath, commit_sha, git_ref, \
             content_hash, remote_sha, last_checked_at) \
             VALUES ('src_git', ?1, 'git', 'https://example.test/skills', 'skills/pdf', \
                     'abc123', 'main', 'hash-git', 'def456', '2026-11-05T09:00:00.000Z')",
            [&item],
        )
        .expect("git source inserts");
    connection
        .execute(
            "INSERT INTO sources (id, kind, url) VALUES ('src_folder', 'folder', '/Users/me/skills/local')",
            [],
        )
        .expect("folder source inserts");
    connection
        .execute(
            "INSERT INTO sources (id, kind, marketplace_id, install_count, version) \
             VALUES ('src_market', 'marketplace', 'mkt_pdf', 12_345, '1.2.0')",
            [],
        )
        .expect("marketplace source inserts");

    let git: (String, String, String, String) = connection
        .query_row(
            "SELECT url, subpath, commit_sha, remote_sha FROM sources WHERE id = 'src_git'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("git source reads back");
    assert_eq!(
        git,
        (
            "https://example.test/skills".to_owned(),
            "skills/pdf".to_owned(),
            "abc123".to_owned(),
            "def456".to_owned()
        )
    );

    let folder: (Option<String>, String) = connection
        .query_row(
            "SELECT url, kind FROM sources WHERE id = 'src_folder'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("folder source reads back");
    assert_eq!(
        folder,
        (
            Some("/Users/me/skills/local".to_owned()),
            "folder".to_owned()
        )
    );

    let market: (Option<String>, Option<i64>) = connection
        .query_row(
            "SELECT marketplace_id, install_count FROM sources WHERE id = 'src_market'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("marketplace source reads back");
    assert_eq!(market, (Some("mkt_pdf".to_owned()), Some(12_345)));

    // Defaults: etag/ignored_sha/auto_update start NULL, local_modified starts 0.
    let defaults: (Option<String>, Option<String>, Option<i64>, i64) = connection
        .query_row(
            "SELECT etag, ignored_sha, auto_update, local_modified FROM sources WHERE id = 'src_git'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("defaults read back");
    assert_eq!(defaults, (None, None, None, 0));

    // auto_update is 0 off, 1 on, 2 inherit; anything else is a bug.
    connection
        .execute(
            "UPDATE sources SET auto_update = 2 WHERE id = 'src_git'",
            [],
        )
        .expect("inherit is accepted");
    let error = connection
        .execute(
            "UPDATE sources SET auto_update = 3 WHERE id = 'src_git'",
            [],
        )
        .expect_err("unknown auto_update fails");
    assert_constraint_naming(constraint(error), "check");

    // Soft delete keeps the content hash for the update check.
    connection
        .execute(
            "UPDATE sources SET deleted_at = '2026-11-05T10:00:00.000Z' WHERE id = 'src_git'",
            [],
        )
        .expect("soft delete");
    let deleted: (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT deleted_at, content_hash FROM sources WHERE id = 'src_git'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("soft-deleted source reads back");
    assert_eq!(
        deleted,
        (
            Some("2026-11-05T10:00:00.000Z".to_owned()),
            Some("hash-git".to_owned())
        )
    );

    let error = connection
        .execute(
            "INSERT INTO sources (id, kind) VALUES ('src_bad', 'tarball')",
            [],
        )
        .expect_err("unknown source kind fails");
    assert_constraint_naming(constraint(error), "check");
}

#[test]
fn deleting_an_item_detaches_its_source() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item = insert_item(connection, "detached");
    connection
        .execute(
            "INSERT INTO sources (id, item_id, kind, url) \
             VALUES ('src_1', ?1, 'git', 'https://example.test/skills')",
            [&item],
        )
        .expect("source inserts");

    connection
        .execute("DELETE FROM items WHERE id = ?1", [&item])
        .expect("delete item");
    let source: (u32, Option<String>) = connection
        .query_row(
            "SELECT COUNT(*), MAX(item_id) FROM sources WHERE id = 'src_1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("source reads back");
    assert_eq!(
        source,
        (1, None),
        "the source survives with a NULL item link"
    );
}

#[test]
fn health_and_source_columns_and_indexes_match_the_story() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    assert_eq!(
        columns(connection, "health_checks"),
        vec![
            "id",
            "item_id",
            "scope",
            "project_id",
            "agent_id",
            "server_name",
            "transport",
            "checked_at",
            "status",
            "trigger",
            "latency_ms",
            "tools_count",
            "error_code",
            "error_text_redacted",
        ]
    );
    assert_eq!(
        columns(connection, "sources"),
        vec![
            "id",
            "item_id",
            "kind",
            "url",
            "subpath",
            "commit_sha",
            "git_ref",
            "version",
            "content_hash",
            "marketplace_id",
            "install_count",
            "etag",
            "remote_sha",
            "ignored_sha",
            "local_modified",
            "auto_update",
            "deleted_at",
            "installed_at",
            "last_checked_at",
            "update_available",
            "license",
        ]
    );

    let mut health = index_names(connection, "health_checks");
    health.sort();
    assert_eq!(
        health,
        vec![
            "idx_health_checks_checked_at",
            "idx_health_checks_item_checked",
            "idx_health_checks_server_checked",
            "idx_health_checks_status",
        ]
    );
    let mut sources = index_names(connection, "sources");
    sources.sort();
    assert_eq!(
        sources,
        vec![
            "idx_sources_deleted_at",
            "idx_sources_item_id",
            "idx_sources_kind",
            "idx_sources_marketplace_id",
            "idx_sources_url",
        ]
    );
}
