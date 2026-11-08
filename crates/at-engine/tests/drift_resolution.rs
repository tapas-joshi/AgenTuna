//! TC-040: drift adopt and restore resolution against real temporary files.
//!
//! Every test runs in a canonical temporary home. The store, backups, agent
//! directory, and skill target all live under it, so nothing touches the real
//! HOME, the Keychain, or a real agent configuration.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_db::Database;
use at_db::deployments::{DeploymentSpec, upsert_deployment};
use at_db::drift::{DriftEvent, DriftResolution, NewDriftEvent, drift_event, record_drift};
use at_db::items::{AdoptedItemSpec, upsert_adopted_item};
use at_db::ops::OperationActor;
use at_db::versions::{NewVersion, record_skill_version};
use at_engine::drift::{
    DriftAdoptRequest, DriftResolutionError, DriftResolutionOutcome, DriftRestoreRequest,
    adopt_drift, restore_drift,
};
use at_fs::ApplyRoots;
use at_migrate::undo::{UndoOperationRequest, UndoOutcome, undo_operation};
use at_model::{Agent, AgentId, ItemId, ItemKind, ItemState, McpServerDef, McpTransport, Scope};
use at_store::layout::{Slug, StoreLayout};
use at_store::{hash_bytes, hash_skill_folder};

/// Native text a Codex restore must produce for the canonical `alpha` definition.
const MCP_RESTORED_ALPHA: &[u8] =
    b"[mcp_servers.alpha]\ncommand = \"store\"\nargs = []\nenv = {}\nenabled = true\n";
const MCP_DISK_V2: &[u8] = b"[mcp_servers.alpha]\ncommand = \"edited\"\n";
const MCP_DISK_V3: &[u8] = b"[mcp_servers.alpha]\ncommand = \"third\"\n";
const MCP_PRESERVE_DISK_V2: &[u8] = b"# user comment\n[mcp_servers.alpha]\ncommand = \"edited\"\n\n[mcp_servers.beta]\ncommand = \"beta\"\n";
/// The preserved file after the projection rewrites only the managed entry.
const MCP_PRESERVE_RESTORED: &[u8] = b"# user comment\n[mcp_servers.alpha]\ncommand = \"store\"\nargs = []\nenv = {}\nenabled = true\n\n[mcp_servers.beta]\ncommand = \"beta\"\n";
const SKILL_SLUG: &str = "example-skill";
const SKILL_STORE_MD: &str = "# v1\n";
const SKILL_DISK_MD: &str = "# v2\n";
const SKILL_DISK_NOTES: &str = "drifted\n";
const SKILL_DISK_NOTES_V3: &str = "third\n";

/// Canonical temporary home plus the paths every scenario shares.
struct Fixture {
    _temp: tempfile::TempDir,
    store: PathBuf,
    backups: PathBuf,
    agent_dir: PathBuf,
    mcp_target: PathBuf,
    skill_target: PathBuf,
    /// Canonical temporary home, used to read the agent's MCP capability policy.
    home: PathBuf,
    roots: ApplyRoots,
    db: Database,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let home = fs::canonicalize(temp.path())?;
        let store = home.join("store");
        let backups = home.join("backups");
        let agent_dir = home.join("agent");
        let skills_dir = home.join(".claude").join("skills");
        for dir in [&store, &backups, &agent_dir, &skills_dir] {
            fs::create_dir_all(dir)?;
        }
        Ok(Self {
            mcp_target: agent_dir.join("config.toml"),
            skill_target: skills_dir.join(SKILL_SLUG),
            roots: ApplyRoots::new([home.clone()])?,
            home,
            db: Database::open_in_memory()?,
            _temp: temp,
            store,
            backups,
            agent_dir,
        })
    }
}

/// One drifted item and the open drift event that records the drift.
struct Drift {
    item_id: ItemId,
    event_id: i64,
    /// Agent whose deployment the drift event records.
    agent: AgentId,
    store_path: PathBuf,
    disk_path: PathBuf,
    /// Canonical store hash before the external edit.
    store_hash: String,
    /// Hash of the drifted disk content that the event records.
    disk_hash: String,
}

/// Inputs for registering one item, its deployment, and its open drift event.
struct Registration<'a> {
    kind: ItemKind,
    agent: AgentId,
    slug: &'a str,
    store_path: &'a Path,
    disk_path: &'a Path,
    store_hash: &'a str,
    disk_hash: &'a str,
}

/// One node of a snapshot: a directory, a regular file's bytes, or a symlink target.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Disk and store state for one drifted item, compared before and after each action.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    disk: BTreeMap<PathBuf, Entry>,
    store: BTreeMap<PathBuf, Entry>,
}

fn bytes_hash(bytes: &[u8]) -> String {
    hash_bytes(bytes).to_string()
}

fn skill_hash(folder: &Path) -> Result<String, Box<dyn Error>> {
    Ok(hash_skill_folder(folder)?.to_string())
}

fn path_str(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str()
        .ok_or_else(|| format!("non-UTF-8 path: {}", path.display()).into())
}

