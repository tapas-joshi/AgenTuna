//! TS-041 end-to-end tests: the real `agentuna skills` binary against a sandboxed home.
//!
//! Every root is redirected into a temporary directory. Stores are seeded through the
//! same canonical manifest and index APIs the product uses, never by writing index rows
//! directly, so the commands are exercised against realistic state.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use at_db::Database;
use at_db::rebuild::{RebuildPaths, rebuild};
use at_model::{Agent, AgentId, ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::{ManifestItem, StoreManifest};
use serde_json::Value;

use support::{Sandbox, agentuna, exit_code, stderr, stdout};

const SKILL_DOC: &str = "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha\n";

/// Writes a file, creating parents.
fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// Recursive content snapshot used to prove a refused command wrote nothing.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(current: &Path, root: &Path, into: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            // SQLite creates these transient sidecars for a read-only WAL connection.
            if path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.ends_with(".db-wal") || name.ends_with(".db-shm")
            }) {
                continue;
            }
            if path.is_dir() {
                into.push((relative, Vec::new()));
                walk(&path, root, into);
            } else if let Ok(bytes) = fs::read(&path) {
                into.push((relative, bytes));
            }
        }
    }
    let mut collected = Vec::new();
    walk(root, root, &mut collected);
    collected.sort();
    collected
}

/// Creates a canonical store and indexes a skill package into it.
fn seed_skill(store: &StoreLayout, slug_text: &str, name: &str, body: &str) -> ManifestItem {
    let slug = Slug::parse(slug_text).expect("slug");
    let dir = store.skill_dir(&slug);
    write(&dir.join("SKILL.md"), body.as_bytes());
    let rebuilt =
        at_store::manifest_rebuild::plan_manifest_rebuild(store).expect("plan manifest rebuild");
    let token = rebuilt.token().clone();
    at_store::manifest_rebuild::apply_manifest_rebuild(store, &rebuilt, &token)
        .expect("write manifest");
    at_store::manifest::StoreManifest::load(store)
        .expect("reload manifest")
        .manifest
        .item_by_slug(ItemKind::Skill, &slug)
        .unwrap_or_else(|| panic!("{name} is in the manifest"))
        .clone()
}

/// Opens the sandbox database and rebuilds its derived rows from the filesystem.
fn index(sandbox: &Sandbox) -> Database {
    fs::create_dir_all(sandbox.path("home")).expect("home");
    let mut database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let paths = RebuildPaths::new(sandbox.path("home"), sandbox.user_store())
        .with_global_store(sandbox.global_store());
    rebuild(database.connection_mut(), &paths, None).expect("index rebuild");
    agent_ready(&database);
    database
}

/// Marks Claude Code installed and managed so a deployment can target it.
fn agent_ready(database: &Database) {
    at_db::agents::upsert_agent(
        database.connection(),
        &Agent {
            id: AgentId::ClaudeCode,
            display_name: String::from("Claude Code"),
            detected: true,
            version: Some(String::from("1.0")),
        },
    )
    .expect("agent row");
    at_db::agents::set_agent_managed(database.connection(), &AgentId::ClaudeCode, true)
        .expect("managed agent");
}

/// A sandbox with one indexed `alpha` skill in the User store and Claude ready.
fn seeded() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    init_store(Scope::User, store.root()).expect("init store");
    seed_skill(&store, "alpha", "alpha", SKILL_DOC);
    let _database = index(&sandbox);
    sandbox
}

#[test]
fn enable_dry_run_returns_the_engine_changeset_without_writing() {
    let sandbox = seeded();
    let before = snapshot(&sandbox.path(""));
    let output = agentuna(
        &sandbox,
        &[
            "--read-only",
            "skills",
            "enable",
            "alpha",
            "--agent",
            "claude_code",
            "--dry-run",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["dry_run"], true);
    let change_set: at_model::ChangeSet =
        serde_json::from_value(parsed["data"]["change_set"].clone()).expect("planned ChangeSet");
    assert_eq!(change_set.summary, "Set skill alpha for claude_code");
    assert!(!change_set.ops.is_empty());
    assert_eq!(
        serde_json::to_value(&change_set).expect("serialize ChangeSet"),
        parsed["data"]["change_set"]
    );
    assert_eq!(
        snapshot(&sandbox.path("")),
        before,
        "a planner-only skill command leaves the sandbox unchanged"
    );
}

fn envelope(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).unwrap_or_else(|error| {
        panic!(
            "one JSON envelope expected ({error}); stderr: {}\nstdout: {}",
            stderr(output),
            stdout(output)
        )
    })
}

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

