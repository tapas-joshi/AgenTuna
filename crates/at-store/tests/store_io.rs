//! TC-003: locked store load/save, drift classification, orphan/missing reconciliation.
//!
//! Every test runs inside a `tempfile::tempdir` store layout; nothing touches a real
//! home, agent folder or the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use at_fs::BackupPolicy;
use at_model::{ItemId, ItemKind, Scope};
use at_store::instance::LockPolicy;
use at_store::layout::{Slug, StoreId, StoreLayout};
use at_store::manifest::{
    ContentHash, DisabledAgents, ManifestItem, StoreManifest, StorePath, hash_mcp_entry,
    hash_skill_folder,
};
use at_store::store_io::{
    McpFileState, ReconcileMode, ReconcileOutcome, StoreAccess, StoreIoError, StoreItemState,
    load_store, lock_exclusive, reconcile, reconcile_with, save_store, save_store_with,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[allow(dead_code)]
const POLICY_FAST: LockPolicy = LockPolicy {
    wait: Duration::from_millis(200),
    poll: Duration::from_millis(10),
};

/// A tempdir plus its initialized User-scope store layout.
struct Fixture {
    _dir: TempDir,
    layout: StoreLayout,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = StoreLayout::new(Scope::User, dir.path().join("store"));
        layout.init().expect("store initializes");
        Self { _dir: dir, layout }
    }

    #[allow(dead_code)]
    fn root(&self) -> &Path {
        self.layout.root()
    }

    fn skill_dir(&self, slug: &str) -> PathBuf {
        let dir = self.layout.skills_dir().join(slug);
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {slug}\ndescription: fixture\n---\n\n# {slug}\n"),
        )
        .expect("SKILL.md");
        dir
    }

    /// Registers `slug` in the manifest with the hash of its current folder.
    fn add_skill(&self, manifest: &mut StoreManifest, slug: &str) -> ItemId {
        let id = ItemId::new();
        let hash = hash_skill_folder(&self.layout.skills_dir().join(slug)).expect("hash skill");
        manifest
            .insert_item(ManifestItem::new(
                id,
                ItemKind::Skill,
                Slug::parse(slug).expect("slug"),
                StorePath::parse(&format!("skills/{slug}")).expect("path"),
                hash,
            ))
            .expect("insert item");
        id
    }

    fn save(&self, manifest: &StoreManifest, observed: &ContentHash) {
        save_store(
            &self.layout,
            manifest,
            observed,
            StoreAccess::ReadWrite,
            BackupPolicy::None,
        )
        .expect("save_store");
    }
}

fn write_mcp(layout: &StoreLayout, servers: &Value) {
    let path = layout.mcp_file();
    fs::create_dir_all(path.parent().expect("parent")).expect("mcp parent");
    let document = json!({ "schema_version": 1, "servers": servers });
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&document).unwrap()),
    )
    .expect("write mcp.json");
}

fn state_of(snapshot: &at_store::store_io::StoreSnapshot, slug: &str) -> Option<StoreItemState> {
    snapshot
        .items
        .iter()
        .find(|entry| entry.item.slug.as_str() == slug)
        .map(|entry| entry.state)
}

/// A store whose manifest lists `alpha` (Present) — the baseline for drift tests.
fn populated() -> Fixture {
    let fixture = Fixture::new();
    fixture.skill_dir("alpha");
    let loaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("load");
    let mut manifest = loaded.manifest.clone();
    fixture.add_skill(&mut manifest, "alpha");
    fixture.save(&manifest, &loaded.manifest_hash);
    fixture
}

#[test]
fn load_classifies_present_modified_and_missing_items() {
    let fixture = populated();

    fixture.skill_dir("beta");
    fixture.skill_dir("gamma");
    let loaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("reload");
    let mut manifest = loaded.manifest.clone();
    fixture.add_skill(&mut manifest, "beta");
    fixture.add_skill(&mut manifest, "gamma");
    fixture.save(&manifest, &loaded.manifest_hash);

    fs::remove_dir_all(fixture.layout.skills_dir().join("beta")).expect("remove beta");
    fs::write(
        fixture.layout.skills_dir().join("gamma/SKILL.md"),
        "---\nname: gamma\ndescription: changed\n---\n\n# gamma\n",
    )
    .expect("modify gamma");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    assert_eq!(state_of(&snapshot, "alpha"), Some(StoreItemState::Present));
    assert_eq!(state_of(&snapshot, "beta"), Some(StoreItemState::Missing));
    assert_eq!(state_of(&snapshot, "gamma"), Some(StoreItemState::Modified));
    assert!(snapshot.has_drift());
}

#[test]
fn unknown_skill_folder_and_rule_file_are_orphans() {
    let fixture = populated();
    fixture.skill_dir("mystery");
    let rules = fixture.layout.rules_dir();
    fs::create_dir_all(&rules).expect("rules dir");
    fs::write(rules.join("stray.md"), "# stray\n").expect("stray rule");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    let orphan_slugs: Vec<&str> = snapshot
        .orphans
        .iter()
        .map(|orphan| orphan.slug.as_str())
        .collect();
    assert!(orphan_slugs.contains(&"mystery"));
    assert!(orphan_slugs.contains(&"stray"));
    assert!(
        snapshot
            .orphans
            .iter()
            .all(|orphan| orphan.kind != ItemKind::McpServer)
    );
}