/// Every entry under `root`, keyed by path relative to `root`; the root itself has the empty key.
/// A missing root yields an empty map.
fn tree(root: &Path) -> io::Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    match fs::symlink_metadata(root) {
        Ok(_) => collect(root, Path::new(""), &mut entries)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(entries)
}

fn collect(
    absolute: &Path,
    relative: &Path,
    entries: &mut BTreeMap<PathBuf, Entry>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(absolute)?;
    if metadata.file_type().is_symlink() {
        entries.insert(
            relative.to_path_buf(),
            Entry::Link(fs::read_link(absolute)?),
        );
    } else if metadata.is_dir() {
        entries.insert(relative.to_path_buf(), Entry::Dir);
        for child in fs::read_dir(absolute)? {
            let child = child?;
            collect(&child.path(), &relative.join(child.file_name()), entries)?;
        }
    } else {
        entries.insert(relative.to_path_buf(), Entry::File(fs::read(absolute)?));
    }
    Ok(())
}

fn snapshot(drift: &Drift) -> io::Result<Snapshot> {
    Ok(Snapshot {
        disk: tree(&drift.disk_path)?,
        store: tree(&drift.store_path)?,
    })
}

/// Sets a Unix mode and restores the original mode on drop, so temporary trees stay removable.
struct ModeGuard {
    path: PathBuf,
    original: u32,
}

impl ModeGuard {
    fn lock(path: &Path, mode: u32) -> Result<Self, Box<dyn Error>> {
        let original = fs::metadata(path)?.permissions().mode() & 0o7777;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(Self {
            path: path.to_path_buf(),
            original,
        })
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.original));
    }
}

/// Fails the test when the file is still writable, which happens when the suite runs as root.
fn require_read_only_file(path: &Path) -> Result<(), Box<dyn Error>> {
    if fs::OpenOptions::new().append(true).open(path).is_ok() {
        return Err(format!(
            "precondition: {} is still writable; run without root",
            path.display()
        )
        .into());
    }
    Ok(())
}

/// Fails the test when the directory still accepts new files, which happens when the suite runs as root.
fn require_read_only_dir(dir: &Path) -> Result<(), Box<dyn Error>> {
    let probe = dir.join(".permission-probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        return Err(format!(
            "precondition: {} is still writable; run without root",
            dir.display()
        )
        .into());
    }
    Ok(())
}

fn adopt(fx: &Fixture, event_id: i64) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    adopt_drift(DriftAdoptRequest {
        event_id,
        store_root: &fx.store,
        backup_root: &fx.backups,
        authorized_roots: &fx.roots,
        connection: fx.db.connection(),
        actor: OperationActor::Ui,
    })
}

fn restore(fx: &Fixture, event_id: i64) -> Result<DriftResolutionOutcome, DriftResolutionError> {
    restore_drift(DriftRestoreRequest {
        event_id,
        store_root: &fx.store,
        backup_root: &fx.backups,
        authorized_roots: &fx.roots,
        connection: fx.db.connection(),
        home: &fx.home,
        actor: OperationActor::Ui,
    })
}

fn undo(fx: &Fixture, operation_id: &str) -> Result<UndoOutcome, Box<dyn Error>> {
    Ok(undo_operation(
        UndoOperationRequest {
            connection: fx.db.connection(),
            store_root: &fx.store,
            backup_root: &fx.backups,
            authorized_roots: &fx.roots,
            actor: OperationActor::Cli,
            force: false,
        },
        operation_id,
    )?)
}

fn write_skill(dir: &Path, skill_md: &str, notes: Option<&str>) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("SKILL.md"), skill_md)?;
    if let Some(notes) = notes {
        fs::write(dir.join("notes.md"), notes)?;
    }
    Ok(())
}

/// Store holds v1; disk holds the drifted v2 folder.
fn setup_skill(fx: &mut Fixture) -> Result<Drift, Box<dyn Error>> {
    let slug = Slug::parse(SKILL_SLUG)?;
    let store_path = StoreLayout::new(Scope::User, fx.store.clone()).skill_dir(&slug);
    let disk_path = fx.skill_target.clone();
    write_skill(&store_path, SKILL_STORE_MD, None)?;
    write_skill(&disk_path, SKILL_DISK_MD, Some(SKILL_DISK_NOTES))?;
    let store_hash = skill_hash(&store_path)?;
    let disk_hash = skill_hash(&disk_path)?;
    register_drift(
        fx,
        Registration {
            kind: ItemKind::Skill,
            agent: AgentId::ClaudeCode,
            slug: SKILL_SLUG,
            store_path: &store_path,
            disk_path: &disk_path,
            store_hash: &store_hash,
            disk_hash: &disk_hash,
        },
    )
}