fn agent_skills_root(sandbox: &Sandbox) -> PathBuf {
    sandbox.path("home/.claude/skills")
}

#[test]
fn list_json_schema_is_stable() {
    let sandbox = seeded();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    seed_skill(
        &store,
        "beta",
        "beta",
        "---\nname: beta\ndescription: Beta\n---\n",
    );
    let _ = index(&sandbox);
    let output = agentuna(&sandbox, &["skills", "list", "--scope", "user", "--json"]);
    let parsed = assert_exit(&output, 0);
    let data = &parsed["data"];
    assert_eq!(
        data.as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["count", "indexed", "scope", "skills"],
        "the list payload schema is frozen"
    );
    assert_eq!(data["scope"], "user");
    assert_eq!(data["count"], 2);
    assert_eq!(data["indexed"], true);
    let rows = data["skills"].as_array().expect("skills array");
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row.as_object()
                .expect("row")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "agents",
                "description",
                "hash",
                "indexed",
                "item_id",
                "name",
                "path",
                "scope",
                "slug",
                "state",
            ],
            "one row schema for every skill"
        );
        assert_eq!(row["scope"], "user");
        assert_eq!(row["state"], "present");
        assert_eq!(row["indexed"], true);
    }
}

#[test]
fn list_project_scope_reads_the_selected_project_store() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project dir");
    {
        let database = Database::open_file(sandbox.paths().db_path()).expect("database");
        at_db::projects::add_project(database.connection(), &project, Some("demo"), None)
            .expect("register project");
    }
    let store_root = project.join(".agentuna");
    init_store(Scope::Project, &store_root).expect("project store");
    let store = StoreLayout::new(Scope::Project, store_root);
    seed_skill(&store, "alpha", "alpha", SKILL_DOC);
    let output = agentuna(
        &sandbox,
        &[
            "skills",
            "list",
            "--scope",
            "project",
            "--project",
            project.to_str().expect("utf8"),
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["scope"], "project");
    assert_eq!(parsed["data"]["count"], 1);
    assert_eq!(parsed["data"]["skills"][0]["slug"], "alpha");
}

#[test]
fn install_folder_writes_the_package_manifest_and_index_row() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let source = sandbox.path("source/alpha");
    write(&source.join("SKILL.md"), SKILL_DOC.as_bytes());
    let output = agentuna(
        &sandbox,
        &[
            "skills",
            "install",
            source.to_str().expect("utf8"),
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["source"], "folder");
    assert_eq!(parsed["data"]["slug"], "alpha");
    assert_eq!(parsed["data"]["scope"], "user");
    let package = sandbox.user_store().join("skills/alpha/SKILL.md");
    assert!(package.exists(), "the canonical package exists");
    assert_eq!(fs::read(&package).expect("package"), SKILL_DOC.as_bytes());
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    let manifest = StoreManifest::load(&store).expect("manifest").manifest;
    let item = manifest
        .item_by_slug(ItemKind::Skill, &Slug::parse("alpha").expect("slug"))
        .expect("manifest item");
    let database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let item_id = database
        .connection()
        .query_row(
            "SELECT id FROM items WHERE kind = 'skill' AND scope = 'user' AND slug = 'alpha'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("indexed item row");
    assert_eq!(item_id, item.id.to_string());
    let list = assert_exit(
        &agentuna(&sandbox, &["skills", "list", "--scope", "user", "--json"]),
        0,
    );
    assert_eq!(list["data"]["skills"][0]["indexed"], true);
    assert_eq!(list["data"]["skills"][0]["item_id"], item.id.to_string());
}

#[test]
fn install_git_url_imports_a_local_repository() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let repo = sandbox.path("repo");
    write(&repo.join("SKILL.md"), SKILL_DOC.as_bytes());
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init"]);
    git(&["add", "SKILL.md"]);
    git(&["commit", "-m", "fixture"]);
    let url = format!("file://{}", repo.display());
    let output = agentuna(&sandbox, &["skills", "install", &url, "--json"]);
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["source"], "git");
    assert!(
        sandbox.user_store().join("skills/alpha/SKILL.md").exists(),
        "the cloned package is installed"
    );
}

