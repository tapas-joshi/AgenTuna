//! TM-009: health_checks queries, status mapping and retention.

mod common;

use at_db::Database;
use at_db::health::{
    HEALTH_KEEP_PER_SERVER, HealthDot, HealthStatus, HealthTransport, NewHealthCheck,
    failing_servers, history, last_n, prune_health_checks, record_check, status_of,
};
use at_db::migrations::{MIGRATION_V1_SQL, Migration, Migrations};
use at_model::{Scope, SecretRedactor};

use common::{ALL_VERSIONS, open_with};

struct SentinelRedactor(&'static str);

impl SecretRedactor for SentinelRedactor {
    fn redact(&self, text: &str) -> String {
        text.replace(self.0, "[SECRET_MASKED]")
    }
}

#[test]
fn module_issues_no_ddl() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    // Shipped schema compiles every query
    let item = "srv_test";
    assert_eq!(last_n(connection, item, 5).unwrap().len(), 0);
    assert_eq!(status_of(connection, item).unwrap(), None);
    assert_eq!(failing_servers(connection, Scope::User).unwrap().len(), 0);
    assert_eq!(history(connection, item, None, None).unwrap().len(), 0);
    assert_eq!(prune_health_checks(connection).unwrap(), 0);

    // SQL strings contain no DDL
    let sql_samples = [
        "SELECT id, item_id FROM health_checks",
        "DELETE FROM health_checks WHERE id IN (SELECT id FROM health_checks)",
        "INSERT INTO health_checks (item_id, scope, server_name, checked_at, status, trigger) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    ];
    for sql in sql_samples {
        let lowered = sql.to_ascii_lowercase();
        assert!(!lowered.contains("create "));
        assert!(!lowered.contains("alter "));
        assert!(!lowered.contains("drop "));
    }

    // Against a schema-v1 database, health_checks table does not exist
    let v1_set = Migrations::new(&[Migration::new(1, MIGRATION_V1_SQL)]).expect("v1");
    let v1_db = Database::open_in_memory_with(v1_set).expect("v1 db");
    let err = last_n(v1_db.connection(), item, 5).unwrap_err();
    assert!(
        err.to_string().contains("no such table: health_checks"),
        "expected no such table error, got: {err}"
    );
}

#[test]
fn last_n_returns_five_newest_rows_and_status_round_trips() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item_id = "srv_roundtrip";

    let statuses = [
        HealthStatus::Ok,
        HealthStatus::Failed,
        HealthStatus::Unknown,
        HealthStatus::SkippedOffline,
    ];

    for (i, &status) in statuses.iter().enumerate() {
        let check = NewHealthCheck::new("my-server", Scope::User, 1000 + i as i64, status)
            .for_item(item_id)
            .in_context(None, None, Some(HealthTransport::Stdio));
        record_check(connection, &check, None).expect("record check");
    }

    // Tolerate fewer than 5: we inserted 4 rows
    let rows = last_n(connection, item_id, 5).expect("last 5");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].status, HealthStatus::SkippedOffline);
    assert_eq!(rows[0].checked_at, 1003);
    assert_eq!(rows[1].status, HealthStatus::Unknown);
    assert_eq!(rows[2].status, HealthStatus::Failed);
    assert_eq!(rows[3].status, HealthStatus::Ok);

    // Verify dot mappings
    assert_eq!(HealthStatus::Ok.dot(), HealthDot::Healthy);
    assert_eq!(HealthStatus::Failed.dot(), HealthDot::Failing);
    assert_eq!(HealthStatus::Unknown.dot(), HealthDot::Unknown);
    assert_eq!(
        HealthStatus::SkippedOffline.dot(),
        HealthDot::SkippedOffline
    );

    // Now insert 3 more rows to exceed 5
    for i in 4..7 {
        let check =
            NewHealthCheck::new("my-server", Scope::User, 1000 + i as i64, HealthStatus::Ok)
                .for_item(item_id);
        record_check(connection, &check, None).expect("record check");
    }

    // Now last_n(..., 5) returns exactly the 5 newest in descending checked_at order
    let rows5 = last_n(connection, item_id, 5).expect("last 5");
    assert_eq!(rows5.len(), 5);
    assert_eq!(rows5[0].checked_at, 1006);
    assert_eq!(rows5[1].checked_at, 1005);
    assert_eq!(rows5[2].checked_at, 1004);
    assert_eq!(rows5[3].checked_at, 1003);
    assert_eq!(rows5[4].checked_at, 1002);
}

