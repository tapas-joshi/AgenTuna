//! TF-009: the operations log write API, timeline queries and undo listing.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use at_db::ops::{
    CurrentState, DiskFileProbe, FileAction, FileProbe, NewOperation, OperationActor,
    OperationKind, OperationStatus, RecentChangesQuery, TouchedFile, attach_file, begin, finish,
    operation_detail, recent_changes, record, record_completed, record_interrupted, undo_file_list,
    undo_file_list_with,
};
use at_db::{Database, DbError};
use at_model::Scope;
use rusqlite::params;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Lowercase hex SHA-256, the hash shape the writers record.
fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// An in-memory database with the full shipped schema.
fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens with the shipped schema")
}

/// One skill item and one agent deployment, so the timeline join has names.
fn seed_deployment(connection: &rusqlite::Connection, operation: Option<&str>) -> (String, String) {
    connection
        .execute(
            "INSERT INTO agents (id, display_name) VALUES ('claude_code', 'Claude Code')",
            [],
        )
        .expect("agent inserts");
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name) \
             VALUES ('itm_1', 'skill', 'project', 'deploy', 'Deploy skill')",
            [],
        )
        .expect("item inserts");
    connection
        .execute(
            "INSERT INTO deployments (item_id, agent_id, scope, state, operation_id) \
             VALUES ('itm_1', 'claude_code', 'project', 'linked', ?1)",
            [operation],
        )
        .expect("deployment inserts");
    ("Deploy skill".to_owned(), "Claude Code".to_owned())
}

#[test]
fn a_successful_deploy_writes_the_operation_and_every_file() {
    let database = database();
    let connection = database.connection();

    let operation = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui)
            .with_scope(Scope::Project)
            .with_summary("Deploy deploy to Claude Code")
            .with_correlation_id("corr-1"),
    )
    .expect("operation begins");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(
            "/project/.claude/skills/deploy/SKILL.md",
            FileAction::Created,
        )
        .with_hashes(None, Some("hash-after"))
        .with_sizes(0, 120),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new("/project/.claude/settings.json", FileAction::Modified)
            .with_hashes(Some("hash-before"), Some("hash-after"))
            .with_backup("bkp_1"),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new("/project/.claude/old.md", FileAction::Deleted)
            .with_hashes(Some("hash-before"), None)
            .with_sizes(40, 0),
    )
    .expect("file attaches");
    assert!(finish(connection, &operation, OperationStatus::Succeeded, None).expect("finishes"));

    let detail = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.kind, Some(OperationKind::Deploy));
    assert_eq!(detail.operation.scope, Some(Scope::Project));
    assert_eq!(detail.operation.actor, Some(OperationActor::Ui));
    assert_eq!(detail.operation.status, Some(OperationStatus::Succeeded));
    assert_eq!(
        detail.operation.summary.as_deref(),
        Some("Deploy deploy to Claude Code")
    );
    assert_eq!(detail.operation.correlation_id.as_deref(), Some("corr-1"));
    assert!(detail.operation.finished_at.is_some());
    assert_eq!(detail.files.len(), 3);
    assert_eq!(detail.files[0].action, Some(FileAction::Created));
    assert_eq!(detail.files[0].before_hash, None);
    assert_eq!(detail.files[0].after_hash.as_deref(), Some("hash-after"));
    assert_eq!(detail.files[1].backup_id.as_deref(), Some("bkp_1"));
    assert_eq!(detail.files[2].bytes_after, Some(0));
}

#[test]
fn a_failed_operation_keeps_its_error_and_every_file_it_touched() {
    let database = database();
    let connection = database.connection();

    let operation = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Cli)
            .with_scope(Scope::User)
            .with_summary("Deploy deploy to Codex"),
    )
    .expect("operation begins");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(
            "/users/me/.codex/skills/deploy/SKILL.md",
            FileAction::Created,
        )
        .with_hashes(None, Some("hash-after")),
    )
    .expect("file attaches");
    assert!(
        finish(
            connection,
            &operation,
            OperationStatus::Failed,
            Some("permission_denied")
        )
        .expect("finishes")
    );

    let detail = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.status, Some(OperationStatus::Failed));
    assert_eq!(
        detail.operation.error_kind.as_deref(),
        Some("permission_denied")
    );
    assert_eq!(detail.files.len(), 1, "file rows survive a failure");
}

