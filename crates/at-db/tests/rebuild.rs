//! Integration tests for TF-019: DB rebuild and reindex from files.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use at_db::rebuild::{
    DiscoveredDeployment, DiscoveredItem, HistoryCounts, NormalizedDump, RebuildInput,
    RebuildObserver, RebuildPaths, RebuildProgress, normalized_derived_dump, rebuild,
    rebuild_with_input, snapshot_history_counts,
};
use at_db::{Database, Migrations};
use at_model::{AgentId, ItemId, ItemKind, ItemState, Scope, SyncLabel};
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

mod common;

fn open_test_db() -> Database {
    let migrations = Migrations::shipped().expect("valid migrations");
    Database::open_in_memory_with(migrations).expect("in-memory db opens")
}

fn open_file_db(path: &Path) -> Database {
    let migrations = Migrations::shipped().expect("valid migrations");
    Database::open_file_with(path, migrations).expect("file db opens")
}

/// Helper to seed durable history tables that must survive rebuilds.
fn seed_durable_history(conn: &rusqlite::Connection) -> HistoryCounts {
    // 1. Operations
    for i in 1..=5 {
        conn.execute(
            "INSERT INTO operations (id, kind, scope, actor, status, summary, started_at) \
             VALUES (?1, 'deploy', 'user', 'cli', 'succeeded', 'Deploy op', '2026-11-05T12:00:00.000Z')",
            params![format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:02}")],
        )
        .expect("insert op");
    }

    // 2. Operation files
    for i in 1..=10 {
        let op_id = format!("01ARZ3NDEKTSV4RRFFQ69G5F{:02}", (i % 5) + 1);
        conn.execute(
            "INSERT INTO operation_files (operation_id, path, action, before_hash, after_hash) \
             VALUES (?1, ?2, 'modified', 'hash-before', 'hash-after')",
            params![op_id, format!("/path/file{i}.txt")],
        )
        .expect("insert op file");
    }

    // 3. Backups
    for i in 1..=4 {
        conn.execute(
            "INSERT INTO backups (id, operation_id, original_path, backup_path, size) \
             VALUES (?1, '01ARZ3NDEKTSV4RRFFQ69G5F01', ?2, ?3, 1024)",
            params![
                format!("bkp_{i}"),
                format!("/original/{i}"),
                format!("/backup/{i}.tar.gz")
            ],
        )
        .expect("insert backup");
    }

    // 4. Health checks
    for i in 1..=8 {
        conn.execute(
            "INSERT INTO health_checks (scope, server_name, checked_at, status, trigger) \
             VALUES ('user', ?1, ?2, 'ok', 'manual')",
            params![format!("server-{i}"), 1730800000 + (i as i64) * 1000],
        )
        .expect("insert health check");
    }

    // 5. Drift events
    for i in 1..=3 {
        conn.execute(
            "INSERT INTO drift_events (agent_id, scope, path, expected_hash, actual_hash) \
             VALUES ('claude_code', 'user', ?1, 'hash-a', 'hash-b')",
            params![format!("/drift/path/{i}")],
        )
        .expect("insert drift");
    }

    // 6. Preset applications & members
    for i in 1..=2 {
        conn.execute(
            "INSERT INTO preset_applications (id, preset_id, target_kind, target_id, member_count) \
             VALUES (?1, 'prs_1', 'agent', 'claude_code', 2)",
            params![i],
        )
        .expect("insert preset application");

        conn.execute(
            "INSERT INTO preset_application_members (application_id, item_id, prior_enabled) \
             VALUES (?1, ?2, 1)",
            params![i, format!("itm_member_{i}")],
        )
        .expect("insert preset application member");
    }

    // 7. Settings (custom preference rows)
    conn.execute(
        "INSERT INTO settings (key, value_json) VALUES ('theme', '\"dark\"')",
        [],
    )
    .expect("insert setting theme");
    conn.execute(
        "INSERT INTO settings (key, value_json) VALUES ('telemetry_enabled', 'false')",
        [],
    )
    .expect("insert setting telemetry");

    snapshot_history_counts(conn).expect("snapshot history")
}

fn durable_history_dump(
    connection: &rusqlite::Connection,
) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
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
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .expect("prepare durable table snapshot");
            let column_count = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..column_count)
                        .map(|index| row.get(index))
                        .collect::<Result<Vec<rusqlite::types::Value>, _>>()
                })
                .expect("query durable table")
                .collect::<Result<Vec<_>, _>>()
                .expect("read durable table");
            ((*table).to_owned(), rows)
        })
        .collect()
}