/// Store holds `store_bytes`; the agent file holds the drifted `disk_bytes`.
/// Seeds one canonical MCP server plus a drifted Codex `config.toml`.
///
/// The store file is the canonical `mcp.json` (`schema_version` + `servers`), never a native
/// document: restore projects the one `alpha` definition into the native file, so the native
/// text in `disk_bytes` and the canonical definition are independent inputs.
fn setup_mcp(
    fx: &mut Fixture,
    store_command: &str,
    disk_bytes: &[u8],
) -> Result<Drift, Box<dyn Error>> {
    let store_path = StoreLayout::new(Scope::User, fx.store.clone()).mcp_file();
    let mut store = at_store::mcp_store::McpStore::empty();
    store.set(
        "alpha",
        McpServerDef {
            display_name: "alpha".to_owned(),
            description: None,
            transport: McpTransport::Stdio {
                command: store_command.to_owned(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        },
    );
    let store_bytes = store.to_bytes()?;
    let disk_path = fx.mcp_target.clone();
    fs::write(&store_path, &store_bytes)?;
    fs::write(&disk_path, disk_bytes)?;
    let store_hash = bytes_hash(&store_bytes);
    let disk_hash = bytes_hash(disk_bytes);
    register_drift(
        fx,
        Registration {
            kind: ItemKind::McpServer,
            agent: AgentId::Codex,
            slug: "alpha",
            store_path: &store_path,
            disk_path: &disk_path,
            store_hash: &store_hash,
            disk_hash: &disk_hash,
        },
    )
}

/// Display name the fixture registers for `agent`.
fn agent_display_name(agent: &AgentId) -> &'static str {
    match agent {
        AgentId::Codex => "Codex",
        _ => "Claude Code",
    }
}

/// Inserts the agent, item, deployment, and store version rows, then records one open drift event.
fn register_drift(fx: &mut Fixture, reg: Registration<'_>) -> Result<Drift, Box<dyn Error>> {
    let agent = reg.agent;
    let scope = Scope::User;
    let store_str = path_str(reg.store_path)?;
    let disk_str = path_str(reg.disk_path)?;
    let connection = fx.db.connection_mut();
    at_db::agents::upsert_agent(
        connection,
        &Agent {
            id: agent.clone(),
            display_name: agent_display_name(&agent).to_owned(),
            detected: true,
            version: None,
        },
    )?;
    let item_id = {
        let transaction = connection.transaction()?;
        let item_id = upsert_adopted_item(
            &transaction,
            &AdoptedItemSpec {
                proposed_id: ItemId::new(),
                kind: reg.kind,
                scope,
                project_id: None,
                slug: reg.slug,
                name: reg.slug,
                source_path: store_str,
                content_hash: Some(reg.store_hash),
            },
        )?;
        transaction.commit()?;
        item_id
    };
    let hashes = at_db::deployments::DeploymentHashes::in_sync(reg.store_hash);
    let deployment = DeploymentSpec::applied(
        &item_id,
        &agent,
        scope,
        ItemState::Linked,
        Some("copy"),
        Some(disk_str),
        hashes,
    );
    upsert_deployment(connection, &deployment)?;
    record_skill_version(
        connection,
        &NewVersion::new(&item_id, scope, reg.store_hash),
    )?;
    let item_text = item_id.to_string();
    let event_id = record_drift(
        connection,
        &NewDriftEvent::new(&agent, scope, disk_str)
            .for_item(&item_text)
            .with_hashes(Some(reg.store_hash), Some(reg.disk_hash)),
    )?;
    Ok(Drift {
        item_id,
        event_id,
        agent,
        store_path: reg.store_path.to_path_buf(),
        disk_path: reg.disk_path.to_path_buf(),
        store_hash: reg.store_hash.to_owned(),
        disk_hash: reg.disk_hash.to_owned(),
    })
}

/// `(store_hash, target_hash, baseline_hash)` of one deployment row.
type DeploymentHashes = (Option<String>, Option<String>, Option<String>);

fn deployment_hashes(
    fx: &Fixture,
    item_id: &ItemId,
    agent: &AgentId,
) -> Result<DeploymentHashes, Box<dyn Error>> {
    let hashes = fx.db.connection().query_row(
        "SELECT store_hash, target_hash, baseline_hash FROM deployments \
         WHERE item_id = ?1 AND agent_id = ?2 AND scope = ?3",
        rusqlite::params![
            item_id.to_string(),
            agent.to_string(),
            Scope::User.wire_name(),
        ],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    Ok(hashes)
}

fn version_count(
    fx: &Fixture,
    item_id: &ItemId,
    content_hash: &str,
) -> Result<i64, Box<dyn Error>> {
    let count = fx.db.connection().query_row(
        "SELECT COUNT(*) FROM item_versions WHERE item_id = ?1 AND content_hash = ?2",
        rusqlite::params![item_id.to_string(), content_hash],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count)
}

/// One backup row: the content hash recorded for it and the blob path.
type BackupRow = (Option<String>, PathBuf);

/// Backup rows the operation recorded for `path`: content hash and blob path.
fn backup_rows(
    fx: &Fixture,
    operation_id: &str,
    path: &Path,
) -> Result<Vec<BackupRow>, Box<dyn Error>> {
    let connection = fx.db.connection();
    let mut statement = connection.prepare(
        "SELECT content_hash, backup_path FROM backups WHERE operation_id = ?1 AND original_path = ?2",
    )?;
    let rows = statement.query_map(rusqlite::params![operation_id, path_str(path)?], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut backups = Vec::new();
    for row in rows {
        let (hash, blob) = row?;
        backups.push((hash, PathBuf::from(blob)));
    }
    Ok(backups)
}

/// Asserts the operation backed `path` up before writing it. A file's backup row
/// must record `pre_hash`. A folder's row stores the at-fs node hash, so its single
/// blob is hashed instead. Either way the blob must hold the pre-write content.
fn assert_backed_up_before_write(
    fx: &Fixture,
    operation_id: &str,
    path: &Path,
    pre_hash: &str,
    is_folder: bool,
) -> Result<(), Box<dyn Error>> {
    let backups = backup_rows(fx, operation_id, path)?;
    let blob = if is_folder {
        match backups.as_slice() {
            [(_, blob)] => blob.clone(),
            _ => {
                return Err(format!(
                    "expected one backup row for {}: {backups:?}",
                    path.display()
                )
                .into());
            }
        }
    } else {
        backups
            .iter()
            .find(|(hash, _)| hash.as_deref() == Some(pre_hash))
            .map(|(_, blob)| blob.clone())
            .ok_or_else(|| {
                format!(
                    "no backup row for {} records pre-write hash {pre_hash}: {backups:?}",
                    path.display()
                )
            })?
    };
    let blob_hash = if is_folder {
        skill_hash(&blob)?
    } else {
        bytes_hash(&fs::read(&blob)?)
    };
    assert_eq!(
        blob_hash,
        pre_hash,
        "backup blob for {} must hold the pre-write content",
        path.display()
    );
    Ok(())
}

/// Names the recorded resolution of a drift event for assertions.
fn resolution_label(event: &DriftEvent) -> &'static str {
    match &event.resolution {
        Some(DriftResolution::Adopt) => "adopt",
        Some(DriftResolution::Restore) => "restore",
        Some(DriftResolution::Ignore) => "ignore",
        Some(_) => "unknown",
        None => "none",
    }
}

fn assert_resolved_as(fx: &Fixture, event_id: i64, expected: &str) -> Result<(), Box<dyn Error>> {
    let event = drift_event(fx.db.connection(), event_id)?.ok_or("drift event is missing")?;
    assert!(!event.is_open(), "event {event_id} must be resolved");
    assert_eq!(
        resolution_label(&event),
        expected,
        "resolution recorded for event {event_id}"
    );
    Ok(())
}

fn assert_still_open(fx: &Fixture, event_id: i64) -> Result<(), Box<dyn Error>> {
    let event = drift_event(fx.db.connection(), event_id)?.ok_or("drift event is missing")?;
    assert!(
        event.is_open(),
        "event {event_id} must stay open after a failed resolution"
    );
    Ok(())
}

fn assert_hashes_after_resolution(
    fx: &Fixture,
    drift: &Drift,
    canonical: &str,
) -> Result<(), Box<dyn Error>> {
    let (store, target, baseline) = deployment_hashes(fx, &drift.item_id, &drift.agent)?;
    assert_eq!(
        store.as_deref(),
        Some(canonical),
        "deployments.store_hash must match the canonical content"
    );
    assert_eq!(
        target.as_deref(),
        Some(canonical),
        "deployments.target_hash must match the canonical content"
    );
    assert_eq!(
        baseline.as_deref(),
        Some(canonical),
        "deployments.baseline_hash must match the canonical content"
    );
    Ok(())
}

#[test]
fn adopt_skill_edit_replaces_store_and_resolves_event() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;

    let outcome = adopt(&fx, drift.event_id)?;

    assert_eq!(outcome.touched_paths, vec![drift.store_path.clone()]);
    assert_eq!(
        tree(&drift.store_path)?,
        tree(&drift.disk_path)?,
        "store must hold the drifted folder after adopt"
    );
    assert_eq!(
        skill_hash(&drift.store_path)?,
        drift.disk_hash,
        "store hash must equal the adopted content hash"
    );
    assert_resolved_as(&fx, drift.event_id, "adopt")?;
    assert_hashes_after_resolution(&fx, &drift, &drift.disk_hash)?;
    assert_eq!(
        version_count(&fx, &drift.item_id, &drift.disk_hash)?,
        1,
        "item_versions must record the adopted content"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.store_path,
        &drift.store_hash,
        true,
    )?;
    Ok(())
}

#[test]
fn adopt_mcp_edit_replaces_store_and_resolves_event() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;

    let outcome = adopt(&fx, drift.event_id)?;

    let adopted = at_store::mcp_store::McpStore::parse(&fs::read(&drift.store_path)?)?;
    let definition = adopted
        .get("alpha")
        .ok_or("adopt must keep the alpha definition")?;
    match &definition.transport {
        McpTransport::Stdio { command, .. } => {
            assert_eq!(command, "edited", "store must hold the drifted command");
        }
        other => return Err(format!("expected a stdio definition, got {other:?}").into()),
    }
    assert_eq!(
        fs::read(&drift.disk_path)?,
        MCP_DISK_V2,
        "adopt must leave the disk file unchanged"
    );
    assert_resolved_as(&fx, drift.event_id, "adopt")?;
    // The canonical document changed; the deployment keeps the native file as its baseline.
    let adopted_hash = bytes_hash(&fs::read(&drift.store_path)?);
    let native_hash = bytes_hash(&fs::read(&drift.disk_path)?);
    let (store, target, baseline) = deployment_hashes(&fx, &drift.item_id, &drift.agent)?;
    assert_eq!(store.as_deref(), Some(adopted_hash.as_str()));
    assert_eq!(target.as_deref(), Some(native_hash.as_str()));
    assert_eq!(baseline.as_deref(), Some(native_hash.as_str()));
    assert_eq!(
        version_count(&fx, &drift.item_id, &adopted_hash)?,
        1,
        "item_versions must record the adopted content"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.store_path,
        &drift.store_hash,
        false,
    )?;
    Ok(())
}