#[test]
fn all_twenty_six_kinds_round_trip_through_the_database() {
    let database = database();
    let connection = database.connection();

    for kind in OperationKind::ALL {
        begin(
            connection,
            &NewOperation::new(*kind, OperationActor::Watcher),
        )
        .expect("operation begins");
    }
    let changes = recent_changes(connection, &RecentChangesQuery::new(50)).expect("timeline reads");
    assert_eq!(changes.len(), 26);
    let mut kinds: Vec<OperationKind> = changes.iter().filter_map(|row| row.kind).collect();
    kinds.sort_unstable();
    let mut expected = OperationKind::ALL.to_vec();
    expected.sort_unstable();
    assert_eq!(kinds, expected);
}

#[test]
fn a_stored_value_this_binary_does_not_know_is_a_typed_error() {
    let database = database();
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO operations (id, kind, actor, status) \
             VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV', 'teleport', 'ui', 'running')",
            [],
        )
        .expect("the column has no CHECK, so the row can be seeded");

    let error = recent_changes(connection, &RecentChangesQuery::new(10)).expect_err("bad kind");
    assert!(matches!(error, DbError::UnknownValue(_)), "{error}");
}

#[test]
fn the_timeline_is_newest_first_and_joins_item_and_agent_names() {
    let database = database();
    let connection = database.connection();

    let first = begin(
        connection,
        &NewOperation::new(OperationKind::Scan, OperationActor::Watcher),
    )
    .expect("operation begins");
    finish(connection, &first, OperationStatus::Succeeded, None).expect("finishes");
    let second = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui).with_scope(Scope::Project),
    )
    .expect("operation begins");
    let (item_name, agent_name) = seed_deployment(connection, Some(second.as_str()));
    attach_file(
        connection,
        &second,
        &TouchedFile::new(
            "/project/.claude/skills/deploy/SKILL.md",
            FileAction::Created,
        )
        .with_hashes(None, Some("hash-after")),
    )
    .expect("file attaches");
    finish(connection, &second, OperationStatus::Succeeded, None).expect("finishes");

    let changes = recent_changes(connection, &RecentChangesQuery::new(50)).expect("timeline reads");
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].id, second.as_str(), "newest first");
    assert_eq!(changes[0].item_name.as_deref(), Some(item_name.as_str()));
    assert_eq!(
        changes[0].agent_display_name.as_deref(),
        Some(agent_name.as_str())
    );
    assert_eq!(changes[0].file_count, 1);
    assert_eq!(changes[1].id, first.as_str());
    assert_eq!(changes[1].item_name, None);
    assert_eq!(changes[1].file_count, 0);
}

#[test]
fn the_timeline_filters_by_scope_kind_and_start() {
    let database = database();
    let connection = database.connection();

    let project = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui).with_scope(Scope::Project),
    )
    .expect("operation begins");
    let user = begin(
        connection,
        &NewOperation::new(OperationKind::EditSkill, OperationActor::Ui).with_scope(Scope::User),
    )
    .expect("operation begins");
    let unscoped = begin(
        connection,
        &NewOperation::new(OperationKind::Update, OperationActor::Updater),
    )
    .expect("operation begins");

    let by_scope = recent_changes(
        connection,
        &RecentChangesQuery::new(50).with_scope(Scope::Project),
    )
    .expect("timeline reads");
    assert_eq!(by_scope.len(), 1);
    assert_eq!(by_scope[0].id, project.as_str());

    let by_kind = recent_changes(
        connection,
        &RecentChangesQuery::new(50).with_kind(OperationKind::EditSkill),
    )
    .expect("timeline reads");
    assert_eq!(by_kind.len(), 1);
    assert_eq!(by_kind[0].id, user.as_str());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let future = recent_changes(
        connection,
        &RecentChangesQuery::new(50).since(i64::try_from(now + 60).expect("fits")),
    )
    .expect("timeline reads");
    assert!(future.is_empty());

    let all = recent_changes(connection, &RecentChangesQuery::new(50)).expect("timeline reads");
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|row| row.id == unscoped.as_str()));
}

#[test]
fn the_timeline_limit_is_clamped() {
    let database = database();
    let connection = database.connection();
    for _ in 0..3 {
        begin(
            connection,
            &NewOperation::new(OperationKind::Scan, OperationActor::Cli),
        )
        .expect("operation begins");
    }
    let none = recent_changes(connection, &RecentChangesQuery::new(0)).expect("timeline reads");
    assert_eq!(none.len(), 1, "a zero limit still returns one row");
    let two = recent_changes(connection, &RecentChangesQuery::new(2)).expect("timeline reads");
    assert_eq!(two.len(), 2);
}