/// Creates a populated filesystem fixture on disk with 3 scopes (global, user, project),
/// 40 items across them, and 3 agent directories (Claude Code, Codex, omp).
fn setup_fixture_tree(temp: &TempDir) -> RebuildPaths {
    let root = temp.path();
    let home = root.join("home");
    let global_store = root.join("global_store");
    let user_store = home.join(".agentuna");
    let project_dir = root.join("project");
    let project_store = project_dir.join(".agentuna");

    fs::create_dir_all(&home).expect("mkdir home");
    fs::create_dir_all(&global_store).expect("mkdir global_store");
    fs::create_dir_all(&user_store).expect("mkdir user_store");
    fs::create_dir_all(&project_store).expect("mkdir project_store");

    // Agent home folders
    fs::create_dir_all(home.join(".claude/skills")).expect("mkdir claude");
    fs::write(home.join(".claude.json"), br#"{"mcpServers":{}}"#).expect("write claude.json");
    fs::write(home.join(".claude/CLAUDE.md"), b"# Claude instructions").expect("write claude.md");

    fs::create_dir_all(home.join(".agents/skills")).expect("mkdir codex");
    fs::create_dir_all(home.join(".codex")).expect("mkdir codex dir");
    fs::write(home.join(".codex/config.toml"), b"[mcp_servers]").expect("write codex config");
    fs::write(home.join(".codex/AGENTS.md"), b"# Codex instructions")
        .expect("write codex agents.md");

    fs::create_dir_all(home.join(".omp/agent/managed-skills")).expect("mkdir omp");
    fs::write(home.join(".omp/agent/mcp.json"), br#"{"servers":{}}"#).expect("write omp mcp");
    fs::write(home.join(".omp/agent/AGENTS.md"), b"# Omp instructions")
        .expect("write omp agents.md");

    // Generate 10 Global items, 15 User items, 15 Project items = 40 items
    create_scope_items(&global_store, Scope::Global, 1..=10, None);
    create_scope_items(&user_store, Scope::User, 11..=25, None);
    create_scope_items(&project_store, Scope::Project, 26..=40, Some("prj_1"));

    // Also deploy 3 skills into agents via symlinks
    let user_skill_11 = user_store.join("skills/skill-11");
    let claude_deploy = home.join(".claude/skills/skill-11");
    let _ = symlink(&user_skill_11, &claude_deploy);

    let user_skill_12 = user_store.join("skills/skill-12");
    let codex_deploy = home.join(".agents/skills/skill-12");
    let _ = symlink(&user_skill_12, &codex_deploy);

    let proj_skill_26 = project_store.join("skills/skill-26");
    let omp_deploy = home.join(".omp/agent/managed-skills/skill-26");
    let _ = symlink(&proj_skill_26, &omp_deploy);

    RebuildPaths::new(&home, &user_store)
        .with_global_store(&global_store)
        .with_project_and_store(&project_dir, &project_store)
}

fn create_scope_items(
    store_dir: &Path,
    scope: Scope,
    range: std::ops::RangeInclusive<usize>,
    _proj_id: Option<&str>,
) {
    let mut items = Vec::new();
    let mut generated_ids = Vec::new();
    let skills_dir = store_dir.join("skills");
    let rules_dir = store_dir.join("rules");
    fs::create_dir_all(&skills_dir).expect("mkdir skills");
    fs::create_dir_all(&rules_dir).expect("mkdir rules");

    for i in range {
        let item_id = ItemId::new().to_string();
        generated_ids.push(item_id.clone());
        let slug = format!("skill-{i}");
        let skill_dir = skills_dir.join(&slug);
        fs::create_dir_all(&skill_dir).expect("mkdir skill");
        let skill_doc = skill_dir.join("SKILL.md");
        fs::write(
            &skill_doc,
            format!("---\nname: {slug}\ndescription: Test skill {i}\n---\nBody {i}"),
        )
        .expect("write skill");

        let rel_path = format!("skills/{slug}");
        items.push(json!({
            "id": item_id,
            "kind": "skill",
            "scope": scope.wire_name(),
            "slug": slug,
            "name": format!("Skill {i}"),
            "path": rel_path,
            "enabled_default": true,
            "tags": ["test", "fixture"],
            "source": {
                "kind": "folder",
                "url": format!("file:///sources/{slug}")
            }
        }));
    }
    let manifest = json!({
        "schema_version": 1,
        "store_id": "00000000-0000-0000-0000-000000000001",
        "scope": scope.wire_name(),
        "items": items
    });

    fs::write(
        store_dir.join("store.json"),
        serde_json::to_string_pretty(&manifest).expect("json"),
    )
    .expect("write store.json");

    // Add presets.json in user store
    if scope == Scope::User {
        let presets = json!({
            "presets": [
                {
                    "id": "prs_default",
                    "name": "Default Preset",
                    "description": "Standard tools",
                    "scope": "user",
                    "is_builtin": true,
                    "members": [
                        { "item_id": generated_ids.first().cloned().unwrap(), "kind": "skill", "enabled": true },
                        { "item_id": generated_ids.get(1).cloned().unwrap(), "kind": "skill", "enabled": false }
                    ]
                }
            ]
        });
        fs::write(
            store_dir.join("presets.json"),
            serde_json::to_string_pretty(&presets).expect("json"),
        )
        .expect("write presets.json");
    }
}

// =========================================================================
// Acceptance Test 1 & 2: 3 scopes, 40 items, 3 agents, grown history
// =========================================================================

#[test]
fn fixture_with_3_scopes_40_items_3_agents_grown_history_rebuilds_identically() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);

    let mut db = open_test_db();
    let conn = db.connection_mut();

    // 1. Seed durable history tables before any rebuild.
    let initial_history = seed_durable_history(conn);

    // 2. Perform initial cold scan rebuild to establish baseline.
    let report1 = rebuild(conn, &paths, None).expect("initial rebuild");
    assert_eq!(report1.counts.items, 40, "all 40 items must be indexed");
    assert!(report1.counts.agents >= 3, "at least 3 agents detected");
    assert_eq!(report1.counts.deployments, 3, "3 symlinked deployments");

    // Take normalized baseline dump.
    let baseline_dump = normalized_derived_dump(conn).expect("baseline dump");

    // Verify history counts remained identical after first rebuild.
    let history_after_first = snapshot_history_counts(conn).expect("history after first");
    assert_eq!(
        initial_history, history_after_first,
        "history tables must not change during rebuild"
    );

    // 3. Clear all derived tables to empty state (simulating index recreation).
    conn.execute_batch(
        "DELETE FROM deployments;
         DELETE FROM item_versions;
         DELETE FROM preset_members;
         DELETE FROM presets;
         DELETE FROM secret_ref_uses;
         DELETE FROM secret_refs;
         DELETE FROM locks;
         DELETE FROM native_items;
         UPDATE items SET source_id = NULL;
         DELETE FROM sources;
         DELETE FROM items;
         DELETE FROM agent_paths;
         DELETE FROM agents;
         DELETE FROM projects;",
    )
    .expect("wipe derived tables");

    // 4. Rebuild from empty derived state.
    let report2 = rebuild(conn, &paths, None).expect("second rebuild from empty state");
    assert_eq!(report2.counts.items, 40);

    // 5. Verify rebuilt normalized dump is identical to the baseline.
    let rebuilt_dump = normalized_derived_dump(conn).expect("rebuilt dump");
    assert_eq!(
        baseline_dump, rebuilt_dump,
        "normalized dump after rebuild must match baseline cold scan"
    );

    // 6. Verify history counts are still identical.
    let history_after_second = snapshot_history_counts(conn).expect("history after second");
    assert_eq!(
        initial_history, history_after_second,
        "history row counts must be preserved"
    );
}

