//! TA-094: safe rule rendering, composition drift and marker-failure regressions.
//!
//! Files and SQLite state stay under a canonical temporary root. These tests exercise the
//! reviewed ChangeSet path rather than calling the filesystem writer directly.

use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use at_db::Database;
use at_db::deployments::{DeploymentSpec, upsert_deployment};
use at_engine::drift::{DriftKind, DriftService, targets_from_index};
use at_engine::rules::{
    MANAGED_BEGIN, MANAGED_END, ManagedBlockError, RuleRenderError, RuleRenderTarget,
    apply_rule_render_plan, plan_rule_render,
};
use at_fs::{ApplyContext, ApplyRoots, DbOperationRecorder, WatchBatch, WatchChange, WatchEvent};
use at_model::{AgentId, ItemId, ItemState, Scope};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const PROJECT_ID: &str = "fixture-project";
const INITIAL_RULE: &[u8] = b"# Shared instructions\n";
const UPDATED_RULE: &[u8] = b"# Updated instructions\n";

#[test]
fn paired_render_records_one_operation_and_noop_preserves_file_bytes_and_mtime()
-> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let database = Database::open_file(root.join("agentuna.sqlite3"))?;
    let item_id = insert_rule_item(&database, Scope::Project)?;
    let claude = root.join("CLAUDE.md");
    let agents = root.join("AGENTS.md");
    // The two native files keep different user-owned text, so their versions have distinct
    // hashes; a byte-identical pair collapses to one version row under the schema's
    // `(item_id, content_hash)` uniqueness.
    let seeded =
        b"# Claude header\n<!-- agentuna:begin -->\nold body\n<!-- agentuna:end -->\n".to_vec();
    fs::write(&claude, &seeded)?;
    let plan = plan_rule_render(
        INITIAL_RULE,
        [target(&claude, Some(seeded)), target(&agents, None)],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )?;
    let first = apply_render(&plan, &item_id, &database, &root)?
        .ok_or_else(|| io::Error::other("initial paired render made no ChangeSet"))?;

    let claude_bytes = fs::read(&claude)?;
    let agents_bytes = fs::read(&agents)?;
    assert_eq!(managed_body(&claude_bytes)?, managed_body(&agents_bytes)?);
    assert_eq!(marker_count(&claude_bytes, MANAGED_BEGIN), 1);
    assert_eq!(marker_count(&claude_bytes, MANAGED_END), 1);
    assert_eq!(marker_count(&agents_bytes, MANAGED_BEGIN), 1);
    assert_eq!(marker_count(&agents_bytes, MANAGED_END), 1);

    let mtimes = [modified(&claude)?, modified(&agents)?];
    assert_eq!(count_rows(database.connection(), "operations")?, 1);
    assert_eq!(
        operation_file_count(database.connection(), &first.operation_id)?,
        2
    );
    let versions = version_rows(database.connection(), &item_id)?;
    assert_eq!(versions.len(), 2, "{versions:?}");
    let expected_versions = vec![
        (
            at_fs::sha256_hex(&agents_bytes),
            agents.to_string_lossy().into_owned(),
            first.operation_id.clone(),
        ),
        (
            at_fs::sha256_hex(&claude_bytes),
            claude.to_string_lossy().into_owned(),
            first.operation_id.clone(),
        ),
    ];
    assert_eq!(versions, expected_versions);

    let second_plan = plan_rule_render(
        INITIAL_RULE,
        [
            target(&claude, Some(claude_bytes.clone())),
            target(&agents, Some(agents_bytes.clone())),
        ],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )?;
    assert!(second_plan.change_set.is_none());
    assert!(apply_render(&second_plan, &item_id, &database, &root)?.is_none());
    assert_eq!(fs::read(&claude)?, claude_bytes);
    assert_eq!(fs::read(&agents)?, agents_bytes);
    assert_eq!([modified(&claude)?, modified(&agents)?], mtimes);
    assert_eq!(count_rows(database.connection(), "operations")?, 1);
    assert_eq!(
        operation_file_count(database.connection(), &first.operation_id)?,
        2
    );
    assert_eq!(
        version_rows(database.connection(), &item_id)?,
        expected_versions,
        "a no-op render adds no version rows"
    );
    Ok(())
}

