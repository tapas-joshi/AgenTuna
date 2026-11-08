//! Behaviour of the shared test fixtures (`at_fs::testkit`).

// Unix-only harness: POSIX symlinks, permission bits and raw errno constants. The Windows
// twins arrive with the slice that enables the Windows test suite.
#![cfg(unix)]
#![cfg(feature = "testkit")]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use at_fs::testkit::{SAMPLE_SERVER, SAMPLE_SKILL, TestHome, snapshot};
use at_store::AppPaths;
use at_store::PathEnvironment;

const ENV_VARS: [&str; 5] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "APPDATA",
    "LOCALAPPDATA",
];

fn paths_for(home: &TestHome) -> AppPaths {
    home.paths_with(|home| AppPaths::resolve(&PathEnvironment::default().with_home(home)))
        .expect("app paths")
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn homes_are_unique_and_isolated_under_parallel_tests() {
    let handles: Vec<_> = (0..8)
        .map(|index| {
            std::thread::spawn(move || {
                let home = TestHome::new().expect("home");
                home.write(format!("own-{index}.txt"), b"marker")
                    .expect("write");
                let root = home.root().to_path_buf();
                let home_dir = home.home().to_path_buf();
                let files = names(&home_dir);
                (root, home_dir, files)
            })
        })
        .collect();

    let mut roots = BTreeSet::new();
    for (index, handle) in handles.into_iter().enumerate() {
        let (root, home_dir, files) = handle.join().expect("thread");
        assert!(roots.insert(root), "each TestHome owns a unique temp dir");
        assert_eq!(
            files,
            vec![format!("own-{index}.txt")],
            "{} must only contain its own file",
            home_dir.display()
        );
    }
    assert_eq!(roots.len(), 8);
}

#[test]
fn install_claude_code_writes_skills_mcp_and_rules() {
    let home = TestHome::empty_home().expect("home");
    let fixture = home.install_claude_code().expect("claude code");

    let skill = home
        .home()
        .join(".claude/skills")
        .join(SAMPLE_SKILL)
        .join("SKILL.md");
    assert_eq!(fixture.skill_path, skill);
    let skill_body = fs::read_to_string(&skill).expect("skill");
    assert!(skill_body.starts_with("---\nname: "), "{skill_body}");
    assert!(skill_body.contains(SAMPLE_SKILL));

    let mcp = home.home().join(".claude.json");
    assert_eq!(fixture.mcp_path, mcp);
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&mcp).expect("mcp")).expect("valid json");
    let servers = parsed
        .get("mcpServers")
        .and_then(|servers| servers.as_object())
        .expect("mcpServers object");
    assert_eq!(servers.len(), 1);
    assert!(servers.contains_key(SAMPLE_SERVER));

    let rules = home.home().join(".claude/CLAUDE.md");
    assert_eq!(fixture.rules_path.as_deref(), Some(rules.as_path()));
    assert!(rules.exists());
    assert_eq!(home.installed_agents().len(), 1);
}

#[test]
fn install_codex_writes_config_shared_skills_and_rules() {
    let home = TestHome::empty_home().expect("home");
    let fixture = home.install_codex().expect("codex");

    let config = home.home().join(".codex/config.toml");
    assert_eq!(fixture.mcp_path, config);
    let toml = fs::read_to_string(&config).expect("config");
    assert!(
        toml.contains(&format!("[mcp_servers.{SAMPLE_SERVER}]")),
        "{toml}"
    );
    assert!(toml.contains("command = \"npx\""), "{toml}");

    let skill = home
        .home()
        .join(".agents/skills")
        .join(SAMPLE_SKILL)
        .join("SKILL.md");
    assert_eq!(fixture.skill_path, skill);
    assert!(skill.exists(), "{}", skill.display());

    let rules = home.home().join(".codex/AGENTS.md");
    assert_eq!(fixture.rules_path.as_deref(), Some(rules.as_path()));
    assert!(rules.exists());
}

#[test]
fn omp_cursor_and_custom_installers_write_their_layouts() {
    let home = TestHome::empty_home().expect("home");

    let omp = home.install_omp().expect("omp");
    assert!(omp.skill_path.exists());
    assert!(omp.mcp_path.ends_with(".omp/agent/mcp.json"));
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&omp.mcp_path).expect("mcp")).expect("json");
    assert!(parsed.get("$schema").is_some(), "omp writes a $schema");
    assert!(parsed["mcpServers"].get(SAMPLE_SERVER).is_some());

    let cursor = home.install_cursor().expect("cursor");
    assert!(cursor.mcp_path.ends_with(".cursor/mcp.json"));
    let rule = cursor.rules_path.expect("cursor rules");
    assert!(rule.extension().is_some_and(|extension| extension == "mdc"));

    let custom = home
        .install_custom_agent("acme-agent", "custom-root")
        .expect("custom");
    assert_eq!(custom.agent.to_string(), "custom:acme-agent");
    assert_eq!(custom.root, home.home().join("custom-root"));
    assert!(custom.mcp_path.ends_with("custom-root/mcp.json"));

    assert_eq!(home.installed_agents().len(), 3);
    assert!(home.install_custom_agent("Not Valid", "x").is_err());
}

