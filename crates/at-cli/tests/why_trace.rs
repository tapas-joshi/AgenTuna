//! TA-070: `agentuna why` emits the four-scope trace inside the shared JSON envelope.
//!
//! The test seeds a real User store in a temporary sandbox, indexes it, then reads the trace
//! twice to prove the envelope is byte-stable. It also checks that the five command groups are
//! registered on the one CLI tree, so `--help` lists each noun exactly once.

mod support;

use std::error::Error;
use std::fs;
use std::path::Path;

use at_db::Database;
use at_db::rebuild::{RebuildPaths, rebuild};
use at_model::Scope;
use at_store::layout::{Slug, StoreLayout, init_store};
use serde_json::Value;

use support::{Sandbox, agentuna, exit_code, stderr, stdout};

const SKILL_DOC: &str = "---\nname: reviewer\ndescription: Reviews changes\n---\n\n# Reviewer\n";

/// Writes a file, creating parents.
fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

/// Creates the User store and one canonical skill package inside it.
fn seed_skill(sandbox: &Sandbox) -> String {
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    init_store(Scope::User, store.root()).expect("initialize the User store");
    let slug = Slug::parse("reviewer").expect("slug");
    write(
        &store.skill_dir(&slug).join("SKILL.md"),
        SKILL_DOC.as_bytes(),
    );
    let rebuilt =
        at_store::manifest_rebuild::plan_manifest_rebuild(&store).expect("plan manifest rebuild");
    let token = rebuilt.token().clone();
    at_store::manifest_rebuild::apply_manifest_rebuild(&store, &rebuilt, &token)
        .expect("write manifest");
    at_store::manifest::StoreManifest::load(&store)
        .expect("reload manifest")
        .manifest
        .item_by_slug(at_model::ItemKind::Skill, &slug)
        .expect("seeded skill is in the manifest")
        .id
        .to_string()
}

/// Rebuilds the derived index from the sandbox filesystem.
fn index(sandbox: &Sandbox) {
    fs::create_dir_all(sandbox.path("home")).expect("home");
    let mut database = Database::open_file(sandbox.paths().db_path()).expect("database");
    let paths = RebuildPaths::new(sandbox.path("home"), sandbox.user_store())
        .with_global_store(sandbox.global_store());
    rebuild(database.connection_mut(), &paths, None).expect("index rebuild");
}

#[test]
fn why_emits_the_four_step_trace_and_a_byte_stable_envelope() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let item_id = seed_skill(&sandbox);
    index(&sandbox);

    let args = ["--scope", "user", "why", item_id.as_str(), "--json"];
    let first = agentuna(&sandbox, &args);
    assert_eq!(
        exit_code(&first),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&first)
    );
    let envelope: Value = serde_json::from_str(&stdout(&first))?;
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "why");

    let data = &envelope["data"];
    assert_eq!(data["item_id"], item_id.as_str());
    assert_eq!(data["active_scope"], "user");
    let steps = data["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 4, "one step per scope");
    assert_eq!(
        steps
            .iter()
            .map(|step| step["scope"].as_str().expect("scope string"))
            .collect::<Vec<_>>(),
        vec!["global", "user", "project", "local"],
        "steps are ordered Global, User, Project, Local"
    );
    assert_eq!(steps[0]["status"], "absent");
    assert_eq!(steps[1]["status"], "winning");
    let location = &steps[1]["location"];
    assert_eq!(
        location["line"], 2,
        "the winning store skill reports its SKILL.md name line"
    );
    let span = location["byte_span"]
        .as_array()
        .expect("name byte span array");
    assert_eq!(span.len(), 2, "byte span has a start and an end");
    let document = fs::read_to_string(sandbox.user_store().join("skills/reviewer/SKILL.md"))
        .expect("read the seeded SKILL.md");
    let start = usize::try_from(span[0].as_u64().expect("span start")).expect("start fits");
    let end = usize::try_from(span[1].as_u64().expect("span end")).expect("end fits");
    assert_eq!(
        &document[start..end],
        "name: reviewer",
        "the span covers the frontmatter name entry"
    );
    for step in steps {
        let location = &step["location"];
        assert!(
            location["line"].is_null() || location["line"].is_u64(),
            "every location carries a line key: {step}"
        );
        assert!(step["reason"].is_string(), "every step explains itself");
    }
    assert_eq!(data["verdict"]["on"], true);
    assert_eq!(data["verdict"]["decisive_step"], 1);

    let second = agentuna(&sandbox, &args);
    assert_eq!(
        exit_code(&second),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&second)
    );
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "the JSON envelope is byte-stable across runs"
    );
    assert!(
        stderr(&second).is_empty(),
        "diagnostics do not reach stdout in JSON mode: {}",
        stderr(&second)
    );
    Ok(())
}