// =========================================================================
// Acceptance Test 5: Rebuild is idempotent
// =========================================================================

#[test]
fn rebuild_is_idempotent_and_produces_no_duplicate_rows() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);

    let mut db = open_test_db();
    let conn = db.connection_mut();
    seed_durable_history(conn);

    // Run 1
    rebuild(conn, &paths, None).expect("run 1");
    let dump1 = normalized_derived_dump(conn).expect("dump 1");

    // Run 2 (consecutive run on same db)
    rebuild(conn, &paths, None).expect("run 2");
    let dump2 = normalized_derived_dump(conn).expect("dump 2");
    assert_eq!(dump1.projects, dump2.projects, "projects diff");
    assert_eq!(dump1.agents, dump2.agents, "agents diff");
    assert_eq!(dump1.agent_paths, dump2.agent_paths, "agent_paths diff");
    if dump1.items != dump2.items {
        for (i, (a, b)) in dump1.items.iter().zip(dump2.items.iter()).enumerate() {
            if a != b {
                panic!("item {i} differs:\nleft:  {a:?}\nright: {b:?}");
            }
        }
        if dump1.items.len() != dump2.items.len() {
            panic!(
                "len differs: {} vs {}",
                dump1.items.len(),
                dump2.items.len()
            );
        }
    }
    assert_eq!(
        dump1.item_versions, dump2.item_versions,
        "item_versions diff"
    );
    assert_eq!(dump1.native_items, dump2.native_items, "native_items diff");
    assert_eq!(dump1.deployments, dump2.deployments, "deployments diff");
    assert_eq!(dump1.locks, dump2.locks, "locks diff");
    assert_eq!(dump1.sources, dump2.sources, "sources diff");
    assert_eq!(dump1.presets, dump2.presets, "presets diff");
    assert_eq!(
        dump1.preset_members, dump2.preset_members,
        "preset_members diff"
    );
    assert_eq!(dump1.secret_refs, dump2.secret_refs, "secret_refs diff");
    assert_eq!(
        dump1.secret_ref_uses, dump2.secret_ref_uses,
        "secret_ref_uses diff"
    );
    // Assert no duplicate items or agents exist
    let item_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .expect("count items");
    assert_eq!(item_count, 40);
}

