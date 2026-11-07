//! TA-040/TA-070: the shared agent-registry refresh detects, persists and stays idempotent.
//!
//! Every root is redirected into a temporary directory; the test never reads the developer's
//! home, stores or agent folders.

use std::fs;
use std::path::Path;

use at_db::Database;
use at_db::agents::{
    AgentPresence, is_agent_managed, list_agent_paths, list_agents_on_mac, set_agent_managed,
};
use at_engine::refresh::refresh_agents;
use at_model::AgentId;
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};

/// Resolves every AgenTuna root inside `root`.
fn paths(root: &Path) -> AppPaths {
    AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(root)
            .with_overrides(PathOverrides {
                home: Some(root.join("support")),
                user_store: Some(root.join("user-store")),
                global_store: Some(root.join("global-store")),
                project_store: None,
            }),
    )
    .expect("sandbox paths resolve")
}

/// Creates the native folders three built-in adapters recognise.
fn seed_native_home(root: &Path) {
    for directory in [
        ".claude/skills/review",
        ".codex/skills/review",
        ".omp/agent",
    ] {
        fs::create_dir_all(root.join(directory)).expect("create native directory");
    }
    fs::write(root.join(".claude/skills/review/SKILL.md"), b"fixture\n").expect("skill file");
    fs::write(root.join(".claude.json"), b"{}\n").expect("claude config");
}

#[test]
fn refresh_records_detected_agents_paths_and_preserves_managed_state() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().canonicalize().expect("canonical root");
    seed_native_home(&root);
    let paths = paths(&root);
    let database = Database::open_in_memory().expect("in-memory index");
    // A probed pass (the desktop app) recorded Cursor before its executable disappeared; the
    // filesystem-only CLI pass must keep that evidence instead of downgrading it.
    at_db::agents::upsert_agent(
        database.connection(),
        &at_model::Agent {
            id: AgentId::Cursor,
            display_name: String::from("cursor"),
            detected: true,
            version: Some(String::from("9.9.9")),
        },
    )
    .expect("probed cursor row");

    let written = refresh_agents(database.connection(), &paths).expect("first detection pass");
    assert_eq!(written, 5, "every registered adapter is recorded once");

    let agents = list_agents_on_mac(database.connection()).expect("agent rows");
    let agent = |id: &str| {
        agents
            .iter()
            .find(|agent| agent.id.to_string() == id)
            .unwrap_or_else(|| panic!("agent `{id}` is recorded"))
    };
    for id in ["claude_code", "codex", "omp"] {
        assert_eq!(
            agent(id).presence,
            AgentPresence::Installed,
            "{id} is installed in the injected home"
        );
    }
    assert_eq!(
        agent("cursor").presence,
        AgentPresence::Installed,
        "a filesystem-only pass keeps the probed installation"
    );
    assert_eq!(
        agent("cursor").version.as_deref(),
        Some("9.9.9"),
        "a filesystem-only pass keeps the probed version"
    );
    let claude_paths =
        list_agent_paths(database.connection(), &AgentId::ClaudeCode).expect("claude paths");
    let skills_root = root.join(".claude/skills");
    assert!(
        claude_paths.iter().any(|path| {
            path.path.as_deref() == Some(skills_root.as_path())
                && path.exists == Some(true)
                && path.verified
                && path.is_primary == Some(true)
        }),
        "the existing skills root is recorded as a verified primary path: {claude_paths:?}"
    );
    let path_count = claude_paths.len();

    set_agent_managed(database.connection(), &AgentId::ClaudeCode, false).expect("manage off");
    let written = refresh_agents(database.connection(), &paths).expect("second detection pass");
    assert_eq!(
        written, 5,
        "a repeated pass updates rows instead of duplicating them"
    );
    assert!(
        !is_agent_managed(database.connection(), &AgentId::ClaudeCode).expect("managed flag"),
        "detection never overrides the user's manage opt-out"
    );
    assert_eq!(
        list_agents_on_mac(database.connection())
            .expect("agent rows")
            .len(),
        5,
        "five adapters are registered and each has exactly one row"
    );
    assert_eq!(
        list_agent_paths(database.connection(), &AgentId::ClaudeCode)
            .expect("claude paths")
            .len(),
        path_count,
        "re-probing the same candidates does not duplicate path rows"
    );
}