#[test]
fn snapshot_detects_a_single_byte_change_and_never_follows_symlinks() {
    let home = TestHome::new().expect("home");
    let tree = home.mkdir("tree").expect("tree");
    let outside = home.mkdir("outside").expect("outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, b"original").expect("outside file");
    symlink(&outside_file, tree.join("link.txt")).expect("symlink");

    let file = tree.join("file.txt");
    fs::write(&file, b"aaaa").expect("file");
    let before = snapshot(&tree).expect("snapshot");
    let keys: Vec<&String> = before.keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "keys are sorted");
    assert!(before.contains_key("file.txt"));
    assert!(before.contains_key("link.txt"));

    // Changing the symlink target's content is invisible to the snapshot.
    fs::write(&outside_file, b"changed").expect("outside write");
    let after_outside = snapshot(&tree).expect("snapshot");
    assert_eq!(before, after_outside, "symlinks are not followed");

    // One byte inside the tree changes exactly one hash.
    fs::write(&file, b"aaab").expect("file");
    let after = snapshot(&tree).expect("snapshot");
    assert_ne!(before, after);
    assert_ne!(before["file.txt"], after["file.txt"]);
    assert_eq!(before["link.txt"], after["link.txt"]);
}

#[test]
fn empty_home_has_no_agent_files_and_no_store_items() {
    let home = TestHome::empty_home().expect("home");
    assert!(home.installed_agents().is_empty());
    for root in home.agent_roots() {
        assert!(!root.exists(), "{} must not exist", root.display());
    }
    assert!(
        snapshot(home.home()).expect("snapshot").is_empty(),
        "an empty home holds nothing"
    );

    let paths = paths_for(&home);
    assert_eq!(paths.user_store(), home.home().join(".agentuna"));
    assert!(!paths.user_store().exists());
    assert!(paths.user_store().starts_with(home.root()));
    assert!(paths.support_dir().starts_with(home.home()));
}

#[test]
fn project_fixture_registers_its_path_and_cleans_up() {
    let home = TestHome::new().expect("home");
    let root_of_home = home.root().to_path_buf();
    let project = home.project("acme").expect("project");
    let root = project.root().to_path_buf();

    assert_eq!(home.registered_projects(), vec![root.clone()]);
    assert!(root.starts_with(&root_of_home));
    assert_eq!(project.name(), "acme");
    assert!(root.join(".mcp.json").exists());
    assert!(root.join(".claude/settings.local.json").exists());
    assert!(root.join("CLAUDE.md").exists());
    assert!(root.join("AGENTS.md").exists());
    assert!(project.skill_path().exists());
    assert_eq!(project.store(), root.join(".agentuna"));

    let paths = home
        .paths_with(|home| {
            AppPaths::resolve(
                &PathEnvironment::default()
                    .with_home(home)
                    .with_project_root(&root),
            )
        })
        .expect("app paths");
    assert_eq!(
        paths.project_store(),
        Some(project.store().as_path()),
        "the fixture root is what the caller injects"
    );

    drop(project);
    assert!(!root.exists(), "dropping the fixture removes the tree");

    drop(home);
    assert!(!root_of_home.exists(), "dropping the home removes the tree");
}

static ENV_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn env_scope_restores_previous_values_after_a_panic() {
    let _lock = ENV_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let previous: Vec<(&str, Option<String>)> = ENV_VARS
        .iter()
        .map(|name| (*name, std::env::var(name).ok()))
        .collect();

    let outcome = std::panic::catch_unwind(|| {
        let home = TestHome::with_env().expect("home with env");
        let scoped = home.home().to_path_buf();
        assert_eq!(
            std::env::var("HOME").expect("HOME is set"),
            scoped.to_string_lossy()
        );
        assert_eq!(
            std::env::var("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME is set"),
            scoped.join(".config").to_string_lossy()
        );
        assert!(home.env_scope().is_some());
        panic!("unwinding with an env scope alive");
    });
    assert!(outcome.is_err(), "the fixture test panicked as intended");

    for (name, value) in previous {
        assert_eq!(
            std::env::var(name).ok(),
            value,
            "{name} was restored after the panic"
        );
    }
}

#[test]
fn env_scope_release_restores_values_without_waiting_for_drop() {
    let _lock = ENV_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let before = std::env::var("HOME").ok();
    let home = TestHome::new().expect("home");
    let scope = at_fs::testkit::EnvScope::new(home.home()).expect("scope");
    assert!(scope.is_active());
    assert_ne!(std::env::var("HOME").ok(), before);
    scope.release();
    assert_eq!(std::env::var("HOME").ok(), before);
}

#[test]
fn paths_with_roots_app_paths_at_the_fixture() {
    let home = TestHome::new().expect("home");
    let paths = paths_for(&home);
    assert_eq!(paths.home_dir(), Some(home.home()));
    assert_eq!(paths.user_store(), home.home().join(".agentuna"));
    assert!(paths.project_store().is_none());
    assert!(
        home.paths_with(|root| root.to_path_buf()) == home.home(),
        "paths_with hands the home to the resolver"
    );
    let _unused: PathBuf = home.path("relative/file");
}
