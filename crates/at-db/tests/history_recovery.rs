//! TF-054: history survives index rebuilds and recovery, and recovery leaves no debris.
//!
//! The fixture is a 3,000-item, three-scope store tree. Every test gets its own temporary
//! support directory; the real home, Keychain and network are never touched.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use at_db::Database;
use at_db::rebuild::{
    HistoryCounts, RebuildPaths, RebuildProgress, normalized_derived_dump, rebuild,
    snapshot_history_counts,
};
use at_model::{ItemId, Scope};
use rusqlite::types::Value;
use serde_json::json;
use tempfile::TempDir;

const ITEMS_PER_SCOPE: usize = 1000;
/// Generous bound for the whole rebuild suite: the assertion only has to catch a pathological
/// slowdown, and a loaded CI runner (with a debug build) is far slower than a dev machine.
const SUITE_BOUND: Duration = Duration::from_secs(300);

struct Sandbox {
    dir: TempDir,
    home: PathBuf,
    support: PathBuf,
    user_store: PathBuf,
    global_store: PathBuf,
    project: PathBuf,
    project_store: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let home = dir.path().join("home");
        let support = dir.path().join("support");
        let user_store = home.join(".agentuna");
        let global_store = dir.path().join("global");
        let project = dir.path().join("project");
        let project_store = project.join(".agentuna");
        for directory in [&home, &support, &global_store, &project] {
            fs::create_dir_all(directory).expect("sandbox directory");
        }
        let sandbox = Self {
            dir,
            home,
            support,
            user_store,
            global_store,
            project,
            project_store,
        };
        sandbox.seed_agent_folders();
        sandbox.seed_stores();
        sandbox
    }

    fn db_path(&self) -> PathBuf {
        self.support.join("agentuna.db")
    }

    fn rebuild_paths(&self) -> RebuildPaths {
        RebuildPaths::new(&self.home, &self.user_store)
            .with_global_store(&self.global_store)
            .with_project_and_store(&self.project, &self.project_store)
    }

    fn open(&self) -> Database {
        Database::open_file(self.db_path()).expect("database opens")
    }

    fn seed_agent_folders(&self) {
        fs::create_dir_all(self.home.join(".claude/skills")).expect("claude skills");
        fs::write(self.home.join(".claude.json"), br#"{"mcpServers":{}}"#).expect("claude config");
        fs::write(
            self.home.join(".claude/CLAUDE.md"),
            b"# Claude instructions",
        )
        .expect("claude rule");
        fs::create_dir_all(self.home.join(".agents/skills")).expect("shared skills root");
        fs::create_dir_all(self.home.join(".codex")).expect("codex dir");
        fs::write(self.home.join(".codex/config.toml"), b"[mcp_servers]").expect("codex config");
        fs::write(self.home.join(".codex/AGENTS.md"), b"# Codex instructions").expect("codex rule");
        fs::create_dir_all(self.home.join(".omp/agent/managed-skills")).expect("omp skills");
        fs::write(self.home.join(".omp/agent/mcp.json"), br#"{"servers":{}}"#).expect("omp mcp");
        fs::write(
            self.home.join(".omp/agent/AGENTS.md"),
            b"# Omp instructions",
        )
        .expect("omp rule");
    }

    fn seed_stores(&self) {
        create_scope_items(
            &self.global_store,
            Scope::Global,
            0,
            "00000000-0000-0000-0000-0000000000aa",
        );
        create_scope_items(
            &self.user_store,
            Scope::User,
            ITEMS_PER_SCOPE,
            "00000000-0000-0000-0000-0000000000bb",
        );
        create_scope_items(
            &self.project_store,
            Scope::Project,
            ITEMS_PER_SCOPE * 2,
            "00000000-0000-0000-0000-0000000000cc",
        );
        let deployed = self
            .user_store
            .join(format!("skills/skill-{}", ITEMS_PER_SCOPE));
        let _ = std::os::unix::fs::symlink(&deployed, self.home.join(".claude/skills/skill-user"));
        let project_skill = self
            .project_store
            .join(format!("skills/skill-{}", ITEMS_PER_SCOPE * 2));
        let _ = std::os::unix::fs::symlink(
            &project_skill,
            self.home.join(".omp/agent/managed-skills/skill-project"),
        );
    }

    fn rebuild(&self, database: &mut Database) -> at_db::rebuild::RebuildReport {
        rebuild(database.connection_mut(), &self.rebuild_paths(), None).expect("rebuild")
    }
}

fn create_scope_items(store: &Path, scope: Scope, start: usize, store_id: &str) {
    let skills = store.join("skills");
    fs::create_dir_all(&skills).expect("skills dir");
    fs::create_dir_all(store.join("rules")).expect("rules dir");
    let mut items = Vec::with_capacity(ITEMS_PER_SCOPE);
    for index in start..start + ITEMS_PER_SCOPE {
        let slug = format!("skill-{index}");
        let folder = skills.join(&slug);
        fs::create_dir_all(&folder).expect("skill folder");
        fs::write(
            folder.join("SKILL.md"),
            format!(
                "---\nname: {slug}\ndescription: Recovery fixture {index}\n---\n\nBody {index}\n"
            ),
        )
        .expect("skill document");
        items.push(json!({
            "id": ItemId::new().to_string(),
            "kind": "skill",
            "scope": scope.wire_name(),
            "slug": slug,
            "name": format!("Skill {index}"),
            "path": format!("skills/{slug}"),
            "enabled_default": true,
        }));
    }
    fs::write(
        store.join("store.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "store_id": store_id,
            "scope": scope.wire_name(),
            "items": items,
        }))
        .expect("manifest json"),
    )
    .expect("store manifest");
}