// =========================================================================
// Acceptance Test 5b: Aborted rebuild leaves previous derived data intact
// =========================================================================

struct CancellingObserver {
    cancelled_after_step: AtomicUsize,
    step_counter: AtomicUsize,
}

impl RebuildObserver for CancellingObserver {
    fn on_progress(&self, _progress: RebuildProgress) {
        self.step_counter.fetch_add(1, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.step_counter.load(Ordering::SeqCst) >= self.cancelled_after_step.load(Ordering::SeqCst)
    }
}

#[test]
fn aborted_rebuild_leaves_previous_derived_data_intact() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);

    let mut db = open_test_db();
    let conn = db.connection_mut();

    // 1. Run successful initial rebuild
    rebuild(conn, &paths, None).expect("initial rebuild");
    let original_dump = normalized_derived_dump(conn).expect("original dump");
    assert_eq!(original_dump.items.len(), 40);

    // 2. Run rebuild with an observer that cancels mid-flight
    let observer = CancellingObserver {
        cancelled_after_step: AtomicUsize::new(2),
        step_counter: AtomicUsize::new(0),
    };

    let err = rebuild(conn, &paths, Some(&observer)).expect_err("must be cancelled");
    assert!(
        matches!(err, at_db::rebuild::RebuildError::Cancelled),
        "error must be Cancelled"
    );

    // 3. Verify previous derived data is completely intact!
    let dump_after_cancel = normalized_derived_dump(conn).expect("dump after cancel");
    assert_eq!(
        original_dump, dump_after_cancel,
        "aborted rebuild must leave previous derived data intact"
    );
}

// =========================================================================
// Acceptance Test 4: Deleted file disappears, new native SKILL.md appears as status=new
// =========================================================================