#[test]
fn restore_skill_edit_replaces_disk_and_resolves_event() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;

    let outcome = restore(&fx, drift.event_id)?;

    assert_eq!(
        tree(&drift.disk_path)?,
        tree(&drift.store_path)?,
        "disk must match the store after restore"
    );
    assert_resolved_as(&fx, drift.event_id, "restore")?;
    assert_hashes_after_resolution(&fx, &drift, &drift.store_hash)?;
    assert_eq!(
        version_count(&fx, &drift.item_id, &drift.store_hash)?,
        1,
        "the canonical version row must remain recorded"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.disk_path,
        &drift.disk_hash,
        true,
    )?;
    Ok(())
}

#[test]
fn restore_mcp_edit_replaces_disk_and_resolves_event() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;

    let outcome = restore(&fx, drift.event_id)?;

    assert_eq!(
        fs::read(&drift.disk_path)?,
        MCP_RESTORED_ALPHA,
        "disk must hold the projected canonical entry after restore"
    );
    assert_resolved_as(&fx, drift.event_id, "restore")?;
    // The store keeps the canonical document; the deployment records the projected file.
    let native_hash = bytes_hash(&fs::read(&drift.disk_path)?);
    let (store, target, baseline) = deployment_hashes(&fx, &drift.item_id, &drift.agent)?;
    assert_eq!(
        store.as_deref(),
        Some(drift.store_hash.as_str()),
        "deployments.store_hash keeps the canonical document hash"
    );
    assert_eq!(
        target.as_deref(),
        Some(native_hash.as_str()),
        "deployments.target_hash records the projected native file"
    );
    assert_eq!(
        baseline.as_deref(),
        Some(native_hash.as_str()),
        "deployments.baseline_hash records the projected native file"
    );
    assert_eq!(
        version_count(&fx, &drift.item_id, &drift.store_hash)?,
        1,
        "the canonical version row must remain recorded"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.disk_path,
        &drift.disk_hash,
        false,
    )?;
    Ok(())
}

