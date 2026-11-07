//! TC-011: single-item adoption of unmanaged native entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use at_db::{Database, ops};
use at_fs::ApplyRoots;
use at_migrate::adopt::{
    AdoptError, AdoptHandling, AdoptPreviewRequest, AdoptRequest, PathState, adopt_item,
    preview_adopt,
};
use at_migrate::apply::{UndoOperationRequest, UndoOutcome, undo_operation};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: PathBuf,
    backups: PathBuf,
    database: Database,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().expect("temporary root");
    let root = fs::canonicalize(temp.path()).expect("canonical root");
    let store = root.join("store");
    let backups = root.join("backups");
    fs::create_dir_all(&backups).expect("backup root");
    let database = Database::open_in_memory().expect("database");
    Fixture {
        _temp: temp,
        root,
        store,
        backups,
        database,
    }
}

fn skill_item(fixture: &Fixture, relative: &str, name: &str, agent: AgentId) -> ScanItem {
    let root = fixture.root.join(relative);
    fs::create_dir_all(&root).expect("skill root");
    fs::write(root.join("SKILL.md"), format!("# {name}\nbody\n")).expect("skill document");
    let normalized = at_store::manifest::hash_skill_folder(&root)
        .expect("skill hash")
        .to_string();
    ScanItem {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        agents: BTreeSet::from([agent]),
        path: root,
        scope: Scope::User,
        content_hash: Some(at_fs::sha256_hex(b"raw tree")),
        normalized_hash: Some(normalized),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    }
}

fn report(items: Vec<ScanItem>) -> ScanReport {
    ScanReport {
        roots: Vec::new(),
        items,
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    }
}

fn roots(fixture: &Fixture) -> ApplyRoots {
    ApplyRoots::new([fixture.root.clone()]).expect("authorized root")
}

fn tree_hash(path: &Path) -> String {
    at_fs::changeset::hash_node(path)
        .expect("hash tree")
        .expect("tree exists")
}

#[test]
fn adopting_a_scanned_skill_creates_the_store_item_and_leaves_a_symlink() {
    let mut fixture = fixture();
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let before = tree_hash(&item.path);
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt skill");

    assert_eq!(outcome.slug, "sample-skill");
    let store_item = fixture.store.join("skills/sample-skill");
    assert_eq!(
        tree_hash(&store_item),
        before,
        "store item has the adopted content"
    );
    let metadata = fs::symlink_metadata(&item.path).expect("native path");
    assert!(metadata.file_type().is_symlink(), "original is a link");
    assert_eq!(
        item.path.canonicalize().expect("link target"),
        store_item.canonicalize().expect("store item")
    );

    // Ownership is recorded: a deployment for the agent and an adopted native row.
    let item_id = at_db::items::item_id_by_identity(
        fixture.database.connection(),
        ItemKind::Skill,
        Scope::User,
        None,
        "sample-skill",
    )
    .expect("item lookup")
    .expect("item row");
    let deployments =
        at_db::deployments::list_deployments_for_item(fixture.database.connection(), &item_id)
            .expect("deployments");
    assert_eq!(deployments.len(), 1);
    assert_eq!(deployments[0].mode.as_deref(), Some("symlink"));
    let native_path = item.path.to_string_lossy().into_owned();
    let native = at_db::native::native_item_by_path(fixture.database.connection(), &native_path)
        .expect("native lookup")
        .expect("native row");
    assert_eq!(native.status, at_db::native::NativeStatus::Adopted);
    assert_eq!(
        native.adopted_item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );

    // The operation recorded a backup of the original tree before replacing it.
    let detail = ops::operation_detail(fixture.database.connection(), &outcome.operation_id)
        .expect("operation detail")
        .expect("operation row");
    assert!(
        detail.files.iter().any(|file| file.backup_id.is_some()),
        "the replaced original has an operation backup"
    );
}