/// Seeds every durable history table the rebuild must preserve.
fn seed_durable_history(connection: &rusqlite::Connection) -> HistoryCounts {
    for index in 1..=5 {
        connection
            .execute(
                "INSERT INTO operations (id, kind, scope, actor, status, summary, started_at) \
                 VALUES (?1, 'deploy', 'user', 'cli', 'succeeded', 'Deploy op', '2026-11-05T12:00:00.000Z')",
                rusqlite::params![format!("01ARZ3NDEKTSV4RRFFQ69G5F{index:02}")],
            )
            .expect("operation row");
    }
    for index in 1..=10 {
        connection
            .execute(
                "INSERT INTO operation_files (operation_id, path, action, before_hash, after_hash) \
                 VALUES (?1, ?2, 'modified', 'hash-before', 'hash-after')",
                rusqlite::params![
                    format!("01ARZ3NDEKTSV4RRFFQ69G5F{:02}", (index % 5) + 1),
                    format!("/path/file{index}.txt")
                ],
            )
            .expect("operation file row");
    }
    for index in 1..=4 {
        connection
            .execute(
                "INSERT INTO backups (id, operation_id, original_path, backup_path, size) \
                 VALUES (?1, '01ARZ3NDEKTSV4RRFFQ69G5F01', ?2, ?3, 1024)",
                rusqlite::params![
                    format!("bkp_{index}"),
                    format!("/original/{index}"),
                    format!("/backup/{index}.tar.gz")
                ],
            )
            .expect("backup row");
    }
    for index in 1..=8 {
        connection
            .execute(
                "INSERT INTO health_checks (scope, server_name, checked_at, status, trigger) \
                 VALUES ('user', ?1, ?2, 'ok', 'manual')",
                rusqlite::params![
                    format!("server-{index}"),
                    1_730_800_000 + (index as i64) * 1000
                ],
            )
            .expect("health check row");
    }
    for index in 1..=3 {
        connection
            .execute(
                "INSERT INTO drift_events (agent_id, scope, path, expected_hash, actual_hash) \
                 VALUES ('claude_code', 'user', ?1, 'hash-a', 'hash-b')",
                [format!("/drift/path/{index}")],
            )
            .expect("drift row");
    }
    for index in 1..=2 {
        connection
            .execute(
                "INSERT INTO preset_applications (id, preset_id, target_kind, target_id, member_count) \
                 VALUES (?1, 'prs_1', 'agent', 'claude_code', 2)",
                [index],
            )
            .expect("preset application row");
        connection
            .execute(
                "INSERT INTO preset_application_members (application_id, item_id, prior_enabled) \
                 VALUES (?1, ?2, 1)",
                rusqlite::params![index, format!("itm_member_{index}")],
            )
            .expect("preset member row");
    }
    connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('theme', '\"dark\"')",
            [],
        )
        .expect("theme setting");
    connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('telemetry_enabled', 'false')",
            [],
        )
        .expect("telemetry setting");
    snapshot_history_counts(connection).expect("history counts")
}

