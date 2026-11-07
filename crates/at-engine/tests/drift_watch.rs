//! TC-037: watcher-driven drift detection against a fake HOME.
//!
//! Every test drives the real debounced watcher over temporary directories and feeds the
//! batches it produces into [`DriftService`]. Nothing here touches the network, the real HOME,
//! the Keychain or a real agent configuration, and no assertion relies on a fixed sleep: the
//! platform decides when a batch arrives, while the timing budget checks that it arrives in
//! time.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use at_db::Database;
use at_db::deployments::{DeploymentSpec, upsert_deployment};
use at_engine::drift::{
    DriftKind, DriftNotice, DriftService, DriftTarget, drift_watch_config, targets_from_index,
};
use at_fs::{
    RecentWrites, WatchBatch, WatchConfig, WatchEvent, WatchRoot, WatchRootKind, WatchService,
    WriteRequest,
};
use at_model::{AgentId, ItemId, ItemState, Scope};

/// Time a fixture is given to settle before watching starts, so FSEvents does not replay the
/// creation of the tree as an external change. A loaded CI runner starts the watcher thread
/// late, so this is longer than a quiet machine needs.
const SETTLE: Duration = Duration::from_millis(2_500);
/// Per-test deadline for the watcher to deliver a batch.
///
/// FSEvents with a 300 ms debounce normally delivers in well under a second; the deadline is
/// generous because it waits for an OS callback, not for our own work.
const BUDGET: Duration = Duration::from_secs(15);

/// Files one burst rewrites, and the quiet window that ends the drain of its batches.
const ASSETS: usize = 300;
const QUIET: Duration = Duration::from_secs(3);

/// A canonical temporary home with one agent skills folder and one AgenTuna store.
struct Fixture {
    #[allow(dead_code)]
    temp: tempfile::TempDir,
    home: PathBuf,
    store: PathBuf,
    skills: PathBuf,
    database: Database,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::TempDir::new()?;
        // macOS reports `/private/var/...`; both the watcher and the assertions use one form.
        let home = fs::canonicalize(temp.path())?;
        let store = home.join(".agentuna");
        let skills = home.join(".claude/skills");
        fs::create_dir_all(&skills)?;
        fs::create_dir_all(&store)?;
        let database = Database::open_in_memory()?;
        Ok(Self {
            temp,
            home,
            store,
            skills,
            database,
        })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Seeds one item and deployment row so `targets_from_index` sees a real baseline.
    ///
    /// The columns mirror the deployment row on purpose; a caller that leaves a column out
    /// would silently test a different baseline.
    #[allow(clippy::too_many_arguments)]
    fn seed(
        &self,
        item_id: &ItemId,
        agent: &AgentId,
        kind: &str,
        target: &Path,
        mode: &str,
        baseline: Option<&str>,
        link: Option<&Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let connection = self.database.connection();
        connection.execute(
            "INSERT INTO items (id, kind, scope, slug, name, source_path, state, managed) \
             VALUES (?1, ?2, 'user', ?3, ?3, ?4, 'present', 1)",
            [
                item_id.to_string(),
                kind.to_owned(),
                format!("slug-{}", item_id),
                target.display().to_string(),
            ],
        )?;
        let target_text = target.to_string_lossy().into_owned();
        let hashes = at_db::deployments::DeploymentHashes::new(None, baseline, baseline);
        let mut spec = DeploymentSpec::applied(
            item_id,
            agent,
            Scope::User,
            ItemState::Linked,
            Some(mode),
            Some(&target_text),
            hashes,
        );
        let link_text = link.map(|path| path.to_string_lossy().into_owned());
        if let Some(link_text) = link_text.as_deref() {
            spec = spec.with_link_target(link_text);
        }
        upsert_deployment(connection, &spec)?;
        Ok(())
    }

    fn targets(&self) -> Result<Vec<DriftTarget>, Box<dyn std::error::Error>> {
        Ok(targets_from_index(self.database.connection())?)
    }

    fn service(&self, roots: Vec<WatchRoot>) -> Result<WatchService, Box<dyn std::error::Error>> {
        let service = WatchService::new(drift_watch_config(roots, Arc::new(RecentWrites::new())));
        service.start()?;
        std::thread::sleep(SETTLE);
        Ok(service)
    }

