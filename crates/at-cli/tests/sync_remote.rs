//! TC-031 end-to-end tests: `agentuna sync` and `agentuna remote` in a sandbox.
//!
//! Every test runs the real binary with all roots redirected into a temporary directory and
//! `AGENTUNA_SECRET_STORE=memory`, so no test reads the developer's home, store, agent folders,
//! Keychain or the network.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use at_db::Database;
use at_model::{AgentId, ItemId, ItemKind, ItemState, Scope, SyncLabel};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::{ManifestItem, StoreManifest, StorePath};
use at_store::skill_package::SkillPackage;
use serde_json::Value;
use sha2::{Digest, Sha256};

use support::{Sandbox, exit_code, stderr, stdout};

const PROJECT_DOC: &[u8] =
    b"---\nname: alpha\ndescription: Project copy\n---\n\n# Alpha\n\nProject copy.\n";
const LIBRARY_DOC: &[u8] =
    b"---\nname: alpha\ndescription: Library copy\n---\n\n# Alpha\n\nLibrary copy.\n";

/// Which side holds the recorded baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Baseline {
    Project,
    Library,
}

/// A sandbox with one `alpha` skill in both stores and a project sync baseline.
struct Fixture {
    sandbox: Sandbox,
    project: PathBuf,
    item_id: ItemId,
    project_hash: String,
    library_hash: String,
    project_store: StoreLayout,
    user_store: StoreLayout,
}

/// Runs the real binary with the sandbox environment, a memory credential store and no colour.
fn agentuna(sandbox: &Sandbox, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentuna"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("AGENTUNA_SECRET_STORE", "memory")
        .envs(sandbox.vars())
        .current_dir(sandbox.cwd())
        .output()
        .expect("spawn the agentuna binary")
}

/// Writes a file, creating parents.
fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// Recursive path-to-content snapshot used to prove a command wrote nothing.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(current: &Path, root: &Path, into: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).expect("metadata");
            if metadata.file_type().is_symlink() {
                into.insert(
                    relative,
                    format!("symlink:{}", fs::read_link(&path).expect("link").display()),
                );
            } else if metadata.is_dir() {
                into.insert(relative, "dir".to_owned());
                walk(&path, root, into);
            } else {
                let digest = Sha256::digest(fs::read(&path).expect("read"));
                let hex = digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                into.insert(relative, format!("file:{hex}"));
            }
        }
    }
    let mut into = BTreeMap::new();
    walk(root, root, &mut into);
    into
}

/// Writes one skill package and records it in the store manifest with a chosen identity.
fn write_skill(store: &StoreLayout, slug_text: &str, doc: &[u8], item_id: ItemId) {
    let slug = Slug::parse(slug_text).expect("slug");
    let dir = store.skill_dir(&slug);
    write(&dir.join("SKILL.md"), doc);
    let hash = SkillPackage::read(&dir).expect("read package").hash;
    let loaded = StoreManifest::load(store).expect("manifest");
    let mut manifest = loaded.manifest;
    manifest
        .insert_item(ManifestItem::new(
            item_id,
            ItemKind::Skill,
            slug,
            StorePath::parse(&format!("skills/{slug_text}")).expect("store path"),
            hash,
        ))
        .expect("manifest item");
    fs::write(
        store.manifest_path(),
        manifest.to_bytes().expect("manifest bytes"),
    )
    .expect("write manifest");
}