#[test]
fn two_writers_do_not_lose_operations() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("index.sqlite");
    Database::open_file(&path).expect("database opens");

    let mut handles = Vec::new();
    for writer in 0..2 {
        let path = path.clone();
        handles.push(thread::spawn(move || {
            let database = Database::open_file(&path).expect("second handle opens");
            for _ in 0..25 {
                let operation = begin(
                    database.connection(),
                    &NewOperation::new(OperationKind::Deploy, OperationActor::Cli),
                )
                .expect("operation begins");
                assert!(
                    finish(
                        database.connection(),
                        &operation,
                        OperationStatus::Succeeded,
                        None
                    )
                    .expect("finishes")
                );
            }
            writer
        }));
    }
    for handle in handles {
        handle.join().expect("writer finishes");
    }

    let database = Database::open_file(&path).expect("database reopens");
    let changes = recent_changes(database.connection(), &RecentChangesQuery::new(500))
        .expect("timeline reads");
    assert_eq!(
        changes.len(),
        50,
        "every operation from both writers landed"
    );
    assert!(
        changes
            .iter()
            .all(|row| row.status == Some(OperationStatus::Succeeded))
    );
}

#[test]
fn the_timeline_stays_fast_on_a_large_fixture() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("index.sqlite");
    let mut database = Database::open_file(&path).expect("database opens");

    // 3k items, one deployment each, 5k operations with two files each.
    let connection = database.connection_mut();
    let transaction = connection.transaction().expect("transaction opens");
    transaction
        .execute(
            "INSERT INTO agents (id, display_name) VALUES ('omp', 'omp'), ('codex', 'Codex')",
            [],
        )
        .expect("agents insert");
    for index in 0..3_000 {
        transaction
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name) \
                 VALUES (?1, 'skill', 'user', ?2, ?3)",
                params![
                    format!("itm_{index}"),
                    format!("skill-{index}"),
                    format!("Skill number {index}")
                ],
            )
            .expect("item inserts");
        transaction
            .execute(
                "INSERT INTO deployments (item_id, agent_id, scope, state, operation_id) \
                 VALUES (?1, 'omp', 'user', 'linked', ?2)",
                params![format!("itm_{index}"), format!("op_{index}")],
            )
            .expect("deployment inserts");
    }
    for index in 0..5_000 {
        transaction
            .execute(
                "INSERT INTO operations (id, kind, scope, actor, status, summary) \
                 VALUES (?1, 'deploy', 'user', 'ui', 'succeeded', ?2)",
                params![
                    format!("01ARZ3NDEKTSV4RRFFQ69G5F{index:02}"),
                    format!("deploy {index}")
                ],
            )
            .expect("operation inserts");
        for file in 0..2 {
            transaction
                .execute(
                    "INSERT INTO operation_files (operation_id, path, action, after_hash) \
                     VALUES (?1, ?2, 'created', 'hash')",
                    params![
                        format!("01ARZ3NDEKTSV4RRFFQ69G5F{index:02}"),
                        format!("/users/me/.omp/skills/skill-{index}/file-{file}.md")
                    ],
                )
                .expect("file inserts");
        }
    }
    transaction.commit().expect("transaction commits");

    let connection = database.connection();
    let query = RecentChangesQuery::new(50);
    let mut fastest = Duration::from_secs(60);
    let mut rows = 0;
    for _ in 0..5 {
        let started = Instant::now();
        rows = recent_changes(connection, &query)
            .expect("timeline reads")
            .len();
        fastest = fastest.min(started.elapsed());
    }
    assert_eq!(rows, 50);
    assert!(
        fastest < Duration::from_millis(20),
        "recent_changes took {fastest:?} for 50 of 5k operations"
    );
}

