//! TU-026: the project above the working directory is the default `--project`.
//!
//! The flag's help promises that it replaces "the nearest Git or registered project above
//! the working directory". These tests run the real binary from directories inside a
//! sandbox and prove the walk, the explicit override and that discovery never writes.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use at_model::{ItemKind, Scope};
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::manifest::StoreManifest;
use serde_json::Value;

use support::{Sandbox, agentuna, agentuna_in, exit_code, stderr, stdout};

/// The physical form of a path, so `/var` and `/private/var` compare equal.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A skill document the store loader accepts.
fn skill_doc(name: &str) -> String {
    format!("---\nname: {name}\ndescription: discovery fixture\n---\n\n# {name}\n")
}

/// Initializes `project/.agentuna` with one skill and returns its item id.
fn seed_project_skill(project: &Path, slug_text: &str, name: &str) -> String {
    let store = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, store.root()).expect("initialize the project store");
    let slug = Slug::parse(slug_text).expect("slug");
    let directory = store.skill_dir(&slug);
    fs::create_dir_all(&directory).expect("skill directory");
    fs::write(directory.join("SKILL.md"), skill_doc(name)).expect("SKILL.md");
    let rebuilt =
        at_store::manifest_rebuild::plan_manifest_rebuild(&store).expect("plan manifest rebuild");
    let token = rebuilt.token().clone();
    at_store::manifest_rebuild::apply_manifest_rebuild(&store, &rebuilt, &token)
        .expect("write manifest");
    StoreManifest::load(&store)
        .expect("reload manifest")
        .manifest
        .item_by_slug(ItemKind::Skill, &slug)
        .expect("the seeded skill is in the manifest")
        .id
        .to_string()
}

/// A repository root with a nested working directory below it.
fn repo_with_nested(repo: &Path) -> PathBuf {
    fs::create_dir_all(repo.join(".git")).expect("git marker");
    let nested = repo.join("src/deep");
    fs::create_dir_all(&nested).expect("nested directory");
    nested
}

/// Parses the one envelope the command wrote.
fn envelope(output: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(output)).expect("one JSON envelope on stdout")
}

#[test]
fn discovery_stops_at_the_nearest_git_root_or_registered_project() {
    let sandbox = Sandbox::new();
    let outer = sandbox.path("outer");
    fs::create_dir_all(outer.join(".git")).expect("git marker");
    let inner = outer.join("nested/deep");
    fs::create_dir_all(&inner).expect("nested directory");

    let found = at_cli::discovery::discover(&inner, &[]).expect("a Git root above the start");
    assert_eq!(
        canonical(&found),
        canonical(&outer),
        "the nearest ancestor with a `.git` entry wins"
    );

    // A registered root matches even without a `.git` entry.
    let registered = sandbox.path("registered");
    let start = registered.join("a/b");
    fs::create_dir_all(&start).expect("registered tree");
    let found = at_cli::discovery::discover(&start, &[canonical(&registered)])
        .expect("a registered root above the start");
    assert_eq!(canonical(&found), canonical(&registered));
}

#[test]
fn a_git_root_above_the_working_directory_becomes_the_project() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let repo = sandbox.path("home/work/repo");
    let nested = repo_with_nested(&repo);

    let output = agentuna_in(&sandbox, &nested, &["--json", "effective"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(
        envelope["data"]["scope"], "project",
        "a Git root above the working directory answers the project scope: {envelope}"
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|warning| warning["code"] == "scope_defaulted"),
        "the defaulted scope is reported: {envelope}"
    );
}

#[test]
fn a_registered_project_above_the_working_directory_becomes_the_project() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let project = sandbox.path("home/work/lib-tools");
    fs::create_dir_all(&project).expect("project root");
    let item_id = seed_project_skill(&project, "lib-skill", "lib-skill");
    let added = agentuna(
        &sandbox,
        &["--json", "projects", "add", &project.display().to_string()],
    );
    assert_eq!(exit_code(&added), 0, "{}", stderr(&added));
    let nested = project.join("src/deep");
    fs::create_dir_all(&nested).expect("nested directory");

    let output = agentuna_in(&sandbox, &nested, &["--json", "effective"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["data"]["scope"], "project");
    assert!(
        envelope["data"]["items"]
            .as_array()
            .expect("items array")
            .iter()
            .any(|item| item["item_id"] == item_id.as_str()),
        "the registered project's store answered: {envelope}"
    );
}

#[test]
fn an_explicit_project_flag_overrides_discovery() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let repo = sandbox.path("home/work/repo");
    let nested = repo_with_nested(&repo);
    let other = sandbox.path("home/work/other-project");
    fs::create_dir_all(&other).expect("other project");
    let other_item = seed_project_skill(&other, "other-skill", "other-skill");

    let output = agentuna_in(
        &sandbox,
        &nested,
        &[
            "--json",
            "--project",
            &other.display().to_string(),
            "effective",
        ],
    );
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    let envelope = envelope(&output);
    assert_eq!(envelope["data"]["scope"], "project");
    assert!(
        envelope["data"]["items"]
            .as_array()
            .expect("items array")
            .iter()
            .any(|item| item["item_id"] == other_item.as_str()),
        "--project, not the discovered Git root, answered: {envelope}"
    );
}

#[test]
fn discovery_reads_without_creating_a_store_or_index() {
    let sandbox = Sandbox::new();
    let repo = sandbox.path("home/work/repo");
    let nested = repo_with_nested(&repo);

    let output = agentuna_in(&sandbox, &nested, &["--json", "--read-only", "effective"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr(&output));
    assert!(
        !sandbox.paths().db_path().exists(),
        "discovery never creates the index"
    );
    assert!(
        !repo.join(".agentuna").exists(),
        "discovery never creates a project store"
    );
}