    fn process(&self, event: &WatchEvent) -> Result<Vec<DriftNotice>, Box<dyn std::error::Error>> {
        let targets = self.targets()?;
        let service = DriftService {
            database: &self.database,
            targets: &targets,
        };
        Ok(service.process(event)?)
    }

    /// Runs the reconciliation path the watcher uses after startup or an overflow.
    fn reconcile(&self) -> Result<Vec<DriftNotice>, Box<dyn std::error::Error>> {
        let targets = self.targets()?;
        let service = DriftService {
            database: &self.database,
            targets: &targets,
        };
        Ok(service.reconcile()?)
    }
}

/// Waits for the next batch, returning it with the time it took to arrive.
fn next_batch(service: &WatchService, budget: Duration) -> Option<(WatchBatch, Duration)> {
    let started = Instant::now();
    let deadline = started + budget;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match service.next_event(remaining) {
            Some(WatchEvent::Batch(batch)) => return Some((batch, started.elapsed())),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// Debounce windows a span covers, at least one, for the coalescing bound.
fn spanning_windows(span: Duration) -> u128 {
    span.as_nanos() / at_fs::DEFAULT_DEBOUNCE.as_nanos() + 1
}

fn skill_hash(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(at_store::hash_skill_folder(root)?.to_string())
}

fn file_hash(bytes: &[u8]) -> String {
    at_fs::sha256_hex(bytes)
}

const SKILL_BODY: &[u8] = b"---\nname: demo\ndescription: drift fixture\n---\nBody\n";

#[test]
fn an_external_skill_edit_is_drifted_with_both_hashes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let skill = fixture.path(".claude/skills/demo");
    fs::create_dir_all(&skill)?;
    fs::write(skill.join("SKILL.md"), SKILL_BODY)?;
    let baseline = skill_hash(&skill)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &skill,
        "copy",
        Some(&baseline),
        None,
    )?;
    let service = fixture.service(vec![WatchRoot::dir(
        &fixture.skills,
        WatchRootKind::SkillsDir,
    )])?;

    let edited = b"---\nname: demo\ndescription: drift fixture\n---\nEdited externally\n";
    fs::write(skill.join("SKILL.md"), edited)?;
    let (batch, elapsed) = next_batch(&service, BUDGET).expect("an external edit produces a batch");
    assert!(
        elapsed < BUDGET,
        "the batch arrived after {elapsed:?}, over the {BUDGET:?} budget"
    );
    assert!(
        batch.contains(&skill) || batch.paths.iter().any(|path| path.starts_with(&skill)),
        "the batch names the edited skill: {:?}",
        batch.paths
    );

    let notices = fixture.process(&WatchEvent::Batch(batch))?;
    assert_eq!(notices.len(), 1, "one notice per changed deployment");
    let notice = &notices[0];
    assert_eq!(notice.kind, DriftKind::Drifted);
    assert_eq!(notice.path, skill);
    assert_eq!(notice.expected_hash.as_deref(), Some(baseline.as_str()));
    assert_eq!(
        notice.actual_hash.as_deref(),
        Some(skill_hash(&skill)?.as_str())
    );
    assert!(notice.changed_file_count >= 1);
    service.stop()?;
    Ok(())
}

#[test]
fn an_external_claude_json_edit_is_drifted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let original = br#"{"mcpServers":{"example":{"command":"node"}}}"#;
    let path = fixture.write(".claude.json", original)?;
    let baseline = file_hash(original);
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "mcp_server",
        &path,
        "native",
        Some(&baseline),
        None,
    )?;
    let service = fixture.service(vec![WatchRoot::file(&path, WatchRootKind::McpFile)])?;

    let edited = br#"{"mcpServers":{"example":{"command":"python"}}}"#;
    fs::write(&path, edited)?;
    let (batch, elapsed) = next_batch(&service, BUDGET).expect("a config edit produces a batch");
    assert!(elapsed < BUDGET, "config edit arrived after {elapsed:?}");
    let notices = fixture.process(&WatchEvent::Batch(batch))?;
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, DriftKind::Drifted);
    assert_eq!(notices[0].path, path);
    assert_eq!(notices[0].expected_hash.as_deref(), Some(baseline.as_str()));
    assert_eq!(
        notices[0].actual_hash.as_deref(),
        Some(file_hash(edited).as_str())
    );
    service.stop()?;
    Ok(())
}