fn durable_history_dump(connection: &rusqlite::Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    let tables = [
        "operations",
        "operation_files",
        "backups",
        "health_checks",
        "drift_events",
        "preset_applications",
        "preset_application_members",
        "settings",
    ];
    tables
        .iter()
        .map(|table| {
            let sql = if *table == "settings" {
                "SELECT * FROM settings WHERE key <> 'rebuild.state' ORDER BY rowid"
            } else {
                // The table name is one of the fixed literals above.
                &format!("SELECT * FROM {table} ORDER BY rowid")
            };
            let mut statement = connection.prepare(sql).expect("history statement");
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    let mut values = Vec::with_capacity(columns);
                    for index in 0..columns {
                        values.push(row.get::<_, Value>(index)?);
                    }
                    Ok(values)
                })
                .expect("history rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("decode history rows");
            ((*table).to_owned(), rows)
        })
        .collect()
}

fn assert_no_recovery_leftovers(root: &Path) {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).expect("read fixture directory") {
            let entry = entry.expect("fixture entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.contains(".corrupt-")
                || name.ends_with(".unclean")
                || name.ends_with("-wal")
                || name.ends_with("-shm")
            {
                found.push(path.clone());
            }
            if entry.file_type().expect("entry type").is_dir() {
                walk(&path, found);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    assert!(found.is_empty(), "recovery left files behind: {found:?}");
}

#[test]
fn rebuild_after_deleted_index_matches_cold_scan_and_preserves_history() {
    let started = Instant::now();
    let sandbox = Sandbox::new();
    let mut database = sandbox.open();
    let history_before = seed_durable_history(database.connection());
    sandbox.rebuild(&mut database);
    let baseline = normalized_derived_dump(database.connection()).expect("baseline dump");
    assert_eq!(baseline.items.len(), ITEMS_PER_SCOPE * 3);
    assert_eq!(
        snapshot_history_counts(database.connection()).expect("history counts"),
        history_before
    );
    let history_dump = durable_history_dump(database.connection());

    // A second readable rebuild is idempotent and preserves history contents byte-for-byte.
    sandbox.rebuild(&mut database);
    assert_eq!(
        normalized_derived_dump(database.connection()).expect("second dump"),
        baseline
    );
    assert_eq!(
        snapshot_history_counts(database.connection()).expect("history counts"),
        history_before
    );
    assert_eq!(
        durable_history_dump(database.connection()),
        history_dump,
        "history contents must survive a rebuild"
    );
    drop(database);

    fs::remove_file(sandbox.db_path()).expect("delete index");
    let mut recovered = sandbox.open();
    assert_eq!(
        normalized_derived_dump(recovered.connection())
            .expect("fresh dump")
            .items
            .len(),
        0
    );
    sandbox.rebuild(&mut recovered);
    assert_eq!(
        normalized_derived_dump(recovered.connection()).expect("recovered dump"),
        baseline,
        "a deleted index rebuilds the same derived state as the cold scan"
    );
    drop(recovered);
    assert_no_recovery_leftovers(sandbox.dir.path());
    assert!(
        started.elapsed() < SUITE_BOUND,
        "3k-item rebuild suite took {:?}, above the 60 s bound",
        started.elapsed()
    );
}

#[test]
fn corrupt_index_is_quarantined_and_readable_history_survives_in_the_quarantine() {
    let sandbox = Sandbox::new();
    let mut database = sandbox.open();
    let history_before = seed_durable_history(database.connection());
    sandbox.rebuild(&mut database);
    let baseline = normalized_derived_dump(database.connection()).expect("baseline dump");
    let history_dump = durable_history_dump(database.connection());
    drop(database);

    let corrupted = corrupt_items_page(&sandbox.db_path());
    let mut recovered = sandbox.open();
    let quarantine = recovered
        .startup_warning()
        .expect("startup reports the quarantine")
        .to_path_buf();
    assert!(
        quarantine
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("agentuna.db.corrupt-")),
        "{} must be a timestamped quarantine",
        quarantine.display()
    );
    assert_eq!(
        fs::read(&quarantine).expect("quarantined bytes"),
        corrupted,
        "the damaged file is preserved for inspection"
    );
    assert_eq!(
        recovered.schema_version().expect("fresh schema"),
        at_db::migrations::SHIPPED_MIGRATIONS
            .iter()
            .map(|migration| migration.version())
            .max()
            .expect("at least one migration ships"),
        "a fresh database replaced the damaged file"
    );
    sandbox.rebuild(&mut recovered);
    assert_eq!(
        normalized_derived_dump(recovered.connection()).expect("recovered dump"),
        baseline
    );

    // Durable history is not silently rewritten: whatever is still readable in the preserved
    // file keeps its exact rows.
    let readable = rusqlite::Connection::open_with_flags(
        &quarantine,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("quarantined database opens read-only");
    assert_eq!(
        snapshot_history_counts(&readable).expect("quarantine history counts"),
        history_before
    );
    assert_eq!(
        durable_history_dump(&readable),
        history_dump,
        "readable history rows in the quarantine are unchanged"
    );
    drop(readable);
    drop(recovered);

    // The test consumes the preserved file so the leftover assertion covers the whole home.
    for suffix in ["", "-wal", "-shm"] {
        let mut path = quarantine.as_os_str().to_os_string();
        path.push(suffix);
        match fs::remove_file(PathBuf::from(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not consume the quarantine fixture: {error}"),
        }
    }
    assert_no_recovery_leftovers(sandbox.dir.path());
}

fn corrupt_items_page(db: &Path) -> Vec<u8> {
    let connection = rusqlite::Connection::open(db).expect("raw connection");
    let page_size: i64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("page size");
    let root_page: i64 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_master WHERE name = 'items'",
            [],
            |row| row.get(0),
        )
        .expect("items root page");
    drop(connection);
    let mut bytes = fs::read(db).expect("read database");
    let offset = usize::try_from((root_page - 1) * page_size).expect("page offset");
    let length = usize::try_from(page_size).expect("page size");
    bytes[offset..offset + length].fill(0xFF);
    fs::write(db, &bytes).expect("write corrupt page");
    bytes
}

#[test]
fn killed_rebuild_leaves_previous_state_queryable_and_the_next_rebuild_succeeds() {
    let sandbox = Sandbox::new();
    let mut database = sandbox.open();
    let history_before = seed_durable_history(database.connection());
    sandbox.rebuild(&mut database);
    let baseline = normalized_derived_dump(database.connection()).expect("baseline dump");
    let history_dump = durable_history_dump(database.connection());
    drop(database);

    let status = Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("crash_rebuild_child")
        .arg("--nocapture")
        .env("AT_DB_CRASH_DB", sandbox.db_path())
        .env("AT_DB_CRASH_HOME", &sandbox.home)
        .env("AT_DB_CRASH_USER", &sandbox.user_store)
        .env("AT_DB_CRASH_GLOBAL", &sandbox.global_store)
        .env("AT_DB_CRASH_PROJECT", &sandbox.project)
        .env("AT_DB_CRASH_PROJECT_STORE", &sandbox.project_store)
        .status()
        .expect("spawn crash child");
    assert!(status.success(), "the crash child must run the rebuild");

    let mut recovered = sandbox.open();
    assert_eq!(
        normalized_derived_dump(recovered.connection()).expect("surviving dump"),
        baseline,
        "the killed rebuild must not replace the previous derived state"
    );
    assert_eq!(
        snapshot_history_counts(recovered.connection()).expect("history counts"),
        history_before
    );
    assert_eq!(
        durable_history_dump(recovered.connection()),
        history_dump,
        "history contents survive a killed rebuild"
    );
    sandbox.rebuild(&mut recovered);
    assert_eq!(
        normalized_derived_dump(recovered.connection()).expect("idempotent dump"),
        baseline
    );
    drop(recovered);
    assert_no_recovery_leftovers(sandbox.dir.path());
}

/// Child half of [`killed_rebuild_leaves_previous_state_queryable_and_the_next_rebuild_succeeds`].
#[test]
fn crash_rebuild_child() {
    let Some(db) = std::env::var_os("AT_DB_CRASH_DB") else {
        return;
    };
    let home = std::env::var_os("AT_DB_CRASH_HOME").expect("home");
    let user = std::env::var_os("AT_DB_CRASH_USER").expect("user store");
    let global = std::env::var_os("AT_DB_CRASH_GLOBAL").expect("global store");
    let project = std::env::var_os("AT_DB_CRASH_PROJECT").expect("project");
    let project_store = std::env::var_os("AT_DB_CRASH_PROJECT_STORE").expect("project store");
    let paths = RebuildPaths::new(PathBuf::from(home), PathBuf::from(user))
        .with_global_store(PathBuf::from(global))
        .with_project_and_store(PathBuf::from(project), PathBuf::from(project_store));
    let mut database = Database::open_file(db).expect("child database");
    let observer = |progress: RebuildProgress| {
        if progress.stage == "verify" {
            // Kill the process with the rebuild transaction still open.
            std::process::exit(0);
        }
    };
    let _ = rebuild(database.connection_mut(), &paths, Some(&observer));
    std::process::exit(2);
}

#[test]
fn wal_replay_after_a_killed_handle_preserves_committed_rows() {
    let sandbox = Sandbox::new();
    let database = sandbox.open();
    drop(database);
    let db_path = sandbox.db_path();
    let wal = PathBuf::from(format!("{}-wal", db_path.display()));
    let unclean = db_path.with_extension("unclean");

    let status = Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("wal_child_writer")
        .arg("--nocapture")
        .env("AT_DB_WAL_CHILD_DB", &db_path)
        .status()
        .expect("spawn wal child");
    assert!(status.success(), "the WAL child must commit its rows");
    assert!(
        fs::metadata(&wal).is_ok_and(|metadata| metadata.len() > 0),
        "the killed handle must leave a non-empty WAL"
    );
    assert!(
        unclean.exists(),
        "the killed handle leaves its unclean marker"
    );

    let recovered = sandbox.open();
    let settings: String = recovered
        .connection()
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'wal_child'",
            [],
            |row| row.get(0),
        )
        .expect("committed setting survives WAL replay");
    assert_eq!(settings, "true");
    let operations: i64 = recovered
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE id = '01ARZ3NDEKTSV4RRFFQ69G5FZZ'",
            [],
            |row| row.get(0),
        )
        .expect("committed operation survives WAL replay");
    assert_eq!(operations, 1);
    drop(recovered);
    assert!(!unclean.exists(), "a clean shutdown clears the marker");
    assert!(
        !wal.exists() || fs::metadata(&wal).is_ok_and(|metadata| metadata.len() == 0),
        "a clean shutdown truncates the WAL"
    );
    assert_no_recovery_leftovers(sandbox.dir.path());
}

/// Child half of [`wal_replay_after_a_killed_handle_preserves_committed_rows`].
#[test]
fn wal_child_writer() {
    let Some(db) = std::env::var_os("AT_DB_WAL_CHILD_DB") else {
        return;
    };
    let database = Database::open_file(db).expect("wal child database");
    database
        .connection()
        .execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .expect("disable autocheckpoint");
    database
        .connection()
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('wal_child', 'true')",
            [],
        )
        .expect("committed setting");
    database
        .connection()
        .execute(
            "INSERT INTO operations (id, kind, scope, actor, status, summary, started_at) \
             VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FZZ', 'settings', 'user', 'cli', 'succeeded', 'WAL child', '2026-11-06T10:00:00.000Z')",
            [],
        )
        .expect("committed operation");
    // Exit without dropping the handle: no checkpoint, no clean-shutdown marker removal.
    std::process::exit(0);
}