/// Seeds the project, both stores, the item row and the deployment baseline.
fn fixture(baseline: Baseline) -> Fixture {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project dir");
    let project_id = {
        let database = Database::open_file(sandbox.paths().db_path()).expect("index");
        at_db::projects::add_project(database.connection(), &project, Some("demo"), None)
            .expect("register project")
            .id
    };

    let project_store = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, project_store.root()).expect("project store");
    let user_store = StoreLayout::new(Scope::User, sandbox.user_store());
    init_store(Scope::User, user_store.root()).expect("user store");

    let item_id = ItemId::new();
    write_skill(&project_store, "alpha", PROJECT_DOC, item_id);
    write_skill(&user_store, "alpha", LIBRARY_DOC, item_id);
    let project_hash =
        SkillPackage::read(&project_store.skill_dir(&Slug::parse("alpha").expect("slug")))
            .expect("project package")
            .hash
            .to_string();
    let library_hash =
        SkillPackage::read(&user_store.skill_dir(&Slug::parse("alpha").expect("slug")))
            .expect("library package")
            .hash
            .to_string();
    // TA-005 stores the relation it derived; the consumer reads it verbatim and never
    // recomputes it from these hashes.
    let (baseline_hash, sync_label) = match baseline {
        Baseline::Project => (project_hash.clone(), SyncLabel::ProjectNewer),
        Baseline::Library => (library_hash.clone(), SyncLabel::LibraryNewer),
    };

    let database = Database::open_file(sandbox.paths().db_path()).expect("index");
    let connection = database.connection();
    connection
        .execute(
            "INSERT OR IGNORE INTO agents (id, display_name, kind, version, installed) \
             VALUES ('claude_code', 'Claude Code', 'claude_code', '2.0.0', 1)",
            [],
        )
        .expect("agent row");
    connection
        .execute(
            "INSERT INTO items (id, kind, scope, project_id, slug, name, state, managed) \
             VALUES (?1, 'skill', 'project', ?2, 'alpha', 'alpha', 'present', 1)",
            [item_id.to_string(), project_id.clone()],
        )
        .expect("item row");
    at_db::deployments::upsert_deployment(
        connection,
        &at_db::deployments::DeploymentSpec::new(
            &item_id,
            &AgentId::ClaudeCode,
            Scope::Project,
            ItemState::Linked,
        )
        .with_project(&project_id)
        .with_mode("native")
        .with_target_path(
            project_store
                .skill_dir(&Slug::parse("alpha").expect("slug"))
                .to_str()
                .expect("utf8"),
        )
        .with_hashes(
            Some(&project_hash),
            Some(&library_hash),
            Some(&baseline_hash),
        )
        .with_sync_label(sync_label),
    )
    .expect("deployment row");
    drop(database);

    Fixture {
        sandbox,
        project,
        item_id,
        project_hash,
        library_hash,
        project_store,
        user_store,
    }
}

/// The recorded baseline hash of the project deployment row.
fn baseline_hash(sandbox: &Sandbox, item_id: ItemId) -> Option<String> {
    let database = Database::open_file(sandbox.paths().db_path()).expect("index");
    at_db::deployments::list_deployments_for_item(database.connection(), &item_id)
        .expect("deployments")
        .into_iter()
        .find(|row| row.scope == Scope::Project)
        .and_then(|row| row.baseline_hash)
}

/// Configures one remote repository URL, the way the desktop app stores it.
fn configure_remote(sandbox: &Sandbox, repo: &Path) {
    let database = Database::open_file(sandbox.paths().db_path()).expect("index");
    let mut state =
        at_db::remote_backup::remote_backup_state(database.connection()).expect("remote state");
    state.repo_url = Some(repo.display().to_string());
    at_db::remote_backup::save_remote_backup_state(database.connection(), &state).expect("save");
}

/// The stored last successful backup commit.
fn last_commit(sandbox: &Sandbox) -> Option<String> {
    let database = Database::open_file(sandbox.paths().db_path()).expect("index");
    at_db::remote_backup::remote_backup_state(database.connection())
        .expect("remote state")
        .last_commit
}

/// Parses stdout as exactly one JSON envelope.
fn envelope(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).unwrap_or_else(|error| {
        panic!(
            "one JSON envelope expected ({error}); stderr: {}\nstdout: {}",
            stderr(output),
            stdout(output)
        )
    })
}

/// Asserts the process exit status and returns the envelope.
fn assert_exit(output: &Output, expected: i32) -> Value {
    let actual = exit_code(output);
    let parsed = envelope(output);
    assert_eq!(
        actual,
        expected,
        "exit {expected} expected; stderr: {}\nstdout: {}",
        stderr(output),
        stdout(output)
    );
    parsed
}

/// Writes the project and user store snapshots into one map.
fn stores(fixture: &Fixture) -> BTreeMap<String, String> {
    let mut all = BTreeMap::new();
    for (prefix, root) in [
        ("project", fixture.project_store.root()),
        ("user", fixture.user_store.root()),
    ] {
        for (path, hash) in snapshot(root) {
            all.insert(format!("{prefix}/{path}"), hash);
        }
    }
    all
}

fn project_arg(fixture: &Fixture) -> &str {
    fixture.project.to_str().expect("utf8 project path")
}