#[test]
fn deleted_file_disappears_and_new_native_skill_appears_as_new() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);

    let mut db = open_test_db();
    let conn = db.connection_mut();

    // Initial rebuild
    rebuild(conn, &paths, None).expect("initial rebuild");

    // Verify skill-11 exists in items and deployments
    let item11: Option<String> = conn
        .query_row("SELECT id FROM items WHERE slug = 'skill-11'", [], |r| {
            r.get(0)
        })
        .expect("query item11");
    assert!(item11.is_some());

    // 1. Delete skill-11 from user store on disk and manifest
    let skill11_dir = paths.user_store.join("skills/skill-11");
    let _ = fs::remove_dir_all(&skill11_dir);

    // Also remove deployment symlink from Claude
    let claude_symlink = paths.home.join(".claude/skills/skill-11");
    let _ = fs::remove_file(&claude_symlink);

    // Update store.json to remove skill-11
    let manifest_file = paths.user_store.join("store.json");
    let mut manifest_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_file).expect("read")).expect("json");
    if let Some(items) = manifest_val.get_mut("items").and_then(|v| v.as_array_mut()) {
        items.retain(|it| it.get("slug").and_then(|s| s.as_str()) != Some("skill-11"));
    }
    fs::write(
        &manifest_file,
        serde_json::to_string_pretty(&manifest_val).expect("json"),
    )
    .expect("write updated manifest");

    // 2. Create a new native unmanaged SKILL.md directly inside ~/.claude/skills
    let native_skill_dir = paths.home.join(".claude/skills/my-native-skill");
    fs::create_dir_all(&native_skill_dir).expect("mkdir native skill");
    fs::write(
        native_skill_dir.join("SKILL.md"),
        b"---\nname: my-native-skill\ndescription: A native skill\n---\nNative content",
    )
    .expect("write native skill");

    // 3. Rebuild
    rebuild(conn, &paths, None).expect("rebuild after file changes");

    // 4. Assert skill-11 disappeared from items and deployments
    use rusqlite::OptionalExtension;
    let item11_after: Option<String> = conn
        .query_row("SELECT id FROM items WHERE slug = 'skill-11'", [], |r| {
            r.get(0)
        })
        .optional()
        .expect("query item11 after");
    assert!(
        item11_after.is_none(),
        "deleted skill must disappear from items"
    );

    let dep_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deployments WHERE target_path LIKE '%skill-11'",
            [],
            |r| r.get(0),
        )
        .expect("query dep after");
    assert_eq!(dep_after, 0, "deleted skill deployment must disappear");

    // 5. Assert new native SKILL.md appears in native_items with status='new'
    let native_status: Option<String> = conn
        .query_row(
            "SELECT status FROM native_items WHERE name = 'my-native-skill'",
            [],
            |r| r.get(0),
        )
        .expect("query native status");
    assert_eq!(
        native_status,
        Some("new".to_string()),
        "new native skill must appear with status=new"
    );
}

// =========================================================================
// Acceptance Test 6: Corrupted / deleted index file recovery
// =========================================================================

#[test]
fn deleting_db_file_and_rebuilding_reproduces_derived_rows() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);
    let db_path = temp.path().join("agentuna.db");

    let baseline_dump: NormalizedDump;
    {
        let mut db = open_file_db(&db_path);
        let conn = db.connection_mut();
        rebuild(conn, &paths, None).expect("initial rebuild");
        baseline_dump = normalized_derived_dump(conn).expect("baseline dump");
    }

    // Delete the database file entirely (simulating index loss or quarantine)
    fs::remove_file(&db_path).expect("remove db file");
    assert!(!db_path.exists());

    // Create fresh database at same path
    {
        let mut db = open_file_db(&db_path);
        let conn = db.connection_mut();

        // Fresh database has empty derived tables
        let empty_dump = normalized_derived_dump(conn).expect("empty dump");
        assert_eq!(empty_dump.items.len(), 0);

        // Run rebuild
        let report = rebuild(conn, &paths, None).expect("rebuild on fresh db");
        assert_eq!(report.counts.items, 40);

        let recovered_dump = normalized_derived_dump(conn).expect("recovered dump");
        assert_eq!(
            baseline_dump, recovered_dump,
            "recovered dump after db deletion must match original baseline"
        );
    }
}

#[test]
fn corrupt_database_is_quarantined_then_derived_rows_rebuild_from_store() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);
    let db_path = temp.path().join("agentuna.db");

    {
        let mut database = open_file_db(&db_path);
        rebuild(database.connection_mut(), &paths, None).expect("initial rebuild");
    }

    let corrupt_bytes = b"invalid sqlite bytes";
    fs::write(&db_path, corrupt_bytes).expect("corrupt db fixture");
    let mut recovered = open_file_db(&db_path);
    let quarantine = recovered
        .startup_warning()
        .expect("startup reports quarantined file");
    assert_eq!(
        fs::read(quarantine).expect("read quarantined file"),
        corrupt_bytes
    );
    assert!(quarantine.exists());
    assert!(
        quarantine
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("agentuna.db.corrupt-"))
    );

    let report = rebuild(recovered.connection_mut(), &paths, None).expect("rebuild");
    assert_eq!(report.counts.items, 40);
    assert_eq!(
        normalized_derived_dump(recovered.connection())
            .expect("recovered dump")
            .items
            .len(),
        40
    );
}