#[test]
fn an_adopt_after_the_file_changed_returns_stale_scan_record_and_writes_nothing() {
    let mut fixture = fixture();
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let scan = report(vec![item.clone()]);
    fs::write(item.path.join("SKILL.md"), b"# Changed after scan\n").expect("external edit");
    let changed_hash = at_store::manifest::hash_skill_folder(&item.path)
        .expect("hash")
        .to_string();
    let authorized = roots(&fixture);
    let error = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect_err("stale scan record");
    match error {
        AdoptError::StaleScanRecord { path, current } => {
            assert_eq!(path, item.path);
            assert_eq!(current, PathState::Present(changed_hash));
        }
        other => panic!("expected StaleScanRecord, got {other:?}"),
    }
    assert!(
        !fixture.store.join("skills/sample-skill").exists(),
        "no store item was created"
    );
    assert!(
        !fs::symlink_metadata(&item.path)
            .expect("native path")
            .file_type()
            .is_symlink()
    );
    assert!(
        ops::recent_changes(
            fixture.database.connection(),
            &ops::RecentChangesQuery::new(5)
        )
        .expect("recent changes")
        .is_empty(),
        "no operation was recorded"
    );
}

#[test]
fn an_adopt_of_a_vanished_file_reports_the_missing_state() {
    let mut fixture = fixture();
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let scan = report(vec![item.clone()]);
    fs::remove_dir_all(&item.path).expect("vanished item");
    let authorized = roots(&fixture);
    let error = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect_err("vanished scan record");
    assert!(matches!(
        error,
        AdoptError::StaleScanRecord {
            current: PathState::Missing,
            ..
        }
    ));
}

#[test]
fn adopting_an_mcp_server_patches_only_that_entry_and_keeps_comments() {
    let mut fixture = fixture();
    let config = fixture.root.join("native/codex/config.toml");
    fs::create_dir_all(config.parent().expect("parent")).expect("codex folder");
    let original = b"# keep this comment\nmodel = \"gpt-5\"\n\n[mcp_servers.alpha]\ncommand = \"alpha\"\n\n[mcp_servers.example]\ncommand = \"node\"\nargs = [\"server.js\"]\n";
    fs::write(&config, original).expect("codex config");
    let entries = at_migrate::scan::read_mcp_entries(&config, original).expect("entries");
    let entry_hash = at_migrate::normalize::mcp_entry(&entries["example"])
        .expect("entry hash")
        .to_string();
    let item = ScanItem {
        kind: ItemKind::McpServer,
        name: "example".to_owned(),
        agents: BTreeSet::from([AgentId::Codex]),
        path: config.clone(),
        scope: Scope::User,
        content_hash: Some(entry_hash.clone()),
        normalized_hash: Some(entry_hash),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    let preview = preview_adopt(&AdoptPreviewRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        scope: Scope::User,
        handling: AdoptHandling::Link,
    })
    .expect("preview");
    assert_eq!(preview.store_destination, fixture.store.join("mcp.json"));
    assert_eq!(preview.slug, "example");
    assert_eq!(preview.native_name, "example");

    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt mcp");
    assert_eq!(outcome.store_destination, fixture.store.join("mcp.json"));

    let canonical = fs::read_to_string(fixture.store.join("mcp.json")).expect("canonical mcp");
    let canonical_json: serde_json::Value =
        serde_json::from_str(&canonical).expect("canonical json");
    assert!(
        canonical_json["servers"]["example"].is_object(),
        "canonical: {canonical}"
    );
    assert!(canonical_json["servers"].get("alpha").is_none());

    let patched = fs::read_to_string(&config).expect("patched config");
    assert!(patched.contains("# keep this comment"), "comment kept");
    assert!(patched.contains("model = \"gpt-5\""), "unrelated key kept");
    assert!(patched.contains("[mcp_servers.alpha]"), "other server kept");
    assert!(
        patched.contains("command = \"alpha\""),
        "other server body kept"
    );
    assert!(
        patched.contains("[mcp_servers.example]"),
        "adopted entry kept"
    );
    let reparsed =
        at_migrate::scan::read_mcp_entries(&config, patched.as_bytes()).expect("reparsed entries");
    assert_eq!(reparsed.len(), 2, "both servers remain");
}

