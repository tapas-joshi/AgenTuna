//! `agentuna` over a machine whose skills are manager-installed links.
//!
//! Everything runs the real binary with all roots redirected into a temporary directory.

mod support;

use std::fs;

use at_migrate::testkit::{CC_SWITCH_SKILLS, DANGLING_LINKS, DANGLING_PREFIX, SymlinkedSkillsHome};
use serde_json::Value;
use support::{Sandbox, agentuna, exit_code, stdout};

/// Installs the link layout in the sandbox home.
fn install_layout(sandbox: &Sandbox) -> at_migrate::testkit::SymlinkedSkillsHome {
    let home = sandbox.path("home");
    fs::create_dir_all(&home).expect("home");
    SymlinkedSkillsHome::install(
        &home,
        &sandbox.path("sandbox-root"),
        &sandbox.path("project"),
    )
    .expect("fixture layout")
}

/// Parses the one JSON envelope on stdout, failing on a non-zero exit.
fn scan_items(sandbox: &Sandbox) -> Vec<Value> {
    let output = agentuna(sandbox, &["migrate", "scan", "--json"]);
    assert_eq!(exit_code(&output), 0, "{}", stdout(&output));
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    envelope["data"]["items"]
        .as_array()
        .expect("items array")
        .clone()
}

#[test]
fn migrate_scan_lists_manager_installed_skills_as_findings() {
    let sandbox = Sandbox::new();
    let fixture = install_layout(&sandbox);
    let items = scan_items(&sandbox);
    assert_eq!(items.len(), 96, "one item per entry each agent reads");

    let parsed = items
        .iter()
        .filter(|item| item["kind"] == "skill" && item["parse_error"].is_null())
        .count();
    assert_eq!(parsed, 73, "every link with readable content is parsed");

    // A validated link names the folder it resolved to; the scan never says "unreadable".
    let claude_link = fixture
        .links()
        .get(&(
            at_model::AgentId::ClaudeCode,
            CC_SWITCH_SKILLS[0].to_owned(),
        ))
        .expect("the fixture linked the skill");
    let linked = items
        .iter()
        .find(|item| item["path"] == claude_link.to_string_lossy().as_ref())
        .expect("the claude link is in the scan");
    assert!(linked["parse_error"].is_null(), "{linked}");
    let target = fs::canonicalize(
        fixture
            .library_path(CC_SWITCH_SKILLS[0])
            .expect("library folder"),
    )
    .expect("canonical library");
    assert_eq!(linked["resolved"], target.to_string_lossy().as_ref());
    assert_eq!(linked["discovery"], "valid");
    assert!(linked["content_hash"].is_string());

    // Refused content, dangling links and escapes keep their own reason.
    let reasons = items
        .iter()
        .filter_map(|item| item["parse_error"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| **reason == "symlink inside skill folder: references/alias.txt")
            .count(),
        4,
        "one per root that links the skill: {reasons:?}"
    );
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| **reason == "dangling symbolic link")
            .count(),
        DANGLING_LINKS
    );
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| **reason == "symbolic link target is outside injected roots")
            .count(),
        1
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item["discovery"] == "broken_link")
            .count(),
        DANGLING_LINKS + 2,
        "13 dangling links plus the two-link loop"
    );
    assert!(
        items
            .iter()
            .any(|item| item["name"] == format!("{DANGLING_PREFIX}-01"))
    );
}

#[test]
fn adopting_a_linked_skill_makes_it_a_stored_skill() {
    let sandbox = Sandbox::new();
    let fixture = install_layout(&sandbox);
    let link = fixture
        .links()
        .get(&(
            at_model::AgentId::ClaudeCode,
            CC_SWITCH_SKILLS[0].to_owned(),
        ))
        .expect("the fixture linked the skill")
        .clone();
    let library = fixture
        .library_path(CC_SWITCH_SKILLS[0])
        .expect("library folder");
    let before = at_store::manifest::hash_skill_folder(&library)
        .expect("library hash")
        .to_string();

    let output = agentuna(
        &sandbox,
        &[
            "adopt",
            &link.to_string_lossy(),
            "--keep",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(exit_code(&output), 0, "{}", stdout(&output));

    let output = agentuna(&sandbox, &["skills", "list", "--json"]);
    assert_eq!(exit_code(&output), 0, "{}", stdout(&output));
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    assert_eq!(
        envelope["data"]["count"],
        1,
        "the adopted linked skill is a stored skill: {}",
        stdout(&output)
    );
    let skill = &envelope["data"]["skills"][0];
    // The native copy still holds the name, so the adopted store item takes a suffixed slug.
    assert!(
        skill["name"]
            .as_str()
            .is_some_and(|name| name.starts_with(CC_SWITCH_SKILLS[0])),
        "{skill}"
    );
    assert_eq!(skill["state"], "present");
    assert_eq!(
        at_store::manifest::hash_skill_folder(&library)
            .expect("library hash")
            .to_string(),
        before,
        "the library folder is untouched by adopting its link"
    );
    assert!(link.exists(), "the agent link is kept");
}