#[test]
fn a_link_replaced_by_a_real_folder_is_distinguished_from_an_edit()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let store_skill = fixture.path(".agentuna/skills/demo");
    fs::create_dir_all(&store_skill)?;
    fs::write(store_skill.join("SKILL.md"), SKILL_BODY)?;
    let link = fixture.path(".claude/skills/demo");
    fs::create_dir_all(&fixture.skills)?;
    symlink(&store_skill, &link)?;
    let baseline = skill_hash(&store_skill)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &link,
        "symlink",
        Some(&baseline),
        Some(&store_skill),
    )?;
    let service = fixture.service(vec![
        WatchRoot::dir(&fixture.skills, WatchRootKind::SkillsDir),
        WatchRoot::dir(&fixture.store, WatchRootKind::UserStore),
    ])?;

    fs::remove_file(&link)?;
    fs::create_dir_all(&link)?;
    fs::write(link.join("SKILL.md"), "# Replacement\n")?;
    let (batch, _) = next_batch(&service, BUDGET).expect("replacing the link produces a batch");
    let notices = fixture.process(&WatchEvent::Batch(batch))?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].kind, DriftKind::LinkReplacedByFile);
    assert_eq!(notices[0].path, link);
    assert_eq!(
        notices[0].link_target.as_deref(),
        Some(store_skill.as_path())
    );
    service.stop()?;
    Ok(())
}

#[test]
fn a_deleted_link_is_link_removed_not_an_edit() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let store_skill = fixture.path(".agentuna/skills/demo");
    fs::create_dir_all(&store_skill)?;
    fs::write(store_skill.join("SKILL.md"), SKILL_BODY)?;
    let link = fixture.path(".claude/skills/demo");
    fs::create_dir_all(&fixture.skills)?;
    symlink(&store_skill, &link)?;
    let baseline = skill_hash(&store_skill)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &link,
        "symlink",
        Some(&baseline),
        Some(&store_skill),
    )?;
    let service = fixture.service(vec![WatchRoot::dir(
        &fixture.skills,
        WatchRootKind::SkillsDir,
    )])?;

    fs::remove_file(&link)?;
    // Some runners (and FSEvents under load) never deliver the removal of a symlink: the
    // production design repairs that with the reconciliation pass that also runs after startup
    // and after an overflow, so the test accepts either path and asserts the same outcome.
    let mut notices = match next_batch(&service, SETTLE) {
        Some((batch, _)) => fixture.process(&WatchEvent::Batch(batch))?,
        None => Vec::new(),
    };
    if notices.is_empty() {
        notices = fixture.reconcile()?;
    }
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].kind, DriftKind::LinkRemoved);
    assert_eq!(notices[0].path, link);
    assert!(notices[0].actual_hash.is_none());
    service.stop()?;
    Ok(())
}

#[test]
fn a_store_edit_while_linked_is_store_file_edited_while_linked()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let store_skill = fixture.path(".agentuna/skills/demo");
    fs::create_dir_all(&store_skill)?;
    fs::write(store_skill.join("SKILL.md"), SKILL_BODY)?;
    let link = fixture.path(".claude/skills/demo");
    fs::create_dir_all(&fixture.skills)?;
    symlink(&store_skill, &link)?;
    let baseline = skill_hash(&store_skill)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &link,
        "symlink",
        Some(&baseline),
        Some(&store_skill),
    )?;
    let service = fixture.service(vec![
        WatchRoot::dir(&fixture.store, WatchRootKind::UserStore),
        WatchRoot::dir(&fixture.skills, WatchRootKind::SkillsDir),
    ])?;

    let edited = b"---\nname: demo\ndescription: drift fixture\n---\nEdited in the store\n";
    fs::write(store_skill.join("SKILL.md"), edited)?;
    let (batch, _) = next_batch(&service, BUDGET).expect("a store edit produces a batch");
    assert!(
        batch
            .paths
            .iter()
            .any(|path| path.starts_with(&store_skill)),
        "the batch names the store path: {:?}",
        batch.paths
    );
    let notices = fixture.process(&WatchEvent::Batch(batch))?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].kind, DriftKind::StoreFileEditedWhileLinked);
    assert_eq!(notices[0].path, link, "the deployment path is reported");
    assert_eq!(notices[0].expected_hash.as_deref(), Some(baseline.as_str()));
    assert_eq!(
        notices[0].actual_hash.as_deref(),
        Some(skill_hash(&store_skill)?.as_str())
    );
    service.stop()?;
    Ok(())
}