#[test]
fn restore_mcp_preserves_unrelated_servers_and_toml_comments() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_PRESERVE_DISK_V2)?;

    let outcome = restore(&fx, drift.event_id)?;

    let after = fs::read_to_string(&drift.disk_path)?;
    assert_eq!(
        fs::read(&drift.disk_path)?,
        MCP_PRESERVE_RESTORED,
        "restore must change only the managed value; other bytes must stay identical"
    );
    assert!(
        after.contains("# user comment"),
        "TOML comment must survive restore:\n{after}"
    );
    assert!(
        after.contains("[mcp_servers.beta]"),
        "unrelated server must survive restore:\n{after}"
    );
    assert!(
        after.contains("command = \"beta\""),
        "unrelated server value must survive restore:\n{after}"
    );
    assert!(
        after.contains("command = \"store\""),
        "managed server must take the stored value:\n{after}"
    );
    let disk_after = fs::read(&drift.disk_path)?;
    let disk_hash = bytes_hash(&disk_after);
    let (store, target, baseline) = deployment_hashes(&fx, &drift.item_id, &drift.agent)?;
    assert_eq!(
        store.as_deref(),
        Some(drift.store_hash.as_str()),
        "deployments.store_hash must keep the stored canonical hash"
    );
    assert_eq!(
        target.as_deref(),
        Some(disk_hash.as_str()),
        "deployments.target_hash must record the merged bytes on disk"
    );
    assert_eq!(
        baseline.as_deref(),
        Some(disk_hash.as_str()),
        "deployments.baseline_hash must record the merged bytes on disk"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.disk_path,
        &drift.disk_hash,
        false,
    )?;
    Ok(())
}