#[test]
fn sync_status_reports_the_label_and_evidence_hashes_without_writing() {
    let fixture = fixture(Baseline::Project);
    let before = stores(&fixture);
    let output = agentuna(
        &fixture.sandbox,
        &[
            "sync",
            "status",
            "--project",
            project_arg(&fixture),
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["count"], 1);
    let row = &parsed["data"]["items"][0];
    assert_eq!(row["slug"], "alpha");
    assert_eq!(row["label"], "project_newer");
    assert_eq!(row["action"], "push");
    assert_eq!(row["evidence"]["project_hash"], fixture.project_hash);
    assert_eq!(row["evidence"]["library_hash"], fixture.library_hash);
    assert_eq!(row["evidence"]["base_hash"], fixture.project_hash);
    assert_eq!(
        stores(&fixture),
        before,
        "sync status must not write to either store"
    );
}

#[test]
fn sync_push_without_yes_exits_nine_and_writes_nothing() {
    let fixture = fixture(Baseline::Project);
    let before = stores(&fixture);
    let output = agentuna(
        &fixture.sandbox,
        &[
            "sync",
            "push",
            "alpha",
            "--project",
            project_arg(&fixture),
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 9);
    assert_eq!(parsed["error"]["code"], "confirmation_required");
    assert_eq!(parsed["error"]["details"]["requires"], "yes");
    assert_eq!(
        stores(&fixture),
        before,
        "an unconfirmed push wrote nothing"
    );
}

#[test]
fn sync_push_dry_run_previews_without_writing() {
    let fixture = fixture(Baseline::Project);
    let before = stores(&fixture);
    let baseline_before = baseline_hash(&fixture.sandbox, fixture.item_id);
    let output = agentuna(
        &fixture.sandbox,
        &[
            "sync",
            "push",
            "alpha",
            "--project",
            project_arg(&fixture),
            "--dry-run",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["dry_run"], true);
    assert_eq!(parsed["data"]["changed"], false);
    assert_eq!(parsed["data"]["files_changed"], 0);
    assert!(
        parsed["data"]["files"]
            .as_array()
            .is_some_and(|files| files.len() == 1),
        "the preview lists the one file: {parsed}"
    );
    assert_eq!(stores(&fixture), before, "a dry run wrote nothing");
    assert_eq!(
        baseline_hash(&fixture.sandbox, fixture.item_id),
        baseline_before,
        "a dry run did not move the baseline"
    );
}

#[test]
fn sync_push_with_yes_copies_the_project_copy() {
    let fixture = fixture(Baseline::Project);
    let output = agentuna(
        &fixture.sandbox,
        &[
            "sync",
            "push",
            "alpha",
            "--project",
            project_arg(&fixture),
            "--yes",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["changed"], true);
    assert_eq!(parsed["data"]["files_changed"], 1);
    assert!(
        parsed["data"]["operation_id"].is_string(),
        "the apply is journaled: {parsed}"
    );
    let library_doc = fs::read(
        fixture
            .user_store
            .skill_dir(&Slug::parse("alpha").expect("slug"))
            .join("SKILL.md"),
    )
    .expect("library doc");
    assert_eq!(
        library_doc, PROJECT_DOC,
        "the project copy replaced the library copy"
    );
    assert_eq!(
        baseline_hash(&fixture.sandbox, fixture.item_id).as_deref(),
        Some(fixture.project_hash.as_str()),
        "the shared baseline moved to the copied content"
    );
}

#[test]
fn sync_pull_with_yes_copies_the_library_copy() {
    let fixture = fixture(Baseline::Library);
    let output = agentuna(
        &fixture.sandbox,
        &[
            "sync",
            "pull",
            "alpha",
            "--project",
            project_arg(&fixture),
            "--yes",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["changed"], true);
    let project_doc = fs::read(
        fixture
            .project_store
            .skill_dir(&Slug::parse("alpha").expect("slug"))
            .join("SKILL.md"),
    )
    .expect("project doc");
    assert_eq!(
        project_doc, LIBRARY_DOC,
        "the library copy replaced the project copy"
    );
    assert_eq!(
        baseline_hash(&fixture.sandbox, fixture.item_id).as_deref(),
        Some(fixture.library_hash.as_str())
    );
}

#[test]
fn remote_status_without_refresh_never_contacts_the_network() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let before = stores(&fixture);
    let output = agentuna(&fixture.sandbox, &["remote", "status", "--json"]);
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["configured"], true);
    assert_eq!(parsed["data"]["network_contacted"], false);
    assert_eq!(parsed["data"]["remote_commit"], Value::Null);
    assert_eq!(parsed["data"]["token_saved"], false);
    assert!(
        !repo.exists(),
        "status without --refresh never creates or touches the remote"
    );
    assert_eq!(stores(&fixture), before, "read-only status wrote nothing");
}

#[test]
fn remote_status_refresh_without_a_token_fails_auth_without_a_token_echo() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let output = agentuna(
        &fixture.sandbox,
        &["remote", "status", "--refresh", "--json"],
    );
    let parsed = assert_exit(&output, 10);
    assert_eq!(parsed["error"]["code"], "auth_failed");
    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        !combined.contains("ghp_") && !combined.contains("sentinel"),
        "no token text may be echoed: {combined}"
    );
    assert!(
        !repo.exists(),
        "a failed refresh never creates the remote repository"
    );
}

#[test]
fn remote_backup_without_yes_exits_nine_and_changes_nothing() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let output = agentuna(&fixture.sandbox, &["remote", "backup", "--json"]);
    let parsed = assert_exit(&output, 9);
    assert_eq!(parsed["error"]["code"], "confirmation_required");
    assert_eq!(parsed["error"]["details"]["requires"], "yes");
    assert!(last_commit(&fixture.sandbox).is_none());
    assert!(!repo.exists(), "an unconfirmed backup creates nothing");
}

