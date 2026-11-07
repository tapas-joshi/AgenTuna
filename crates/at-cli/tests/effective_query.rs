//! TA-006: `agentuna effective --json` is the shared `EffectiveQueryView`, serialized once.
//!
//! The test seeds a real Global store through the canonical manifest APIs, indexes it, locks
//! the item with the CLI's own lock command and then reads the effective payload back. Every
//! root lives in a temporary sandbox.

mod support;

use std::error::Error;
use std::fs;
use std::path::Path;

use at_db::Database;
use at_db::rebuild::{RebuildPaths, rebuild};
use at_engine::effective::EffectiveQueryView;
use at_model::Scope;
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::ManifestItem;
use serde_json::Value;

use support::{Sandbox, agentuna, exit_code as process_exit, stderr, stdout};

const REASON: &str = "Managed by organization policy";
const SKILL_DOC: &str =
    "---\nname: governance\ndescription: Org-wide governance skill\n---\n\n# Governance\n";

/// Writes a file, creating parents.
fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
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
        .item_by_slug(at_model::ItemKind::Skill, &slug)
        .unwrap_or_else(|| panic!("{name} is in the manifest"))
        .clone()
}

/// Rebuilds the sandbox index from the filesystem so the CLI sees the seeded item.
fn index(sandbox: &Sandbox) {
    fs::create_dir_all(sandbox.path("home")).expect("home");
    let mut database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let paths = RebuildPaths::new(sandbox.path("home"), sandbox.user_store())
        .with_global_store(sandbox.global_store());
    rebuild(database.connection_mut(), &paths, None).expect("index rebuild");
}

#[test]
fn effective_json_is_the_shared_view_and_carries_the_global_lock() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    fs::create_dir_all(sandbox.global_store())?;
    let store = StoreLayout::new(Scope::Global, sandbox.global_store());
    init_store(Scope::Global, store.root())?;
    let item = seed_skill(&store, "governance", "governance", SKILL_DOC);
    index(&sandbox);

    let item_id = item.id.to_string();
    let locked = agentuna(
        &sandbox,
        &[
            "--scope",
            "global",
            "effective",
            "lock",
            &item_id,
            "--reason",
            REASON,
            "--json",
        ],
    );
    assert_eq!(
        process_exit(&locked),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&locked)
    );

    let output = agentuna(&sandbox, &["--scope", "global", "effective", "--json"]);
    assert_eq!(
        process_exit(&output),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output))?;
    let data = envelope["data"].clone();

    // One struct, one serializer: the payload decodes into the type the Tauri command returns
    // and re-encodes to exactly the same value.
    let view: EffectiveQueryView = serde_json::from_value(data.clone())?;
    assert_eq!(
        serde_json::to_value(&view)?,
        data,
        "the CLI payload is the shared EffectiveQueryView, serialized once"
    );

    let row = view
        .items
        .iter()
        .find(|row| row.name == "governance")
        .expect("the seeded skill is effective at Global scope");
    assert_eq!(row.winner_scope, Scope::Global);
    assert_eq!(
        row.loads_in_agents, 0,
        "no agent is installed in the sandbox"
    );
    let lock = row.lock.as_ref().expect("locked row carries its lock");
    assert_eq!(lock.reason, REASON);
    assert!(
        lock.id.starts_with("lck_"),
        "lock identity is `{}`",
        lock.id
    );
    Ok(())
}

#[test]
fn effective_without_scope_defaults_to_user_and_reports_the_choice() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let output = agentuna(&sandbox, &["effective", "--json"]);
    assert_eq!(
        process_exit(&output),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output))?;
    assert_eq!(
        envelope["data"]["scope"], "user",
        "no project resolves here, so User answers: {envelope}"
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings
                .iter()
                .any(|warning| warning["code"] == "scope_defaulted")),
        "the defaulted scope is reported: {envelope}"
    );
    let view: EffectiveQueryView = serde_json::from_value(envelope["data"].clone())?;
    assert_eq!(view.scope, Some(Scope::User));

    let human = agentuna(&sandbox, &["effective"]);
    assert_eq!(
        process_exit(&human),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&human)
    );
    assert!(
        stdout(&human).starts_with("ITEM  KIND  SCOPE  NAME"),
        "{}",
        stdout(&human)
    );
    assert!(
        stderr(&human).contains("warning: no --scope given"),
        "human mode prints the chosen scope as a warning: {}",
        stderr(&human)
    );
    Ok(())
}
