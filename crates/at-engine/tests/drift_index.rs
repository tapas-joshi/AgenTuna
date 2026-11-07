//! TC-012: drift targets come from the index, and every classification is persisted.
//!
//! The watcher itself (debounce, self-write suppression) is TF-007's; these tests feed the same
//! `WatchEvent::Batch` shape it produces and assert the engine's classification and the
//! `drift_events` row the UI reads.

use std::fs;
use std::path::Path;

use at_engine::drift::{
    DriftKind, DriftService, DriftTarget, reconcile_indexed, targets_from_index,
};
use at_fs::{WatchBatch, WatchChange};
use at_model::{AgentId, ItemId, ItemState, Scope};

fn seed_deployment(
    connection: &rusqlite::Connection,
    item_id: &ItemId,
    agent: &AgentId,
    target: &Path,
    kind: &str,
    baseline: Option<&str>,
) {
    let target_text = target.to_string_lossy();
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, state, managed) \
             VALUES (?1, ?2, 'user', ?3, ?3, ?4, 'present', 1)",
            [
                item_id.to_string(),
                kind.to_owned(),
                format!("slug-{}", item_id),
                target.display().to_string(),
            ],
        )
        .expect("item row");
    let hashes = at_db::deployments::DeploymentHashes::new(None, baseline, baseline);
    let spec = at_db::deployments::DeploymentSpec::applied(
        item_id,
        agent,
        Scope::User,
        ItemState::Linked,
        Some("native"),
        Some(&target_text),
        hashes,
    );
    at_db::deployments::upsert_deployment(connection, &spec).expect("deployment row");
}

fn batch(path: &Path) -> at_fs::WatchEvent {
    at_fs::WatchEvent::Batch(WatchBatch {
        paths: vec![path.to_path_buf()],
        kinds: vec![WatchChange::Modified(path.to_path_buf())],
        observed_at: time::OffsetDateTime::now_utc(),
    })
}

#[test]
fn an_external_edit_produces_one_unresolved_event_naming_the_item_and_hash() {
    let temp = tempfile::tempdir().expect("sandbox");
    let target = temp.path().join(".claude.json");
    fs::write(&target, b"{\"mcpServers\":{}}").expect("baseline file");
    let baseline = at_fs::sha256_hex(b"{\"mcpServers\":{}}");
    let database = at_db::Database::open_in_memory().expect("database");
    let item_id = ItemId::new();
    seed_deployment(
        database.connection(),
        &item_id,
        &AgentId::ClaudeCode,
        &target,
        "mcp_server",
        Some(&baseline),
    );
    fs::write(&target, b"{\"mcpServers\":{\"server\":{}}}").expect("external edit");

    let targets = targets_from_index(database.connection()).expect("index targets");
    assert_eq!(targets.len(), 1, "one deployment row is one target");
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    let notices = service.process(&batch(&target)).expect("process batch");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, DriftKind::Drifted);
    assert_eq!(notices[0].item_id, Some(item_id));
    let open = at_db::drift::open_drift_events(database.connection(), Some(Scope::User))
        .expect("open drift");
    assert_eq!(open.len(), 1);
    assert!(open[0].is_open());
    assert_eq!(
        open[0].item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );
    assert_eq!(
        open[0].expected_hash.as_deref(),
        Some(baseline.as_str()),
        "the deployment baseline is the expected hash"
    );
    assert_eq!(
        open[0].actual_hash.as_deref(),
        Some(at_fs::sha256_hex(b"{\"mcpServers\":{\"server\":{}}}").as_str()),
        "the changed hash is recorded"
    );
}

#[test]
fn a_write_that_matches_the_baseline_records_nothing() {
    let temp = tempfile::tempdir().expect("sandbox");
    let target = temp.path().join("AGENTS.md");
    fs::write(&target, b"# Rules\n").expect("baseline file");
    let database = at_db::Database::open_in_memory().expect("database");
    seed_deployment(
        database.connection(),
        &ItemId::new(),
        &AgentId::Codex,
        &target,
        "rule",
        Some(&at_fs::sha256_hex(b"# Rules\n")),
    );
    let targets = targets_from_index(database.connection()).expect("index targets");
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    // The watcher would have suppressed an AgenTuna write; even if the batch arrives, an
    // unchanged hash must not become a drift event.
    let notices = service.process(&batch(&target)).expect("process batch");
    assert!(notices.is_empty());
    assert!(
        at_db::drift::open_drift_events(database.connection(), Some(Scope::User))
            .expect("open drift")
            .is_empty()
    );
}