#[test]
fn remote_backup_without_a_token_fails_auth_and_creates_no_repository() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let output = agentuna(&fixture.sandbox, &["remote", "backup", "--yes", "--json"]);
    let parsed = assert_exit(&output, 10);
    assert_eq!(parsed["error"]["code"], "auth_failed");
    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        !combined.contains("ghp_") && !combined.contains("sentinel"),
        "no token text may be echoed: {combined}"
    );
    assert!(last_commit(&fixture.sandbox).is_none());
    assert!(!repo.exists(), "a failed backup creates no repository");
}

#[test]
fn remote_backup_dry_run_reports_the_store_without_writing() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let before = stores(&fixture);
    let output = agentuna(
        &fixture.sandbox,
        &["remote", "backup", "--dry-run", "--json"],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["dry_run"], true);
    assert_eq!(parsed["data"]["changed"], false);
    assert_eq!(parsed["data"]["items"], 1);
    assert_eq!(stores(&fixture), before, "a dry run wrote nothing");
    assert!(last_commit(&fixture.sandbox).is_none());
    assert!(!repo.exists());
}

#[test]
fn remote_restore_validates_the_commit_and_needs_confirmation() {
    let fixture = fixture(Baseline::Project);
    let repo = fixture.sandbox.path("remote-repo");
    configure_remote(&fixture.sandbox, &repo);
    let short = agentuna(&fixture.sandbox, &["remote", "restore", "abcd", "--json"]);
    let parsed = assert_exit(&short, 3);
    assert_eq!(parsed["error"]["code"], "validation");
    let commit = "a".repeat(40);
    let unconfirmed = agentuna(&fixture.sandbox, &["remote", "restore", &commit, "--json"]);
    let parsed = assert_exit(&unconfirmed, 9);
    assert_eq!(parsed["error"]["code"], "confirmation_required");
    let before = stores(&fixture);
    let dry_run = agentuna(
        &fixture.sandbox,
        &["remote", "restore", &commit, "--dry-run", "--json"],
    );
    let parsed = assert_exit(&dry_run, 0);
    assert_eq!(parsed["data"]["dry_run"], true);
    assert_eq!(parsed["data"]["commit"], commit);
    assert_eq!(parsed["data"]["target_empty"], false);
    assert_eq!(
        stores(&fixture),
        before,
        "an unconfirmed restore wrote nothing"
    );
    assert!(!repo.exists());
}

#[test]
fn read_only_commands_never_create_the_index() {
    let sandbox = Sandbox::new();
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project dir");
    let output = agentuna(&sandbox, &["remote", "status", "--json"]);
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["configured"], false);
    assert_eq!(parsed["data"]["network_contacted"], false);
    assert!(
        !sandbox.paths().db_path().exists(),
        "remote status must not create the index"
    );
    let sync = agentuna(
        &sandbox,
        &[
            "sync",
            "status",
            "--project",
            project.to_str().expect("utf8"),
            "--json",
        ],
    );
    let parsed = assert_exit(&sync, 4);
    assert_eq!(parsed["error"]["code"], "not_found");
    assert!(
        !sandbox.paths().db_path().exists(),
        "sync status must not create the index"
    );
}