#[test]
fn invalid_utf8_target_is_a_parse_error_and_is_never_rewritten() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let claude = root.join("CLAUDE.md");
    let agents = root.join("AGENTS.md");
    let invalid = b"# header\n\xff\xfe not utf-8\n";
    fs::write(&agents, invalid)?;
    let before = at_fs::sha256_hex(invalid);

    let error = plan_rule_render(
        UPDATED_RULE,
        [
            target(&claude, None),
            target(&agents, Some(invalid.to_vec())),
        ],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )
    .expect_err("invalid UTF-8 must reject the paired plan");
    assert!(matches!(
        &error,
        RuleRenderError::Parse { path } if path == &agents
    ));
    assert!(!claude.exists(), "the paired plan writes neither file");
    assert_eq!(at_fs::sha256_hex(&fs::read(&agents)?), before);
    Ok(())
}

#[test]
fn malformed_second_target_refuses_pair_and_leaves_both_hashes_unchanged()
-> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let claude = root.join("CLAUDE.md");
    let agents = root.join("AGENTS.md");
    let valid = b"<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\n";
    let malformed = b"header\n<!-- agentuna:begin -->\nbody\n";
    fs::write(&claude, valid)?;
    fs::write(&agents, malformed)?;
    let before = [at_fs::sha256_hex(valid), at_fs::sha256_hex(malformed)];

    let error = plan_rule_render(
        UPDATED_RULE,
        [
            target(&claude, Some(valid.to_vec())),
            target(&agents, Some(malformed.to_vec())),
        ],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )
    .expect_err("malformed second file must reject the paired plan");
    assert!(matches!(
        error,
        RuleRenderError::ManagedBlock(ManagedBlockError::MalformedManagedBlock { path, line: 2 })
            if path == agents
    ));
    assert_eq!(fs::read(&claude)?, valid);
    assert_eq!(fs::read(&agents)?, malformed);
    assert_eq!(
        [
            at_fs::sha256_hex(&fs::read(&claude)?),
            at_fs::sha256_hex(&fs::read(&agents)?)
        ],
        before
    );
    Ok(())
}

#[test]
fn drifted_outside_edit_is_reported_and_survives_the_next_render() -> Result<(), Box<dyn Error>> {
    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?;
    let database = Database::open_file(root.join("agentuna.sqlite3"))?;
    let item_id = insert_rule_item(&database, Scope::Project)?;
    let claude = root.join("CLAUDE.md");
    let agents = root.join("AGENTS.md");

    let initial_plan = plan_rule_render(
        INITIAL_RULE,
        [target(&claude, None), target(&agents, None)],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )?;
    apply_render(&initial_plan, &item_id, &database, &root)?
        .ok_or_else(|| io::Error::other("initial paired render made no ChangeSet"))?;
    let baseline_bytes = fs::read(&claude)?;
    let baseline_hash = at_store::hash_rule_bytes(&baseline_bytes).to_string();
    seed_rule_deployment(&database, &item_id, &claude, &baseline_hash)?;

    let external_bytes = [b"# External header\n".as_slice(), baseline_bytes.as_slice()].concat();
    fs::write(&claude, &external_bytes)?;
    let targets = targets_from_index(database.connection())?;
    let service = DriftService {
        database: &database,
        targets: &targets,
    };
    let notices = service.process(&modified_event(&claude))?;
    assert_eq!(notices.len(), 1, "one tracked rule path drifted");
    assert_eq!(notices[0].kind, DriftKind::Drifted);
    assert_eq!(notices[0].path, claude);
    assert_eq!(
        notices[0].expected_hash.as_deref(),
        Some(baseline_hash.as_str())
    );
    assert_eq!(
        notices[0].actual_hash.as_deref(),
        Some(
            at_store::hash_rule_bytes(&external_bytes)
                .to_string()
                .as_str()
        )
    );

    let current_claude = fs::read(&claude)?;
    let current_agents = fs::read(&agents)?;
    let next_plan = plan_rule_render(
        UPDATED_RULE,
        [
            target(&claude, Some(current_claude)),
            target(&agents, Some(current_agents)),
        ],
        Scope::Project,
        Some(PROJECT_ID.to_owned()),
    )?;
    apply_render(&next_plan, &item_id, &database, &root)?
        .ok_or_else(|| io::Error::other("updated source should change both managed blocks"))?;
    let rendered_claude = fs::read(&claude)?;
    assert!(rendered_claude.starts_with(b"# External header\n"));
    assert_eq!(
        managed_body(&rendered_claude)?,
        b"\n# Updated instructions\n"
    );
    assert_eq!(
        at_db::drift::open_drift_events(database.connection(), Some(Scope::Project))?.len(),
        1,
        "the external edit remains in the drift history after rendering"
    );
    Ok(())
}