#[test]
fn mcp_servers_reconcile_as_orphan_missing_and_modified() {
    let fixture = populated();
    let mcp_hash = |entry: &Value| hash_mcp_entry(entry).expect("hash entry");

    let loaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("load for mcp");
    let mut manifest = loaded.manifest.clone();
    let managed_entry = json!({ "command": "run", "args": ["--flag"] });
    manifest
        .insert_item(ManifestItem::new(
            ItemId::new(),
            ItemKind::McpServer,
            Slug::parse("managed").expect("slug"),
            StorePath::parse("mcp.json").expect("path"),
            mcp_hash(&managed_entry),
        ))
        .expect("insert mcp item");
    manifest
        .insert_item(ManifestItem::new(
            ItemId::new(),
            ItemKind::McpServer,
            Slug::parse("ghost").expect("slug"),
            StorePath::parse("mcp.json").expect("path"),
            mcp_hash(&json!({ "command": "gone" })),
        ))
        .expect("insert ghost");
    fixture.save(&manifest, &loaded.manifest_hash);

    write_mcp(
        &fixture.layout,
        &json!({
            "managed": { "command": "run", "args": ["--flag"] },
            "stray": { "url": "https://example.invalid/mcp" }
        }),
    );

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    assert!(matches!(snapshot.mcp, McpFileState::Ok));
    assert_eq!(
        state_of(&snapshot, "managed"),
        Some(StoreItemState::Present)
    );
    assert_eq!(state_of(&snapshot, "ghost"), Some(StoreItemState::Missing));
    assert!(
        snapshot
            .orphans
            .iter()
            .any(|orphan| orphan.kind == ItemKind::McpServer && orphan.slug == "stray")
    );

    write_mcp(
        &fixture.layout,
        &json!({
            "managed": { "command": "run", "args": ["--other"] },
            "stray": { "url": "https://example.invalid/mcp" }
        }),
    );
    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot 2");
    assert_eq!(
        state_of(&snapshot, "managed"),
        Some(StoreItemState::Modified)
    );
}

#[test]
fn external_store_json_write_between_load_and_save_is_concurrent_modification() {
    let fixture = populated();
    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("load");

    // A second process (here: a shell) appends to store.json between load and save.
    let manifest_path = fixture.layout.manifest_path();
    let status = Command::new("sh")
        .args([
            "-c",
            &format!("printf '\\n# touched\\n' >> '{}'", manifest_path.display()),
        ])
        .status()
        .expect("spawn writer");
    assert!(status.success());

    let mut manifest = snapshot.manifest.clone();
    manifest
        .disable(ItemId::new(), DisabledAgents::All)
        .expect_err("unrelated id cannot disable");
    let error = save_store(
        &fixture.layout,
        &manifest,
        &snapshot.manifest_hash,
        StoreAccess::ReadWrite,
        BackupPolicy::None,
    )
    .expect_err("save must fail");
    assert!(
        matches!(error, StoreIoError::ConcurrentModification { .. }),
        "expected ConcurrentModification, got {error:?}"
    );

    // The first process's change was not applied: the file still ends with the
    // external writer's marker.
    let text = fs::read_to_string(&manifest_path).expect("read manifest");
    assert!(text.contains("# touched"), "external bytes intact");
}

#[test]
fn lock_contention_times_out_after_five_seconds_without_writing() {
    let fixture = Fixture::new();
    let exclusive = lock_exclusive(
        &fixture.layout,
        StoreAccess::ReadWrite,
        LockPolicy {
            wait: Duration::from_secs(30),
            poll: Duration::from_millis(10),
        },
    )
    .expect("exclusive lock")
    .expect("guard");

    let contender_layout = fixture.layout.clone();
    let started = Instant::now();
    let handle = thread::spawn(move || {
        save_store_with(
            &contender_layout,
            &StoreManifest::new(Scope::User, StoreId::new()),
            &ContentHash::from_bytes(b"observed"),
            StoreAccess::ReadWrite,
            BackupPolicy::None,
            LockPolicy {
                wait: Duration::from_secs(5),
                poll: Duration::from_millis(50),
            },
        )
    });
    let result = handle.join().expect("contender thread");
    let elapsed = started.elapsed();
    drop(exclusive);
    assert!(
        matches!(result, Err(StoreIoError::LockTimeout { .. })),
        "expected LockTimeout, got {result:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(30),
        "timeout should land around five seconds, took {elapsed:?}"
    );
}