#[test]
fn restoring_one_server_quiets_every_other_entry_sharing_the_file() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let native = fx.home.join(".claude.json");
    let disk = br#"{"mcpServers":{"alpha":{"command":"node","args":["edited.js"]},"beta":{"command":"keep"}}}"#;
    fs::write(&native, disk)?;
    let layout = StoreLayout::new(Scope::User, fx.store.clone());
    let mut store = at_store::mcp_store::McpStore::empty();
    for slug in ["alpha", "beta"] {
        store.set(
            slug,
            McpServerDef {
                display_name: slug.to_owned(),
                description: None,
                transport: McpTransport::Stdio {
                    command: if slug == "alpha" { "node" } else { "keep" }.to_owned(),
                    args: vec!["store.js".to_owned()],
                    cwd: None,
                    env: BTreeMap::new(),
                },
                extra: BTreeMap::new(),
            },
        );
    }
    let store_bytes = store.to_bytes()?;
    fs::write(layout.mcp_file(), &store_bytes)?;
    let store_hash = bytes_hash(&store_bytes);
    let disk_hash = bytes_hash(disk);
    let alpha = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::McpServer,
            agent: AgentId::ClaudeCode,
            slug: "alpha",
            store_path: &layout.mcp_file(),
            disk_path: &native,
            store_hash: &store_hash,
            disk_hash: &disk_hash,
        },
    )?;
    // A second server on the same file records the same file baseline.
    let _beta = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::McpServer,
            agent: AgentId::ClaudeCode,
            slug: "beta",
            store_path: &layout.mcp_file(),
            disk_path: &native,
            store_hash: &store_hash,
            disk_hash: &disk_hash,
        },
    )?;
    fx.db
        .connection()
        .execute(
            "UPDATE deployments SET baseline_hash = ?1, target_hash = ?1 \
             WHERE scope = 'user' AND agent_id = 'claude_code'",
            [store_hash.as_str()],
        )
        .expect("seed the shared baseline");

    restore(&fx, alpha.event_id)?;

    let notices = at_engine::drift::reconcile_indexed(&fx.db).expect("post-restore scan");
    assert!(
        notices.is_empty(),
        "one resolution must quiet every entry on the shared file: {notices:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn adopting_a_drifted_linked_skill_keeps_the_link_and_the_store_copy() -> Result<(), Box<dyn Error>>
{
    use std::os::unix::fs::symlink;

    let mut fx = Fixture::new()?;
    let store_dir = StoreLayout::new(Scope::User, fx.store.clone())
        .skill_dir(&Slug::parse(SKILL_SLUG).expect("slug"));
    fs::create_dir_all(&store_dir)?;
    fs::write(store_dir.join("SKILL.md"), SKILL_STORE_MD)?;
    let links = fx.home.join(".claude/skills");
    fs::create_dir_all(&links)?;
    let link = links.join(SKILL_SLUG);
    symlink(&store_dir, &link)?;
    let store_hash = skill_hash(&store_dir)?;
    // The user edits through the managed link, so the canonical tree changes with it.
    fs::write(link.join("SKILL.md"), SKILL_DISK_MD)?;
    let edited_hash = skill_hash(&store_dir)?;
    assert_ne!(edited_hash, store_hash);
    let drift = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::Skill,
            agent: AgentId::ClaudeCode,
            slug: SKILL_SLUG,
            store_path: &store_dir,
            disk_path: &link,
            store_hash: &store_hash,
            disk_hash: &edited_hash,
        },
    )?;

    let outcome = adopt(&fx, drift.event_id)?;

    assert!(!outcome.operation_id.is_empty());
    assert_eq!(
        fs::read(store_dir.join("SKILL.md"))?,
        SKILL_DISK_MD.as_bytes(),
        "the edit stays in the canonical tree"
    );
    assert!(
        fs::symlink_metadata(&link)?.file_type().is_symlink(),
        "the managed link is not replaced by a directory"
    );
    assert_resolved_as(&fx, drift.event_id, "adopt")?;
    assert_eq!(skill_hash(&store_dir)?, edited_hash);
    Ok(())
}

#[cfg(unix)]
#[test]
fn restoring_a_drifted_linked_skill_leaves_the_link_and_canonical_bytes()
-> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::symlink;

    let mut fx = Fixture::new()?;
    let store_dir = StoreLayout::new(Scope::User, fx.store.clone())
        .skill_dir(&Slug::parse(SKILL_SLUG).expect("slug"));
    fs::create_dir_all(&store_dir)?;
    fs::write(store_dir.join("SKILL.md"), SKILL_STORE_MD)?;
    let links = fx.home.join(".claude/skills");
    fs::create_dir_all(&links)?;
    let link = links.join(SKILL_SLUG);
    symlink(&store_dir, &link)?;
    let store_hash = skill_hash(&store_dir)?;
    fs::write(link.join("SKILL.md"), SKILL_DISK_MD)?;
    let edited_hash = skill_hash(&store_dir)?;
    let drift = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::Skill,
            agent: AgentId::ClaudeCode,
            slug: SKILL_SLUG,
            store_path: &store_dir,
            disk_path: &link,
            store_hash: &store_hash,
            disk_hash: &edited_hash,
        },
    )?;

    let outcome = restore(&fx, drift.event_id)?;

    assert!(!outcome.operation_id.is_empty());
    assert!(
        fs::symlink_metadata(&link)?.file_type().is_symlink(),
        "restore must not turn the managed link into a directory"
    );
    assert_eq!(
        fs::read(store_dir.join("SKILL.md"))?,
        SKILL_DISK_MD.as_bytes(),
        "a linked target has no separate copy to restore over"
    );
    assert_resolved_as(&fx, drift.event_id, "restore")?;
    Ok(())
}