#[test]
fn the_adopt_operation_id_is_accepted_by_undo_and_restores_the_pre_adopt_tree() {
    let mut fixture = fixture();
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let before = tree_hash(&item.path);
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt skill");

    let result = undo_operation(
        UndoOperationRequest {
            connection: fixture.database.connection(),
            store_root: &fixture.store,
            backup_root: &fixture.backups,
            authorized_roots: &authorized,
            actor: ops::OperationActor::Ui,
            force: false,
        },
        &outcome.operation_id,
    )
    .expect("undo adopt");
    assert!(matches!(result, UndoOutcome::Restored { .. }));
    assert_eq!(tree_hash(&item.path), before, "original tree restored");
    assert!(
        !fs::symlink_metadata(&item.path)
            .expect("restored tree")
            .file_type()
            .is_symlink()
    );
    assert!(!fixture.store.join("skills/sample-skill").exists());
    let native_path = item.path.to_string_lossy().into_owned();
    let native = at_db::native::native_item_by_path(fixture.database.connection(), &native_path)
        .expect("native lookup")
        .expect("native row");
    assert_eq!(native.status, at_db::native::NativeStatus::New);
    assert!(native.adopted_item_id.is_none());
}

#[test]
fn a_name_collision_chooses_a_suffixed_slug_for_store_and_agent_copies() {
    let mut fixture = fixture();
    let first = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let second = skill_item(
        &fixture,
        "native/codex/skills/sample-skill",
        "sample-skill",
        AgentId::Codex,
    );
    let scan = report(vec![first.clone(), second.clone()]);
    let authorized = roots(&fixture);

    let first_outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &first.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt first copy");
    assert_eq!(first_outcome.slug, "sample-skill");

    // The store copy exists, so the second copy is adopted under the next free slug.
    let preview = preview_adopt(&AdoptPreviewRequest {
        scan: &scan,
        path: &second.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        scope: Scope::User,
        handling: AdoptHandling::Link,
    })
    .expect("preview second copy");
    assert_eq!(preview.slug, "sample-skill-2");
    assert!(preview.renamed);
    let second_outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &second.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt second copy");
    assert_eq!(second_outcome.slug, "sample-skill-2");
    assert!(fixture.store.join("skills/sample-skill").is_dir());
    assert!(fixture.store.join("skills/sample-skill-2").is_dir());
}

#[test]
fn watch_handling_leaves_the_original_in_place_and_records_it() {
    let mut fixture = fixture();
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let before = tree_hash(&item.path);
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Watch,
        project_id: None,
    })
    .expect("adopt watched skill");
    assert_eq!(tree_hash(&item.path), before, "original untouched");
    assert!(
        !fs::symlink_metadata(&item.path)
            .expect("native path")
            .file_type()
            .is_symlink()
    );
    assert!(fixture.store.join("skills/sample-skill/SKILL.md").is_file());
    assert_eq!(outcome.handling, AdoptHandling::Watch);
}

#[test]
fn an_already_managed_item_is_refused() {
    let mut fixture = fixture();
    let mut item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    item.store_link = true;
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    let error = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect_err("already managed");
    assert!(matches!(error, AdoptError::AlreadyManaged(_)));
}

#[test]
fn adopt_initialises_a_missing_store() {
    let mut fixture = fixture();
    assert!(!fixture.store.join("store.json").exists());
    let item = skill_item(
        &fixture,
        "native/claude/skills/sample-skill",
        "sample-skill",
        AgentId::ClaudeCode,
    );
    let scan = report(vec![item.clone()]);
    let authorized = roots(&fixture);
    adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        name: None,
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &authorized,
        database: &mut fixture.database,
        scope: Scope::User,
        handling: AdoptHandling::Link,
        project_id: None,
    })
    .expect("adopt into a new store");
    assert!(fixture.store.join("store.json").is_file());
    let manifest = at_store::manifest::StoreManifest::load(&at_store::layout::StoreLayout::new(
        Scope::User,
        &fixture.store,
    ))
    .expect("manifest");
    assert_eq!(manifest.manifest.items().len(), 1);
    let _ = BTreeMap::<String, String>::new();
}