#[test]
fn the_five_groups_appear_once_and_effective_extends_without_a_second_registration() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let help = agentuna(&sandbox, &["--help"]);
    assert_eq!(exit_code(&help), at_cli::exit_code::SUCCESS);
    let help = stdout(&help);
    for noun in ["agents", "effective", "why", "overview", "rules"] {
        let occurrences = help
            .lines()
            .filter(|line| line.trim_start().starts_with(noun))
            .count();
        assert_eq!(
            occurrences, 1,
            "`{noun}` appears once in the help tree: {help}"
        );
    }

    // TA-097 adds `effective lock|unlock` to the existing group; the help text must not
    // grow a second `effective` noun to register them.
    let effective = agentuna(&sandbox, &["effective", "--help"]);
    assert_eq!(exit_code(&effective), at_cli::exit_code::SUCCESS);
    let effective = stdout(&effective);
    for verb in ["lock", "unlock"] {
        assert!(
            effective
                .lines()
                .any(|line| line.trim_start().starts_with(verb)),
            "`effective {verb}` is registered on the existing group: {effective}"
        );
    }
}

/// Creates one skill package inside `store` with an explicit slug and lowercase frontmatter
/// name, reindexes the manifest and returns its item id.
fn seed_named_skill(store: &StoreLayout, scope: Scope, slug_text: &str, name: &str) -> String {
    if !store.root().join("store.json").exists() {
        init_store(scope, store.root()).expect("initialize the store");
    }
    let slug = Slug::parse(slug_text).expect("slug");
    let doc = format!("---\nname: {name}\ndescription: Reviews changes\n---\n\n# {name}\n");
    write(&store.skill_dir(&slug).join("SKILL.md"), doc.as_bytes());
    let rebuilt =
        at_store::manifest_rebuild::plan_manifest_rebuild(store).expect("plan manifest rebuild");
    let token = rebuilt.token().clone();
    at_store::manifest_rebuild::apply_manifest_rebuild(store, &rebuilt, &token)
        .expect("write manifest");
    at_store::manifest::StoreManifest::load(store)
        .expect("reload manifest")
        .manifest
        .item_by_slug(at_model::ItemKind::Skill, &slug)
        .expect("seeded skill is in the manifest")
        .id
        .to_string()
}

#[test]
fn why_accepts_a_name_instead_of_an_item_id() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let item_id = seed_skill(&sandbox);
    index(&sandbox);

    let by_id = agentuna(
        &sandbox,
        &["--scope", "user", "why", item_id.as_str(), "--json"],
    );
    assert_eq!(
        exit_code(&by_id),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&by_id)
    );
    let by_name = agentuna(&sandbox, &["--scope", "user", "why", "reviewer", "--json"]);
    assert_eq!(
        exit_code(&by_name),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&by_name)
    );
    assert_eq!(
        stdout(&by_id),
        stdout(&by_name),
        "a name and its id explain the same item"
    );
    Ok(())
}

#[test]
fn why_accepts_a_folder_slug_when_it_differs_from_the_name() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let store = StoreLayout::new(Scope::User, sandbox.user_store());
    let item_id = seed_named_skill(&store, Scope::User, "review-tools", "review-toolkit");
    index(&sandbox);

    let output = agentuna(
        &sandbox,
        &["--scope", "user", "why", "review-tools", "--json"],
    );
    assert_eq!(
        exit_code(&output),
        at_cli::exit_code::SUCCESS,
        "{}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one envelope");
    assert_eq!(envelope["data"]["item_id"], item_id.as_str());
}

#[test]
fn why_lists_every_candidate_when_a_name_is_ambiguous() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let first = seed_skill(&sandbox);
    let project = sandbox.path("project");
    fs::create_dir_all(&project).expect("project root");
    let project_store = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    let second = seed_named_skill(&project_store, Scope::Project, "reviewer", "reviewer");
    index(&sandbox);

    let output = agentuna(
        &sandbox,
        &[
            "--scope",
            "project",
            "--project",
            &project.display().to_string(),
            "why",
            "reviewer",
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&output),
        at_cli::exit_code::VALIDATION,
        "{}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one envelope");
    assert_eq!(envelope["error"]["code"], "validation");
    let candidates = envelope["error"]["details"]["candidates"]
        .as_array()
        .expect("candidates array");
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    let ids = candidates
        .iter()
        .filter_map(|candidate| candidate["item_id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&first.as_str()) && ids.contains(&second.as_str()),
        "both ids are listed: {candidates:?}"
    );
    let scopes = candidates
        .iter()
        .filter_map(|candidate| candidate["scope"].as_str())
        .collect::<Vec<_>>();
    assert!(
        scopes.contains(&"user") && scopes.contains(&"project"),
        "every candidate carries its scope: {candidates:?}"
    );

    let missing = agentuna(
        &sandbox,
        &[
            "--scope",
            "project",
            "--project",
            &project.display().to_string(),
            "why",
            "not-a-skill",
            "--json",
        ],
    );
    assert_eq!(
        exit_code(&missing),
        at_cli::exit_code::NOT_FOUND,
        "an unknown name is not found, not a usage error: {}",
        stderr(&missing)
    );
}