#[test]
fn restore_claude_mcp_keeps_unrelated_servers_and_top_level_keys() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let native = fx.home.join(".claude.json");
    let disk = br#"{
  "mcpServers": {
    "alpha": { "command": "node", "args": ["edited.js"] },
    "beta": { "command": "keep" }
  },
  "keep": true
}
"#;
    fs::write(&native, disk)?;
    let layout = StoreLayout::new(Scope::User, fx.store.clone());
    let mut store = at_store::mcp_store::McpStore::empty();
    store.set(
        "alpha",
        McpServerDef {
            display_name: "alpha".to_owned(),
            description: None,
            transport: McpTransport::Stdio {
                command: "node".to_owned(),
                args: vec!["store.js".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        },
    );
    let store_bytes = store.to_bytes()?;
    fs::write(layout.mcp_file(), &store_bytes)?;
    let drift = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::McpServer,
            agent: AgentId::ClaudeCode,
            slug: "alpha",
            store_path: &layout.mcp_file(),
            disk_path: &native,
            store_hash: &bytes_hash(&store_bytes),
            disk_hash: &bytes_hash(disk),
        },
    )?;

    restore(&fx, drift.event_id)?;

    let after: serde_json::Value = serde_json::from_slice(&fs::read(&native)?)?;
    assert_eq!(
        after["mcpServers"]["alpha"]["args"][0], "store.js",
        "the managed entry takes the canonical definition: {after}"
    );
    assert_eq!(
        after["mcpServers"]["beta"]["command"], "keep",
        "an unrelated server survives: {after}"
    );
    assert_eq!(
        after["keep"], true,
        "an unrelated top-level key survives: {after}"
    );
    assert_resolved_as(&fx, drift.event_id, "restore")?;
    Ok(())
}

#[test]
fn restore_skill_after_store_deleted_is_orphaned_and_writes_nothing() -> Result<(), Box<dyn Error>>
{
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;
    fs::remove_dir_all(&drift.store_path)?;
    let before = snapshot(&drift)?;

    let result = restore(&fx, drift.event_id);

    assert!(
        matches!(result, Err(DriftResolutionError::OrphanedDrift)),
        "expected OrphanedDrift, got {result:?}"
    );
    assert_eq!(snapshot(&drift)?, before, "no byte may change");
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn restore_mcp_after_store_deleted_is_orphaned_and_writes_nothing() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;
    fs::remove_file(&drift.store_path)?;
    let before = snapshot(&drift)?;

    let result = restore(&fx, drift.event_id);

    assert!(
        matches!(result, Err(DriftResolutionError::OrphanedDrift)),
        "expected OrphanedDrift, got {result:?}"
    );
    assert_eq!(snapshot(&drift)?, before, "no byte may change");
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn adopt_mcp_second_edit_returns_drift_changed_again_without_writes() -> Result<(), Box<dyn Error>>
{
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;
    // A second external edit lands after the reviewed v2 diff.
    fs::write(&drift.disk_path, MCP_DISK_V3)?;
    let before = snapshot(&drift)?;

    let result = adopt(&fx, drift.event_id);

    match result {
        Err(DriftResolutionError::DriftChangedAgain { expected, actual }) => {
            assert_eq!(
                expected, drift.disk_hash,
                "expected hash is the one the event recorded"
            );
            assert_eq!(
                actual,
                bytes_hash(MCP_DISK_V3),
                "actual hash is the second edit"
            );
        }
        other => return Err(format!("expected DriftChangedAgain, got {other:?}").into()),
    }
    assert_eq!(snapshot(&drift)?, before, "no byte may change");
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn restore_skill_second_edit_returns_drift_changed_again_without_writes()
-> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;
    // A second external edit lands after the reviewed v2 diff.
    fs::write(drift.disk_path.join("notes.md"), SKILL_DISK_NOTES_V3)?;
    let second_hash = skill_hash(&drift.disk_path)?;
    let before = snapshot(&drift)?;

    let result = restore(&fx, drift.event_id);

    match result {
        Err(DriftResolutionError::DriftChangedAgain { expected, actual }) => {
            assert_eq!(
                expected, drift.disk_hash,
                "expected hash is the one the event recorded"
            );
            assert_eq!(actual, second_hash, "actual hash is the second edit");
        }
        other => return Err(format!("expected DriftChangedAgain, got {other:?}").into()),
    }
    assert_eq!(snapshot(&drift)?, before, "no byte may change");
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn adopt_mcp_permission_denied_keeps_bytes_and_event_open() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;
    let _file_lock = ModeGuard::lock(&drift.store_path, 0o444)?;
    let _dir_lock = ModeGuard::lock(&fx.store, 0o555)?;
    require_read_only_file(&drift.store_path)?;
    require_read_only_dir(&fx.store)?;
    let before = snapshot(&drift)?;

    let result = adopt(&fx, drift.event_id);

    assert!(
        result.is_err(),
        "adopt must fail when the store is read-only, got {result:?}"
    );
    assert_eq!(
        snapshot(&drift)?,
        before,
        "a failed adopt must change no byte"
    );
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn restore_mcp_permission_denied_keeps_bytes_and_event_open() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;
    let _dir_lock = ModeGuard::lock(&fx.agent_dir, 0o555)?;
    let _file_lock = ModeGuard::lock(&drift.disk_path, 0o444)?;
    require_read_only_dir(&fx.agent_dir)?;
    require_read_only_file(&drift.disk_path)?;
    let before = snapshot(&drift)?;

    let result = restore(&fx, drift.event_id);

    assert!(
        result.is_err(),
        "restore must fail when the agent directory is read-only, got {result:?}"
    );
    assert_eq!(
        snapshot(&drift)?,
        before,
        "a failed restore must change no byte"
    );
    assert_still_open(&fx, drift.event_id)?;
    Ok(())
}

#[test]
fn undo_adopt_skill_restores_pre_action_tree() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;
    let before = snapshot(&drift)?;
    let outcome = adopt(&fx, drift.event_id)?;
    assert_ne!(
        snapshot(&drift)?,
        before,
        "adopt must change the store before undo is meaningful"
    );

    let undone = undo(&fx, &outcome.operation_id)?;

    assert!(
        matches!(undone, UndoOutcome::Restored { .. }),
        "undo must report Restored, got {undone:?}"
    );
    assert_eq!(
        snapshot(&drift)?,
        before,
        "undo must return the exact pre-adopt file set"
    );
    Ok(())
}