#[test]
fn retention_keeps_newest_200_and_prunes_50_oldest() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item_id = "srv_retention";

    // Insert 250 rows with increasing checked_at
    for i in 0..250 {
        let check = NewHealthCheck::new(
            "retention-server",
            Scope::User,
            1000 + i as i64,
            HealthStatus::Ok,
        )
        .for_item(item_id);
        record_check(connection, &check, None).expect("record check");
    }

    let count_before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM health_checks WHERE item_id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .expect("count before");
    assert_eq!(count_before, 250);

    // Hourly prune
    let pruned = prune_health_checks(connection).expect("prune");
    assert_eq!(pruned, 50);

    let count_after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM health_checks WHERE item_id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .expect("count after");
    assert_eq!(count_after, HEALTH_KEEP_PER_SERVER as i64);

    let min_checked: i64 = connection
        .query_row(
            "SELECT MIN(checked_at) FROM health_checks WHERE item_id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .expect("min checked");
    assert_eq!(
        min_checked,
        1000 + 50,
        "the 50 oldest rows (checked_at 1000..1049) must be deleted"
    );

    let max_checked: i64 = connection
        .query_row(
            "SELECT MAX(checked_at) FROM health_checks WHERE item_id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .expect("max checked");
    assert_eq!(max_checked, 1000 + 249, "the newest row must remain intact");
}

#[test]
fn failing_servers_filters_correctly() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();

    // 1. srv_failing: last check is failed -> should be returned
    let check1 = NewHealthCheck::new("failing-srv", Scope::User, 100, HealthStatus::Ok)
        .for_item("srv_failing");
    record_check(connection, &check1, None).unwrap();
    let check2 = NewHealthCheck::new("failing-srv", Scope::User, 200, HealthStatus::Failed)
        .for_item("srv_failing");
    record_check(connection, &check2, None).unwrap();

    // 2. srv_recovered: failed earlier, but last check is ok -> should NOT be returned
    let check3 = NewHealthCheck::new("recovered-srv", Scope::User, 100, HealthStatus::Failed)
        .for_item("srv_recovered");
    record_check(connection, &check3, None).unwrap();
    let check4 = NewHealthCheck::new("recovered-srv", Scope::User, 200, HealthStatus::Ok)
        .for_item("srv_recovered");
    record_check(connection, &check4, None).unwrap();

    // 3. srv_unknown: last check is unknown -> should NOT be returned
    let check5 = NewHealthCheck::new("unknown-srv", Scope::User, 100, HealthStatus::Failed)
        .for_item("srv_unknown");
    record_check(connection, &check5, None).unwrap();
    let check6 = NewHealthCheck::new("unknown-srv", Scope::User, 200, HealthStatus::Unknown)
        .for_item("srv_unknown");
    record_check(connection, &check6, None).unwrap();

    // 4. srv_never_checked: not in DB -> should NOT be returned

    let failing = failing_servers(connection, Scope::User).expect("failing servers");
    assert_eq!(failing.len(), 1);
    assert_eq!(failing[0].item_id.as_deref(), Some("srv_failing"));
    assert_eq!(failing[0].status, HealthStatus::Failed);

    // Check status_of
    assert_eq!(
        status_of(connection, "srv_failing").unwrap(),
        Some(HealthStatus::Failed)
    );
    assert_eq!(
        status_of(connection, "srv_recovered").unwrap(),
        Some(HealthStatus::Ok)
    );
    assert_eq!(
        status_of(connection, "srv_unknown").unwrap(),
        Some(HealthStatus::Unknown)
    );
    assert_eq!(status_of(connection, "srv_never_checked").unwrap(), None);
}

#[test]
fn error_text_truncated_and_secrets_redacted() {
    let database = open_with(ALL_VERSIONS);
    let connection = database.connection();
    let item_id = "srv_err";

    let secret_sentinel = "my-super-secret-token-12345";
    let long_prefix = "a".repeat(600);
    let error_text = format!("{long_prefix} with secret: {secret_sentinel}");

    let redactor = SentinelRedactor(secret_sentinel);
    let check = NewHealthCheck::new("err-server", Scope::User, 100, HealthStatus::Failed)
        .for_item(item_id)
        .failed_with("ERR_CRASH", &error_text);

    let _row_id = record_check(connection, &check, Some(&redactor)).expect("record check");
    let check_row = at_db::health::latest_check(connection, item_id)
        .expect("latest")
        .expect("some check");

    let redacted = check_row.error_text_redacted.expect("error text");
    assert!(
        redacted.len() <= 512,
        "length must be <= 512, got {}",
        redacted.len()
    );
    assert!(
        !redacted.contains(secret_sentinel),
        "sentinel secret must not appear in stored text"
    );
}