fn insert_rule_item(database: &Database, scope: Scope) -> Result<ItemId, Box<dyn Error>> {
    let item_id = ItemId::new();
    database.connection().execute(
        "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
         VALUES (?1, 'rule', ?2, ?3, 'instructions', 'instructions', 'present', 1)",
        params![item_id.to_string(), scope.wire_name(), PROJECT_ID],
    )?;
    Ok(item_id)
}

fn seed_rule_deployment(
    database: &Database,
    item_id: &ItemId,
    path: &Path,
    baseline_hash: &str,
) -> Result<(), Box<dyn Error>> {
    let target_path = path.to_string_lossy().into_owned();
    let hashes =
        at_db::deployments::DeploymentHashes::new(None, Some(baseline_hash), Some(baseline_hash));
    let spec = DeploymentSpec::applied(
        item_id,
        &AgentId::ClaudeCode,
        Scope::Project,
        ItemState::Linked,
        Some("native"),
        Some(&target_path),
        hashes,
    )
    .with_project(PROJECT_ID);
    upsert_deployment(database.connection(), &spec)?;
    Ok(())
}

fn target(path: &Path, existing: Option<Vec<u8>>) -> RuleRenderTarget {
    RuleRenderTarget {
        path: path.to_path_buf(),
        existing,
    }
}

fn apply_render(
    plan: &at_engine::rules::RuleRenderPlan,
    item_id: &ItemId,
    database: &Database,
    root: &Path,
) -> Result<Option<at_fs::ApplyOutcome>, Box<dyn Error>> {
    let backup_folder = root.join(".agentuna/backups");
    fs::create_dir_all(&backup_folder)?;
    let roots = ApplyRoots::new([root.to_path_buf()])?;
    let mut recorder = DbOperationRecorder::new(database.connection(), 30);
    let mut context = ApplyContext {
        store_root: root,
        backup_folder: &backup_folder,
        recorder: &mut recorder,
    };
    Ok(apply_rule_render_plan(
        plan,
        item_id,
        database.connection(),
        &mut context,
        &roots,
    )?)
}

fn managed_body(bytes: &[u8]) -> Result<&[u8], Box<dyn Error>> {
    let start = bytes
        .windows(MANAGED_BEGIN.len())
        .position(|window| window == MANAGED_BEGIN)
        .ok_or_else(|| io::Error::other("managed begin marker is absent"))?
        + MANAGED_BEGIN.len();
    let relative_end = bytes[start..]
        .windows(MANAGED_END.len())
        .position(|window| window == MANAGED_END)
        .ok_or_else(|| io::Error::other("managed end marker is absent"))?;
    Ok(&bytes[start..start + relative_end])
}

fn marker_count(bytes: &[u8], marker: &[u8]) -> usize {
    bytes
        .windows(marker.len())
        .filter(|window| *window == marker)
        .count()
}

fn modified(path: &Path) -> Result<SystemTime, Box<dyn Error>> {
    Ok(fs::metadata(path)?.modified()?)
}

fn count_rows(connection: &Connection, table: &str) -> Result<i64, rusqlite::Error> {
    let query = format!("SELECT COUNT(*) FROM {table}");
    connection.query_row(&query, [], |row| row.get(0))
}

fn operation_file_count(
    connection: &Connection,
    operation_id: &str,
) -> Result<i64, rusqlite::Error> {
    connection.query_row(
        "SELECT COUNT(*) FROM operation_files WHERE operation_id = ?1",
        [operation_id],
        |row| row.get(0),
    )
}

/// `(content_hash, path, operation_id)` for every version of one item, ordered by path.
fn version_rows(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Vec<(String, String, String)>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT content_hash, COALESCE(path, ''), COALESCE(operation_id, '') \
           FROM item_versions WHERE item_id = ?1 ORDER BY path",
    )?;
    statement
        .query_map([item_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect()
}

fn modified_event(path: &Path) -> WatchEvent {
    WatchEvent::Batch(WatchBatch {
        paths: vec![path.to_path_buf()],
        kinds: vec![WatchChange::Modified(path.to_path_buf())],
        observed_at: time::OffsetDateTime::now_utc(),
    })
}