/// Restores directory permissions even when an assertion fails.
struct RestorePermits(PathBuf);

impl Drop for RestorePermits {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_permission_denied_folder_is_unreadable_and_other_targets_keep_working()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new()?;
    let refused = fixture.path(".claude/skills/refused");
    fs::create_dir_all(&refused)?;
    fs::write(refused.join("SKILL.md"), SKILL_BODY)?;
    let baseline = skill_hash(&refused)?;
    let refused_id = ItemId::new();
    fixture.seed(
        &refused_id,
        &AgentId::ClaudeCode,
        "skill",
        &refused,
        "copy",
        Some(&baseline),
        None,
    )?;
    let healthy = fixture.path(".claude/skills/healthy");
    fs::create_dir_all(&healthy)?;
    fs::write(healthy.join("SKILL.md"), SKILL_BODY)?;
    let healthy_baseline = skill_hash(&healthy)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &healthy,
        "copy",
        Some(&healthy_baseline),
        None,
    )?;
    let service = fixture.service(vec![WatchRoot::dir(
        &fixture.skills,
        WatchRootKind::SkillsDir,
    )])?;

    let _restore = RestorePermits(refused.clone());
    fs::set_permissions(&refused, fs::Permissions::from_mode(0o000))?;

    // The attribute change and the sibling edit both keep the watcher live; the refused folder
    // is classified by the same baseline comparison the watcher overflow path uses.
    fs::write(
        healthy.join("SKILL.md"),
        b"---\nname: healthy\n---\nChanged\n",
    )?;
    let (_batch, _) = next_batch(&service, BUDGET).expect("the tree change produces a batch");
    let notices = fixture.reconcile()?;
    let refused_notice = notices
        .iter()
        .find(|notice| notice.path == refused)
        .ok_or("the refused folder must be reported")?;
    assert_eq!(refused_notice.kind, DriftKind::Unreadable);
    assert!(refused_notice.actual_hash.is_none());
    assert!(
        notices
            .iter()
            .any(|notice| notice.path == healthy && notice.kind == DriftKind::Drifted),
        "the readable target is still classified: {notices:?}"
    );
    service.stop()?;
    Ok(())
}

#[test]
fn a_burst_of_files_coalesces_within_the_budget() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let skill = fixture.path(".claude/skills/burst");
    fs::create_dir_all(&skill)?;
    fs::write(skill.join("SKILL.md"), SKILL_BODY)?;
    for index in 0..ASSETS {
        fs::write(skill.join(format!("asset-{index:03}.txt")), b"seed")?;
    }
    let baseline = skill_hash(&skill)?;
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "skill",
        &skill,
        "copy",
        Some(&baseline),
        None,
    )?;
    let service = fixture.service(vec![WatchRoot::dir(
        &fixture.skills,
        WatchRootKind::SkillsDir,
    )])?;

    let started = Instant::now();
    for index in 0..ASSETS {
        fs::write(skill.join(format!("asset-{index:03}.txt")), b"changed")?;
    }
    let (first, elapsed) = next_batch(&service, BUDGET).expect("a burst produces a batch");
    assert!(
        elapsed < BUDGET,
        "the burst batch arrived after {elapsed:?}"
    );

    // Drain until the queue goes quiet. A loaded runner can stretch the writes of one burst over
    // several debounce windows, so coalescing is checked by counting batches against the windows
    // the burst actually spanned, never against a fixed duration.
    let mut batches = vec![first];
    while let Some((batch, _)) = next_batch(&service, QUIET) {
        batches.push(batch);
    }
    let span = started.elapsed();

    let mut changed = std::collections::BTreeSet::new();
    for batch in &batches {
        for path in &batch.paths {
            if path != &skill && path.starts_with(&skill) {
                changed.insert(path.clone());
            }
        }
    }
    assert!(
        changed.len() >= ASSETS * 2 / 3,
        "the burst is reported across {} batches: {} of {ASSETS} files",
        batches.len(),
        changed.len()
    );
    // One debounce window yields at most one batch: a slow runner earns more windows, not more
    // batches per window. Uncoalesced writes would need hundreds of windows.
    let windows = spanning_windows(span);
    assert!(
        batches.len() as u128 <= windows + 2,
        "{} batches for a burst that took {span:?} ({windows} debounce windows)",
        batches.len()
    );

    let mut drifted = 0_usize;
    for batch in batches {
        for notice in fixture.process(&WatchEvent::Batch(batch))? {
            assert_eq!(notice.kind, DriftKind::Drifted, "{notice:?}");
            drifted += notice.changed_file_count;
        }
    }
    assert!(
        drifted >= ASSETS * 2 / 3,
        "the notices count the coalesced files: {drifted}"
    );
    service.stop()?;
    Ok(())
}