#[test]
fn report_mode_reconcile_leaves_orphan_and_mcp_bytes_identical() {
    let fixture = populated();
    fixture.skill_dir("mystery");
    let mcp_path = fixture.layout.mcp_file();
    let mcp_before = fs::read(&mcp_path).expect("mcp bytes");
    let orphan_bytes =
        fs::read(fixture.layout.skills_dir().join("mystery/SKILL.md")).expect("orphan bytes");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    let plan = reconcile(&snapshot).expect("plan");
    assert_eq!(plan.imports.len(), 1);
    assert_eq!(plan.imports[0].slug.as_str(), "mystery");
    assert_eq!(plan.drops.len(), 0);
    assert!(!plan.is_empty());

    // Report mode wrote nothing.
    assert_eq!(
        fs::read(&mcp_path).expect("mcp bytes after"),
        mcp_before,
        "mcp.json byte-identical"
    );
    assert_eq!(
        fs::read(fixture.layout.skills_dir().join("mystery/SKILL.md")).expect("orphan still there"),
        orphan_bytes,
        "orphan file byte-identical"
    );
}

#[test]
fn missing_item_stays_in_manifest_until_confirmed_drop() {
    let fixture = populated();
    fs::remove_dir_all(fixture.layout.skills_dir().join("alpha")).expect("remove alpha");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    let plan = reconcile(&snapshot).expect("plan");
    assert_eq!(plan.drops.len(), 1);
    assert_eq!(plan.imports.len(), 0);

    // A wrong token changes nothing.
    let wrong = at_store::store_io::ReconcileToken(ContentHash::from_bytes(b"nope"));
    let error = reconcile_with(&snapshot, ReconcileMode::Apply { token: wrong })
        .expect_err("wrong token refused");
    assert!(matches!(error, StoreIoError::StaleToken));
    let reloaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("reload");
    assert_eq!(reloaded.manifest.items().len(), 1, "missing item retained");

    // The right token drops exactly the missing entry.
    let outcome = reconcile_with(
        &snapshot,
        ReconcileMode::Apply {
            token: plan.token.clone(),
        },
    )
    .expect("apply");
    assert!(matches!(outcome, ReconcileOutcome::Applied { .. }));
    let reloaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("reload 2");
    assert!(reloaded.manifest.items().is_empty());
}

#[test]
fn apply_imports_orphans_without_deleting_files() {
    let fixture = populated();
    fixture.skill_dir("mystery");
    let orphan_file = fixture.layout.skills_dir().join("mystery/SKILL.md");
    let before = fs::read(&orphan_file).expect("orphan bytes");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("snapshot");
    let plan = reconcile(&snapshot).expect("plan");
    assert_eq!(plan.imports.len(), 1);
    reconcile_with(
        &snapshot,
        ReconcileMode::Apply {
            token: plan.token.clone(),
        },
    )
    .expect("apply");

    let reloaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("reload");
    let imported = reloaded
        .manifest
        .item_by_slug(ItemKind::Skill, &Slug::parse("mystery").expect("slug"))
        .expect("imported item");
    assert_eq!(imported.path.as_str(), "skills/mystery");
    assert_eq!(
        fs::read(&orphan_file).expect("orphan bytes after"),
        before,
        "orphan file is never deleted or rewritten"
    );
    let entry = reloaded
        .items
        .iter()
        .find(|entry| entry.item.id == imported.id)
        .expect("classified");
    assert_eq!(entry.state, StoreItemState::Present);
}

#[test]
fn read_only_access_never_creates_lock_or_writes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let layout = StoreLayout::new(Scope::User, dir.path().join("readonly-store"));
    layout.init().expect("init");
    fs::remove_dir_all(layout.state_dir()).expect("remove state");

    let snapshot = load_store(&layout, StoreAccess::ReadOnly).expect("read-only load");
    assert!(!layout.state_dir().exists(), "read-only created .state");
    assert_eq!(snapshot.access, StoreAccess::ReadOnly);

    let error = save_store(
        &layout,
        &snapshot.manifest,
        &snapshot.manifest_hash,
        StoreAccess::ReadOnly,
        BackupPolicy::None,
    )
    .expect_err("read-only save refused");
    assert!(matches!(error, StoreIoError::ReadOnly { .. }));
    assert!(!layout.state_dir().exists());
}

#[test]
fn duplicate_tombstones_collapse_to_one_on_load() {
    let fixture = populated();
    let loaded = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("load");
    let mut manifest = loaded.manifest.clone();
    let alpha = manifest
        .items()
        .iter()
        .find(|item| item.slug.as_str() == "alpha")
        .expect("alpha")
        .id;
    manifest
        .disable(alpha, DisabledAgents::All)
        .expect("disable");
    fixture.save(&manifest, &loaded.manifest_hash);

    // Hand-edit store.json to duplicate the tombstone row, as an older writer could.
    let path = fixture.layout.manifest_path();
    let mut value: Value =
        serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse store.json");
    let disabled = value["disabled"]
        .as_array()
        .expect("disabled array")
        .clone();
    value["disabled"]
        .as_array_mut()
        .expect("disabled array")
        .extend(disabled);
    fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("serialize"),
    )
    .expect("write duplicate tombstones");

    let snapshot = load_store(&fixture.layout, StoreAccess::ReadWrite).expect("load dupes");
    assert_eq!(snapshot.manifest.disabled().len(), 1, "collapsed on load");
}