#[test]
fn install_refuses_git_urls_with_embedded_credentials() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let before = snapshot(&sandbox.user_store());
    for url in [
        "https://user:secret@example.invalid/repo.git",
        "https://token@example.invalid/repo.git",
    ] {
        let output = agentuna(&sandbox, &["skills", "install", url, "--json"]);
        assert_ne!(
            exit_code(&output),
            0,
            "{url} must be refused: {}",
            stderr(&output)
        );
        let parsed = envelope(&output);
        assert_eq!(parsed["error"]["code"], "network_required", "{url}");
    }
    assert_eq!(
        snapshot(&sandbox.user_store()),
        before,
        "no package is installed"
    );
}

#[test]
fn install_marketplace_id_dispatches_without_a_network_request_in_a_dry_run() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let before = snapshot(&sandbox.support());
    let output = agentuna(
        &sandbox,
        &[
            "skills",
            "install",
            "acme/tools/alpha",
            "--dry-run",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["source"], "marketplace");
    assert_eq!(
        parsed["data"]["repository"],
        "https://github.com/acme/tools"
    );
    assert_eq!(parsed["data"]["skill"], "alpha");
    assert_eq!(
        snapshot(&sandbox.support()),
        before,
        "a preview writes nothing"
    );
    assert!(
        !sandbox.user_store().join("skills/alpha").exists(),
        "no package is installed by the preview"
    );
}

#[test]
fn deploy_returns_the_same_payload_as_enable() {
    let sandbox = seeded();
    let enable = assert_exit(
        &agentuna(
            &sandbox,
            &[
                "skills",
                "enable",
                "alpha",
                "--agent",
                "claude_code",
                "--json",
            ],
        ),
        0,
    );
    let disable = assert_exit(
        &agentuna(
            &sandbox,
            &[
                "skills",
                "disable",
                "alpha",
                "--agent",
                "claude_code",
                "--json",
            ],
        ),
        0,
    );
    assert_eq!(disable["data"]["changed"], true, "disable removes the link");
    let deploy = assert_exit(
        &agentuna(
            &sandbox,
            &[
                "skills",
                "deploy",
                "alpha",
                "--agent",
                "claude_code",
                "--json",
            ],
        ),
        0,
    );
    let mut enable_data = enable["data"].as_object().expect("object").clone();
    let mut deploy_data = deploy["data"].as_object().expect("object").clone();
    let enable_operation = enable_data.remove("operation_id");
    let deploy_operation = deploy_data.remove("operation_id");
    assert!(enable_operation.is_some_and(|value| value.is_string()));
    assert!(deploy_operation.is_some_and(|value| value.is_string()));
    assert_eq!(
        enable_data, deploy_data,
        "deploy is a strict alias of enable apart from the operation id"
    );
    let link = agent_skills_root(&sandbox).join("alpha");
    assert!(
        fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()),
        "deploy created the native link"
    );
}

#[test]
fn remove_without_yes_exits_nine_and_writes_nothing() {
    let sandbox = seeded();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    let before = snapshot(&sandbox.user_store());
    let output = agentuna(&sandbox, &["skills", "remove", "alpha", "--json"]);
    let parsed = assert_exit(&output, 9);
    assert_eq!(parsed["error"]["code"], "confirmation_required");
    assert_eq!(
        snapshot(&sandbox.user_store()),
        before,
        "an unconfirmed remove writes nothing"
    );
    assert!(
        store
            .skill_dir(&Slug::parse("alpha").expect("slug"))
            .exists()
    );
}