#[test]
fn an_agentuna_write_is_suppressed_and_produces_no_drift() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new()?;
    let original = br#"{"mcpServers":{"example":{"command":"node"}}}"#;
    let path = fixture.write(".claude.json", original)?;
    let baseline = file_hash(original);
    fixture.seed(
        &ItemId::new(),
        &AgentId::ClaudeCode,
        "mcp_server",
        &path,
        "native",
        Some(&baseline),
        None,
    )?;
    let registry = Arc::new(RecentWrites::new());
    let service = WatchService::new(
        WatchConfig::new([WatchRoot::file(&path, WatchRootKind::McpFile)])
            .with_registry(Arc::clone(&registry)),
    );
    service.start()?;
    std::thread::sleep(SETTLE);

    let written = br#"{"mcpServers":{"example":{"command":"node","args":["-y"]}}}"#;
    let outcome = at_fs::apply(&WriteRequest::replace(
        &path,
        written.to_vec(),
        at_fs::ExpectedState::Hash(baseline.clone()),
    ))?;
    registry.record_outcome(&path, &outcome);

    let events: Vec<WatchEvent> = {
        // An absence assertion: the longer the window, the stronger, and a loaded runner may
        // deliver the suppressed event late.
        let deadline = Instant::now() + QUIET;
        let mut events = Vec::new();
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match service.next_event(remaining) {
                Some(event) => events.push(event),
                None => break,
            }
        }
        events
    };
    for event in &events {
        if let Some(batch) = event.batch() {
            assert!(
                !batch.contains(&path),
                "an AgenTuna write must not reach a batch: {:?}",
                batch.paths
            );
        }
        assert!(
            fixture.process(event)?.is_empty(),
            "an AgenTuna write must not produce drift"
        );
    }
    assert_eq!(fs::read(&path)?, written, "the write itself succeeded");
    service.stop()?;
    Ok(())
}

#[test]
fn drift_detection_declares_no_network_dependency() -> Result<(), Box<dyn std::error::Error>> {
    // The detection path is the engine's drift service plus the filesystem watcher: neither may
    // own an HTTP, TLS, socket or git client, which is what keeps detection offline.
    const NETWORK_CLIENTS: &[&str] = &[
        "attohttpc",
        "curl",
        "git2",
        "hyper",
        "libgit2",
        "native-tls",
        "openssl",
        "reqwest",
        "rustls",
        "ssh2",
        "tokio-tungstenite",
        "tonic",
        "ureq",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("at-engine lives in crates/at-engine")?;
    for krate in ["at-engine", "at-fs"] {
        let manifest = fs::read_to_string(root.join(krate).join("Cargo.toml"))?;
        for client in NETWORK_CLIENTS {
            assert!(
                !manifest.contains(client),
                "{krate} must not depend on the network client `{client}`"
            );
        }
    }
    Ok(())
}

#[test]
fn drift_targets_are_read_from_the_deployment_index() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let path = fixture.write(".claude.json", b"{}")?;
    let id = ItemId::new();
    fixture.seed(
        &id,
        &AgentId::ClaudeCode,
        "mcp_server",
        &path,
        "native",
        None,
        None,
    )?;
    let targets = fixture.targets()?;
    let target = targets
        .iter()
        .find(|target| target.path == path)
        .ok_or("the seeded deployment becomes a watcher target")?;
    assert_eq!(target.item_id, Some(id));
    assert_eq!(target.agent_id, AgentId::ClaudeCode);
    assert_eq!(target.scope, Scope::User);
    assert!(
        !target.store_side_of_link,
        "a native copy is not a store link"
    );
    Ok(())
}