#[test]
fn readable_database_rebuild_preserves_durable_history_contents() {
    let temp = TempDir::new().expect("tempdir");
    let paths = setup_fixture_tree(&temp);
    let mut database = open_file_db(&temp.path().join("agentuna.db"));
    let connection = database.connection_mut();
    seed_durable_history(connection);
    rebuild(connection, &paths, None).expect("initial rebuild");
    let baseline = normalized_derived_dump(connection).expect("baseline dump");
    let history_before = durable_history_dump(connection);

    connection
        .execute("DELETE FROM items", [])
        .expect("simulate damaged derived rows");
    rebuild(connection, &paths, None).expect("rebuild damaged readable db");

    assert_eq!(
        normalized_derived_dump(connection).expect("rebuilt dump"),
        baseline
    );
    assert_eq!(durable_history_dump(connection), history_before);
}

// =========================================================================
// Acceptance Test 3: Rebuilding 3k items completes in under 2 seconds
// =========================================================================

#[test]
fn rebuilding_3k_items_completes_under_2_seconds_with_progress() {
    let mut db = open_test_db();
    let conn = db.connection_mut();

    let mut input = RebuildInput::default();
    input.agents.push(at_db::rebuild::DiscoveredAgent {
        id: AgentId::ClaudeCode,
        display_name: "Claude Code".to_string(),
        version: None,
        installed: true,
        managed: true,
        settings_json: None,
    });

    for i in 1..=3000 {
        let id_parsed = ItemId::new();
        input.items.push(DiscoveredItem {
            id: id_parsed,
            kind: ItemKind::Skill,
            scope: Scope::User,
            project_id: None,
            slug: format!("skill-{i}"),
            name: format!("Skill {i}"),
            source_path: Some(format!("skills/skill-{i}")),
            link_target: None,
            content_hash: Some(format!("hash_{i:064x}")),
            state: Some("active".to_string()),
            managed: true,
            source_id: None,
            enabled_default: Some(true),
            tags_json: None,
        });

        input.item_versions.push(at_db::rebuild::DiscoveredVersion {
            item_id: id_parsed,
            scope: Scope::User,
            content_hash: format!("hash_{i:064x}"),
            path: Some(format!("skills/skill-{i}")),
            size: Some(128),
            mtime: Some(1730800000),
            version_label: None,
            operation_id: None,
        });
    }

    struct ProgressRecorder {
        events: Arc<std::sync::Mutex<Vec<RebuildProgress>>>,
    }

    impl RebuildObserver for ProgressRecorder {
        fn on_progress(&self, progress: RebuildProgress) {
            self.events.lock().unwrap().push(progress);
        }
    }

    let recorder = ProgressRecorder {
        events: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    let start = Instant::now();
    let report = rebuild_with_input(conn, &input, Some(&recorder)).expect("rebuild 3k items");
    let elapsed = start.elapsed();

    assert_eq!(report.counts.items, 3000);
    assert_eq!(report.counts.item_versions, 3000);
    // 2 s is the shipped (release) budget; a debug build under load needs more room.
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(2)
    };
    assert!(
        elapsed < budget,
        "rebuild of 3k items must complete under {budget:?} (took {elapsed:?})"
    );

    let events = recorder.events.lock().unwrap().clone();
    assert!(!events.is_empty(), "progress events must be emitted");
    assert!(events.iter().any(|e| e.stage == "insert"));
    assert!(events.iter().any(|e| e.stage == "done"));
}

// =========================================================================
// Invariant Verification Tests
// =========================================================================

#[test]
fn invariant_check_rejects_linked_deployment_to_missing_item() {
    let mut db = open_test_db();
    let conn = db.connection_mut();

    let mut input = RebuildInput::default();
    let dangling_item_id = ItemId::new();

    input.deployments.push(DiscoveredDeployment {
        item_id: dangling_item_id,
        agent_id: AgentId::ClaudeCode,
        scope: Scope::User,
        project_id: None,
        state: ItemState::Linked,
        mode: Some("symlink".to_string()),
        target_path: Some("/target/path".to_string()),
        link_target: Some("/link/target".to_string()),
        store_hash: None,
        target_hash: None,
        baseline_hash: None,
        sync_label: Some(SyncLabel::InSync),
        operation_id: None,
    });

    let err = rebuild_with_input(conn, &input, None).expect_err("must fail invariant check");
    assert!(
        matches!(err, at_db::rebuild::RebuildError::Invariant(..)),
        "error must be an invariant failure: {err}"
    );
}