#[test]
fn undo_restore_skill_restores_pre_action_tree() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_skill(&mut fx)?;
    let before = snapshot(&drift)?;
    let outcome = restore(&fx, drift.event_id)?;
    assert_ne!(
        snapshot(&drift)?,
        before,
        "restore must change the disk folder before undo is meaningful"
    );

    let undone = undo(&fx, &outcome.operation_id)?;

    assert!(
        matches!(undone, UndoOutcome::Restored { .. }),
        "undo must report Restored, got {undone:?}"
    );
    assert_eq!(
        snapshot(&drift)?,
        before,
        "undo must return the exact pre-restore file set"
    );
    Ok(())
}

#[test]
fn undo_restore_mcp_restores_pre_action_tree() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;
    let before = snapshot(&drift)?;
    let outcome = restore(&fx, drift.event_id)?;
    assert_ne!(
        snapshot(&drift)?,
        before,
        "restore must change the agent file before undo is meaningful"
    );

    let undone = undo(&fx, &outcome.operation_id)?;

    assert!(
        matches!(undone, UndoOutcome::Restored { .. }),
        "undo must report Restored, got {undone:?}"
    );
    assert_eq!(
        snapshot(&drift)?,
        before,
        "undo must return the exact pre-restore file set"
    );
    Ok(())
}

#[test]
fn adopt_mcp_updates_deployment_store_hash() -> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let drift = setup_mcp(&mut fx, "store", MCP_DISK_V2)?;

    let outcome = adopt(&fx, drift.event_id)?;

    let adopted_hash = bytes_hash(&fs::read(&drift.store_path)?);
    let (store, _target, _baseline) = deployment_hashes(&fx, &drift.item_id, &drift.agent)?;
    assert_eq!(
        store.as_deref(),
        Some(adopted_hash.as_str()),
        "deployments.store_hash must track the adopted canonical document"
    );
    assert_backed_up_before_write(
        &fx,
        &outcome.operation_id,
        &drift.store_path,
        &drift.store_hash,
        false,
    )?;
    Ok(())
}

/// Restoring a drifted agent document that is a dotfile-managed symlink writes the resolved
/// target through the link and keeps the link itself in place.
#[test]
fn restoring_a_symlinked_claude_config_keeps_the_link_and_updates_the_target()
-> Result<(), Box<dyn Error>> {
    let mut fx = Fixture::new()?;
    let dotfiles = fx.home.join("dotfiles");
    fs::create_dir_all(&dotfiles)?;
    let target = dotfiles.join("claude.json");
    let drifted = b"{\"mcpServers\":{\"alpha\":{\"command\":\"edited\"}}}\n";
    fs::write(&target, drifted)?;
    let link = fx.home.join(".claude.json");
    std::os::unix::fs::symlink(&target, &link)?;

    let store_path = StoreLayout::new(Scope::User, fx.store.clone()).mcp_file();
    let mut store = at_store::mcp_store::McpStore::empty();
    store.set(
        "alpha",
        McpServerDef {
            display_name: "alpha".to_owned(),
            description: None,
            transport: McpTransport::Stdio {
                command: "store".to_owned(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        },
    );
    let store_bytes = store.to_bytes()?;
    fs::write(&store_path, &store_bytes)?;
    let store_hash = bytes_hash(&store_bytes);
    let disk_hash = bytes_hash(drifted);
    let drift = register_drift(
        &mut fx,
        Registration {
            kind: ItemKind::McpServer,
            agent: AgentId::ClaudeCode,
            slug: "alpha",
            store_path: &store_path,
            disk_path: &link,
            store_hash: &store_hash,
            disk_hash: &disk_hash,
        },
    )?;

    let outcome = restore(&fx, drift.event_id)?;

    assert_eq!(outcome.event_id, drift.event_id);
    assert!(
        fs::symlink_metadata(&link)?.file_type().is_symlink(),
        "the dotfile link must survive the restore"
    );
    let restored: serde_json::Value = serde_json::from_slice(&fs::read(&target)?)?;
    assert_eq!(
        restored["mcpServers"]["alpha"]["command"].as_str(),
        Some("store"),
        "the target holds the canonical projection: {restored}"
    );
    Ok(())
}