#[test]
fn undo_file_list_inverts_actions_and_flags_every_change() {
    let dir = TempDir::new().expect("temp dir");
    let database = database();
    let connection = database.connection();

    let unchanged = dir.path().join("unchanged.md");
    std::fs::write(&unchanged, b"same content").expect("fixture writes");
    let changed = dir.path().join("changed.md");
    std::fs::write(&changed, b"someone else edited this").expect("fixture writes");
    let missing = dir.path().join("missing.md");
    let reappeared = dir.path().join("reappeared.md");
    std::fs::write(&reappeared, b"back again").expect("fixture writes");
    let link = dir.path().join("linked");
    std::fs::create_dir(&link).expect("fixture directory");

    let operation = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui),
    )
    .expect("operation begins");
    let unchanged_hash = hash(b"same content");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(unchanged.to_str().expect("utf-8"), FileAction::Modified)
            .with_hashes(Some("before"), Some(&unchanged_hash)),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(changed.to_str().expect("utf-8"), FileAction::Modified)
            .with_hashes(Some("before"), Some("recorded")),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(missing.to_str().expect("utf-8"), FileAction::Created)
            .with_hashes(None, Some("recorded")),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(reappeared.to_str().expect("utf-8"), FileAction::Deleted)
            .with_hashes(Some("before"), None),
    )
    .expect("file attaches");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new(link.to_str().expect("utf-8"), FileAction::Linked)
            .with_hashes(None, Some("recorded")),
    )
    .expect("file attaches");
    finish(connection, &operation, OperationStatus::Succeeded, None).expect("finishes");

    let list = undo_file_list(connection, operation.as_str())
        .expect("undo list reads")
        .expect("operation exists");
    assert_eq!(list.files.len(), 5);
    assert_eq!(list.window_days, at_db::retention::UNDO_WINDOW_DAYS);
    assert_eq!(list.files[0].inverse, Some(at_db::ops::UndoAction::Restore));
    assert!(
        !list.files[0].conflicted,
        "content still matches after_hash"
    );
    assert_eq!(list.files[1].inverse, Some(at_db::ops::UndoAction::Restore));
    assert!(
        list.files[1].conflicted,
        "content changed after the operation"
    );
    assert_eq!(list.files[2].inverse, Some(at_db::ops::UndoAction::Delete));
    assert!(list.files[2].conflicted, "a created file is gone");
    assert_eq!(list.files[3].inverse, Some(at_db::ops::UndoAction::Restore));
    assert!(list.files[3].conflicted, "a deleted file came back");
    assert_eq!(list.files[4].inverse, Some(at_db::ops::UndoAction::Delete));
    assert!(
        !list.files[4].conflicted,
        "directories are unverifiable, not changed"
    );
    assert_eq!(list.files[4].current, CurrentState::Unverifiable);
}

#[test]
fn a_probe_that_cannot_verify_a_path_never_marks_it_conflicted() {
    struct BlindProbe;
    impl FileProbe for BlindProbe {
        fn current_state(&self, _path: &Path) -> CurrentState {
            CurrentState::Unverifiable
        }
    }

    let database = database();
    let connection = database.connection();
    let operation = begin(
        connection,
        &NewOperation::new(OperationKind::EditMcp, OperationActor::Ui),
    )
    .expect("operation begins");
    attach_file(
        connection,
        &operation,
        &TouchedFile::new("/users/me/.claude.json", FileAction::Modified)
            .with_hashes(Some("before"), Some("after")),
    )
    .expect("file attaches");
    finish(connection, &operation, OperationStatus::Succeeded, None).expect("finishes");

    let list = undo_file_list_with(connection, operation.as_str(), &BlindProbe)
        .expect("undo list reads")
        .expect("operation exists");
    assert_eq!(list.files[0].current, CurrentState::Unverifiable);
    assert!(!list.files[0].conflicted);
    assert_eq!(list.files[0].restore_hash.as_deref(), Some("before"));
}

#[test]
fn finishing_never_rewrites_kind_scope_or_started_at() {
    let database = database();
    let connection = database.connection();
    let operation = begin(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui)
            .with_scope(Scope::Project)
            .with_summary("Deploy"),
    )
    .expect("operation begins");
    let before = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists")
        .operation;

    assert!(finish(connection, &operation, OperationStatus::Succeeded, None).expect("finishes"));
    let after = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists")
        .operation;
    assert_eq!(after.kind, before.kind);
    assert_eq!(after.scope, before.scope);
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.summary, before.summary);

    assert!(
        !finish(
            connection,
            &operation,
            OperationStatus::Failed,
            Some("late")
        )
        .expect("finishes"),
        "an operation finishes exactly once"
    );
    let unchanged = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists")
        .operation;
    assert_eq!(unchanged.status, Some(OperationStatus::Succeeded));
    assert_eq!(unchanged.error_kind, None);

    let running = begin(
        connection,
        &NewOperation::new(OperationKind::Scan, OperationActor::Ui),
    )
    .expect("operation begins");
    assert!(
        !finish(connection, &running, OperationStatus::Running, None).expect("no-op"),
        "running is not a terminal status"
    );
}