#[test]
fn remove_with_yes_reports_a_restorable_backup() {
    let sandbox = seeded();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    let output = agentuna(&sandbox, &["skills", "remove", "alpha", "--yes", "--json"]);
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["removed"], true);
    let backup = PathBuf::from(parsed["data"]["backup_path"].as_str().expect("backup path"));
    assert!(backup.exists(), "the reported backup path exists");
    assert!(
        backup
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(
                parsed["data"]["operation_id"]
                    .as_str()
                    .expect("operation id")
            )),
        "the backup folder is the operation folder"
    );
    let restored = snapshot(&backup)
        .into_iter()
        .find(|(path, _)| path.ends_with("SKILL.md"));
    assert_eq!(
        restored
            .expect("the deleted package is in the operation backup")
            .1,
        SKILL_DOC.as_bytes(),
        "the backup restores the package byte-identically"
    );
    let operation_id = parsed["data"]["operation_id"]
        .as_str()
        .expect("operation id");
    assert!(
        !store
            .skill_dir(&Slug::parse("alpha").expect("slug"))
            .exists(),
        "the canonical package is gone"
    );
    let manifest = StoreManifest::load(&store).expect("manifest").manifest;
    assert!(
        manifest
            .item_by_slug(ItemKind::Skill, &Slug::parse("alpha").expect("slug"))
            .is_none(),
        "the manifest no longer lists the skill"
    );
    let database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let operation = at_db::ops::operation_detail(database.connection(), operation_id)
        .expect("operation detail")
        .expect("recorded operation");
    assert_eq!(
        operation.operation.actor,
        Some(at_db::ops::OperationActor::Cli)
    );
    assert_eq!(
        operation.operation.kind,
        Some(at_db::ops::OperationKind::Delete)
    );
    assert_eq!(operation.operation.scope, Some(Scope::User));
    assert_eq!(
        operation.operation.summary.as_deref(),
        Some("delete skill alpha from user")
    );
    assert_eq!(
        operation.operation.status,
        Some(at_db::ops::OperationStatus::Succeeded)
    );
    assert!(!operation.operation.started_at.is_empty());
    assert!(operation.files.iter().any(|file| file.backup_id.is_some()));
    let rows: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM items WHERE kind = 'skill' AND scope = 'user' AND slug = 'alpha'",
            [],
            |row| row.get(0),
        )
        .expect("row count");
    assert_eq!(rows, 0, "the index row is removed");
}

#[test]
fn json_output_never_contains_ansi_escapes() {
    let sandbox = seeded();
    let output = Command::new(env!("CARGO_BIN_EXE_agentuna"))
        .args(["skills", "list", "--scope", "user", "--json"])
        .env("TERM", "xterm-256color")
        .env("CLICOLOR_FORCE", "1")
        .envs(sandbox.vars())
        .current_dir(sandbox.cwd())
        .output()
        .expect("run");
    assert_eq!(exit_code(&output), 0);
    assert!(
        !stdout(&output).contains('\u{1b}'),
        "--json output carries no ANSI escapes"
    );
}

#[test]
fn preset_apply_reports_the_before_and_after_ratio() {
    let sandbox = seeded();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    let beta = seed_skill(
        &store,
        "beta",
        "beta",
        "---\nname: beta\ndescription: Beta\n---\n",
    );
    let _ = index(&sandbox);
    let mut database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let alpha = StoreManifest::load(&store)
        .expect("manifest")
        .manifest
        .item_by_slug(ItemKind::Skill, &Slug::parse("alpha").expect("slug"))
        .expect("alpha")
        .id;
    let preset_id = {
        let mut service = at_engine::presets::PresetService::new(&mut database);
        service
            .create(
                "daily",
                Some("Daily skills"),
                Scope::User,
                &[(alpha, ItemKind::Skill), (beta.id, ItemKind::Skill)],
            )
            .expect("create preset")
    };
    let list = assert_exit(
        &agentuna(&sandbox, &["skills", "preset", "list", "--json"]),
        0,
    );
    assert_eq!(list["data"]["count"], 1);
    assert_eq!(list["data"]["presets"][0]["id"], preset_id);
    let output = agentuna(
        &sandbox,
        &[
            "skills",
            "preset",
            "apply",
            &preset_id,
            "--agent",
            "claude_code",
            "--json",
        ],
    );
    let parsed = assert_exit(&output, 0);
    assert_eq!(parsed["data"]["ratio_before"]["enabled"], 0);
    assert_eq!(parsed["data"]["ratio_before"]["total"], 2);
    assert_eq!(parsed["data"]["ratio_after"]["enabled"], 2);
    assert_eq!(parsed["data"]["ratio_after"]["total"], 2);
    assert_eq!(parsed["data"]["ratio_after"]["complete"], true);
    assert_eq!(parsed["data"]["changed"], true);
    let skills_root = agent_skills_root(&sandbox);
    for slug in ["alpha", "beta"] {
        let link = skills_root.join(slug);
        assert!(
            fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()),
            "preset apply links {slug}"
        );
    }
}