#[test]
fn item_versions_supplies_the_baseline_when_the_deployment_has_no_hash() {
    let temp = tempfile::tempdir().expect("sandbox");
    let target = temp.path().join("mcp.json");
    fs::write(&target, b"original").expect("baseline file");
    let database = at_db::Database::open_in_memory().expect("database");
    let item_id = ItemId::new();
    seed_deployment(
        database.connection(),
        &item_id,
        &AgentId::Omp,
        &target,
        "mcp_server",
        None,
    );
    let version = at_fs::sha256_hex(b"original");
    let target_text = target.to_string_lossy();
    let mut row = at_db::versions::NewVersion::new(&item_id, Scope::User, &version);
    row = row.with_path(target_text.as_ref());
    at_db::versions::record_skill_version(database.connection(), &row).expect("version row");
    fs::write(&target, b"external").expect("external edit");

    let targets = targets_from_index(database.connection()).expect("index targets");
    assert_eq!(targets[0].expected_hash.as_deref(), Some(version.as_str()));
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    let notices = service.process(&batch(&target)).expect("process batch");
    assert_eq!(notices[0].kind, DriftKind::Drifted);
}

#[test]
fn an_unreadable_target_becomes_an_unreadable_event_and_other_targets_keep_working() {
    let temp = tempfile::tempdir().expect("sandbox");
    let unreadable = temp.path().join("directory-as-file");
    fs::create_dir_all(&unreadable).expect("directory");
    let healthy = temp.path().join("healthy.json");
    fs::write(&healthy, b"before").expect("healthy file");
    let database = at_db::Database::open_in_memory().expect("database");
    seed_deployment(
        database.connection(),
        &ItemId::new(),
        &AgentId::ClaudeCode,
        &unreadable,
        "mcp_server",
        Some(&at_fs::sha256_hex(b"before")),
    );
    seed_deployment(
        database.connection(),
        &ItemId::new(),
        &AgentId::Codex,
        &healthy,
        "mcp_server",
        Some(&at_fs::sha256_hex(b"before")),
    );
    fs::write(&healthy, b"after").expect("healthy edit");

    let targets = targets_from_index(database.connection()).expect("index targets");
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    let notices = service.reconcile().expect("reconcile");
    assert_eq!(notices.len(), 2, "both targets are classified");
    assert!(
        notices
            .iter()
            .any(|notice| notice.kind == DriftKind::Unreadable),
        "the directory is reported as unreadable, not silently dropped"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.kind == DriftKind::Drifted),
        "the other target keeps being watched"
    );
    assert_eq!(
        at_db::drift::open_drift_events(database.connection(), Some(Scope::User))
            .expect("open drift")
            .len(),
        2
    );
}

/// The index query skips rows that are not deployed.
#[test]
fn off_rows_are_not_watched() {
    let temp = tempfile::tempdir().expect("sandbox");
    let target = temp.path().join("off.json");
    fs::write(&target, b"x").expect("file");
    let database = at_db::Database::open_in_memory().expect("database");
    let item_id = ItemId::new();
    database
        .connection()
        .execute(
            "INSERT INTO items (id, kind, scope, slug, name, state, managed) \
             VALUES (?1, 'mcp_server', 'user', 'off', 'off', 'present', 1)",
            [item_id.to_string()],
        )
        .expect("item row");
    let target_text = target.to_string_lossy();
    let spec = at_db::deployments::DeploymentSpec::applied(
        &item_id,
        &AgentId::ClaudeCode,
        Scope::User,
        ItemState::Off,
        None,
        Some(target_text.as_ref()),
        at_db::deployments::DeploymentHashes::none(),
    );
    at_db::deployments::upsert_deployment(database.connection(), &spec).expect("deployment row");
    let targets: Vec<DriftTarget> =
        targets_from_index(database.connection()).expect("index targets");
    assert!(targets.is_empty());
}

#[test]
fn reconcile_indexed_detects_an_edit_without_a_watcher_and_never_duplicates_it() {
    let temp = tempfile::tempdir().expect("sandbox");
    let target = temp.path().join(".claude.json");
    fs::write(&target, b"{\"mcpServers\":{}}").expect("baseline file");
    let baseline = at_fs::sha256_hex(b"{\"mcpServers\":{}}");
    let database = at_db::Database::open_in_memory().expect("database");
    let item_id = ItemId::new();
    seed_deployment(
        database.connection(),
        &item_id,
        &AgentId::ClaudeCode,
        &target,
        "mcp_server",
        Some(&baseline),
    );
    fs::write(&target, b"{\"mcpServers\":{\"alpha\":{}}}").expect("external edit");
    let edited = fs::read(&target).expect("edited bytes");

    let first = reconcile_indexed(&database).expect("first reconciliation");
    assert_eq!(first.len(), 1, "one changed path is one notice");
    assert_eq!(first[0].kind, DriftKind::Drifted);
    assert_eq!(
        first[0].actual_hash.as_deref(),
        Some(at_fs::sha256_hex(&edited).as_str())
    );
    let open = at_db::drift::open_drift_events(database.connection(), Some(Scope::User))
        .expect("open events");
    assert_eq!(open.len(), 1, "the scan records the event for the UI");
    assert_eq!(
        open[0].item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );
    assert_eq!(
        fs::read(&target).expect("target bytes"),
        edited,
        "detection must not write the agent file"
    );

    let second = reconcile_indexed(&database).expect("second reconciliation");
    assert!(second.is_empty(), "an open event must not be duplicated");
    assert_eq!(
        at_db::drift::open_drift_events(database.connection(), Some(Scope::User))
            .expect("open events")
            .len(),
        1
    );
}