#[test]
fn record_writes_the_operation_its_files_and_the_outcome_in_one_transaction() {
    let database = database();
    let connection = database.connection();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_on_bad_path AFTER INSERT ON operation_files \
             WHEN NEW.path = '/boom' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .expect("trigger installs");

    let error = record(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui),
        &[
            TouchedFile::new("/good", FileAction::Created).with_hashes(None, Some("after")),
            TouchedFile::new("/boom", FileAction::Created).with_hashes(None, Some("after")),
        ],
        OperationStatus::Succeeded,
        None,
    )
    .expect_err("the trigger aborts the transaction");
    assert!(!error.to_string().is_empty());
    let operations: i64 = connection
        .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
        .expect("count reads");
    let files: i64 = connection
        .query_row("SELECT COUNT(*) FROM operation_files", [], |row| row.get(0))
        .expect("count reads");
    assert_eq!(operations, 0, "the operation row rolled back");
    assert_eq!(files, 0, "the file rows rolled back");

    let operation = record(
        connection,
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui),
        &[TouchedFile::new("/good", FileAction::Created).with_hashes(None, Some("after"))],
        OperationStatus::Succeeded,
        None,
    )
    .expect("record commits");
    let detail = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.operation.status, Some(OperationStatus::Succeeded));
}

#[test]
fn record_refuses_a_non_terminal_status() {
    let database = database();
    let error = record(
        database.connection(),
        &NewOperation::new(OperationKind::Deploy, OperationActor::Ui),
        &[],
        OperationStatus::Running,
        None,
    )
    .expect_err("running cannot be recorded as an outcome");
    assert!(matches!(
        error,
        DbError::InvalidValue {
            field: "status",
            ..
        }
    ));
}

#[test]
fn record_completed_writes_one_finished_row_without_files() {
    let database = database();
    let connection = database.connection();
    let operation = record_completed(
        connection,
        OperationKind::Settings,
        OperationActor::Ui,
        Some("Changed backup retention"),
    )
    .expect("settings row writes");

    let detail = operation_detail(connection, operation.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.kind, Some(OperationKind::Settings));
    assert_eq!(detail.operation.status, Some(OperationStatus::Succeeded));
    assert_eq!(detail.operation.scope, None);
    assert!(detail.operation.finished_at.is_some());
    assert!(detail.files.is_empty());
}

#[test]
fn attach_file_requires_an_existing_operation() {
    let database = database();
    let connection = database.connection();
    let unknown = at_db::ops::OperationId::new();
    let error = attach_file(
        connection,
        &unknown,
        &TouchedFile::new("/tmp/file", FileAction::Created),
    )
    .expect_err("the foreign key rejects the row");
    assert!(matches!(error, DbError::Constraint { .. }), "{error}");
}

#[test]
fn interrupted_jobs_get_a_row_and_running_rows_are_finished() {
    let database = database();
    let connection = database.connection();

    // A job the bridge never began: one row, stored with its own id.
    let started_at_ms = 1_700_000_000_123_i64;
    record_interrupted(
        connection,
        "job_0190abcdef",
        Some(OperationKind::Update),
        Some("Update check"),
        started_at_ms,
    )
    .expect("job row writes");
    let detail = operation_detail(connection, "job_0190abcdef")
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.status, Some(OperationStatus::Interrupted));
    assert_eq!(detail.operation.kind, Some(OperationKind::Update));
    assert_eq!(detail.operation.summary.as_deref(), Some("Update check"));
    assert_eq!(
        detail.operation.started_at, "2023-11-14T22:13:20.123Z",
        "the bridge's start time is preserved to the millisecond"
    );

    // A running operation the bridge did begin: finished, not overwritten.
    let running = begin(
        connection,
        &NewOperation::new(OperationKind::Scan, OperationActor::Ui),
    )
    .expect("operation begins");
    record_interrupted(connection, running.as_str(), None, Some("Scan"), 0)
        .expect("running row finishes");
    let detail = operation_detail(connection, running.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.status, Some(OperationStatus::Interrupted));
    assert_eq!(
        detail.operation.kind,
        Some(OperationKind::Scan),
        "kind stays"
    );

    // An already-finished row is not rewritten.
    record_interrupted(connection, running.as_str(), None, Some("late"), 0)
        .expect("finished row is left alone");
    let detail = operation_detail(connection, running.as_str())
        .expect("detail reads")
        .expect("operation exists");
    assert_eq!(detail.operation.status, Some(OperationStatus::Interrupted));
}

#[test]
fn the_disk_probe_hashes_files_and_reports_missing_paths() {
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, b"content").expect("fixture writes");
    let probe = DiskFileProbe;
    assert_eq!(
        probe.current_state(&file),
        CurrentState::Hash(hash(b"content"))
    );
    assert_eq!(
        probe.current_state(&dir.path().join("nope.md")),
        CurrentState::Missing
    );
    assert_eq!(probe.current_state(dir.path()), CurrentState::Unverifiable);
}

#[test]
fn unknown_operations_read_back_as_none() {
    let database = database();
    let connection = database.connection();
    assert!(
        operation_detail(connection, "01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("detail reads")
            .is_none()
    );
    assert!(
        undo_file_list(connection, "01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("undo list reads")
            .is_none()
    );
}
