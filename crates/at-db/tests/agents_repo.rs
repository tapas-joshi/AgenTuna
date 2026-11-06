//! TA-040: agents and agent_paths repositories over the TF-011 tables.

use std::path::Path;

use at_db::Database;
use at_db::DbError;
use at_db::agents::{
    AgentPath, AgentPathFormat, AgentPathKind, AgentPathSpec, AgentPresence, agent_settings_json,
    is_agent_managed, list_agent_paths, list_agents_on_mac, managed_agent_ids, mark_path_probed,
    set_agent_managed, set_agent_settings_json, set_path_override, upsert_agent, upsert_agent_path,
};
use at_model::{Agent, AgentId, Scope};

fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens")
}

fn agent(id: AgentId, display_name: &str, detected: bool) -> Agent {
    Agent {
        id,
        display_name: display_name.to_owned(),
        detected,
        version: detected.then(|| "1.2.3".to_owned()),
    }
}

fn count(connection: &rusqlite::Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count runs")
}

#[test]
fn upsert_is_idempotent_for_the_four_builtin_agents() {
    let database = database();
    let connection = database.connection();
    let builtins = [
        agent(AgentId::ClaudeCode, "Claude Code", true),
        agent(AgentId::Codex, "Codex", false),
        agent(AgentId::Omp, "omp", true),
        agent(AgentId::Cursor, "Cursor", false),
    ];

    for _ in 0..3 {
        for builtin in &builtins {
            upsert_agent(connection, builtin).expect("upsert");
        }
    }

    assert_eq!(count(connection, "agents"), 4);
    let names: Vec<String> = builtins.iter().map(|agent| agent.id.to_string()).collect();
    for id in names {
        let stored: String = connection
            .query_row("SELECT id FROM agents WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .expect("row exists");
        assert_eq!(stored, id);
    }
    assert_eq!(count_installed(connection), 2);
}

fn count_installed(connection: &rusqlite::Connection) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM agents WHERE installed = 1",
            [],
            |row| row.get(0),
        )
        .expect("count runs")
}

#[test]
fn an_opt_out_survives_a_later_detection_pass() {
    let database = database();
    let connection = database.connection();
    let codex = agent(AgentId::Codex, "Codex", true);

    upsert_agent(connection, &codex).expect("first detection");
    assert!(is_agent_managed(connection, &AgentId::Codex).expect("managed"));
    assert!(set_agent_managed(connection, &AgentId::Codex, false).expect("update"));
    assert!(!is_agent_managed(connection, &AgentId::Codex).expect("unmanaged"));

    // Detection runs again: the opt-out must not be overwritten.
    upsert_agent(connection, &codex).expect("second detection");
    assert!(!is_agent_managed(connection, &AgentId::Codex).expect("still unmanaged"));
    assert!(
        !managed_agent_ids(connection)
            .expect("ids")
            .contains(&AgentId::Codex)
    );
    assert!(managed_agent_ids(connection).expect("ids").is_empty());

    set_agent_managed(connection, &AgentId::Codex, true).expect("opt in");
    assert_eq!(
        managed_agent_ids(connection).expect("ids"),
        vec![AgentId::Codex]
    );
    assert!(!set_agent_managed(connection, &AgentId::Cursor, false).expect("unknown row"));
    assert!(!is_agent_managed(connection, &AgentId::Cursor).expect("unknown reads false"));
}

#[test]
fn a_path_override_stays_unverified_until_the_probe_succeeds() {
    let mut database = database();
    {
        let connection = database.connection_mut();
        upsert_agent(connection, &agent(AgentId::Codex, "Codex", true)).expect("agent");

        let stored = set_path_override(
            connection,
            &AgentId::Codex,
            Scope::Project,
            AgentPathKind::McpFile,
            Path::new("/work/.codex/config.toml"),
        )
        .expect("override");
        assert!(stored.is_override);
        assert!(!stored.verified);
        assert_eq!(stored.is_primary, Some(true));
        assert_eq!(stored.exists, Some(false));

        let probed = mark_path_probed(connection, stored.id, true)
            .expect("probe")
            .expect("row exists");
        assert!(probed.verified);
        assert!(probed.is_override, "the probe keeps the override flag");
        assert_eq!(probed.exists, Some(true));

        // A second override becomes the only primary row for its scope and kind.
        let replaced = set_path_override(
            connection,
            &AgentId::Codex,
            Scope::Project,
            AgentPathKind::McpFile,
            Path::new("/work/vendor/codex.toml"),
        )
        .expect("second override");
        let primary_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM agent_paths \
                 WHERE agent_id = 'codex' AND scope = 'project' AND kind = 'mcp_file' \
                   AND is_primary = 1",
                [],
                |row| row.get(0),
            )
            .expect("count runs");
        assert_eq!(primary_rows, 1);
        assert_eq!(replaced.is_primary, Some(true));
        assert_eq!(
            mark_path_probed(connection, 99_999, true).expect("missing"),
            None
        );
    }
}

#[test]
fn upserting_a_detected_path_refreshes_one_row() {
    let database = database();
    let connection = database.connection();
    upsert_agent(connection, &agent(AgentId::Omp, "omp", true)).expect("agent");

    let spec = AgentPathSpec {
        agent: &AgentId::Omp,
        scope: Scope::User,
        kind: AgentPathKind::SkillsDir,
        path: Path::new("/Users/someone/.omp/skills"),
        format: Some(AgentPathFormat::Dir),
        is_primary: true,
        exists: true,
    };
    let first = upsert_agent_path(connection, &spec).expect("first detection");
    assert!(first.verified);
    assert_eq!(first.exists, Some(true));
    assert!(!first.is_override);

    let second = upsert_agent_path(
        connection,
        &AgentPathSpec {
            exists: false,
            is_primary: false,
            ..spec.clone()
        },
    )
    .expect("second detection");
    assert_eq!(count(connection, "agent_paths"), 1);
    assert_eq!(second.id, first.id);
    assert_eq!(second.exists, Some(false));
    assert_eq!(second.is_primary, Some(false));

    let paths = list_agent_paths(connection, &AgentId::Omp).expect("paths");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].id, first.id);
}

#[test]
fn the_agents_list_groups_by_presence_and_resolves_the_primary_path() {
    let database = database();
    let connection = database.connection();

    upsert_agent(connection, &agent(AgentId::ClaudeCode, "Claude Code", true)).expect("installed");
    upsert_agent(connection, &agent(AgentId::Codex, "Codex", false)).expect("configured only");
    upsert_agent(connection, &agent(AgentId::Omp, "omp", false)).expect("absent");

    upsert_agent_path(
        connection,
        &AgentPathSpec {
            agent: &AgentId::ClaudeCode,
            scope: Scope::User,
            kind: AgentPathKind::McpFile,
            path: Path::new("/Users/someone/.claude.json"),
            format: Some(AgentPathFormat::Json),
            is_primary: true,
            exists: true,
        },
    )
    .expect("user path");
    upsert_agent_path(
        connection,
        &AgentPathSpec {
            agent: &AgentId::ClaudeCode,
            scope: Scope::Project,
            kind: AgentPathKind::McpFile,
            path: Path::new("/work/.mcp.json"),
            format: Some(AgentPathFormat::Json),
            is_primary: true,
            exists: true,
        },
    )
    .expect("project path");
    upsert_agent_path(
        connection,
        &AgentPathSpec {
            agent: &AgentId::Codex,
            scope: Scope::User,
            kind: AgentPathKind::RulesFile,
            path: Path::new("/Users/someone/.codex/AGENTS.md"),
            format: Some(AgentPathFormat::Markdown),
            is_primary: true,
            exists: false,
        },
    )
    .expect("codex path");

    let agents = list_agents_on_mac(connection).expect("list");
    let order: Vec<(String, AgentPresence)> = agents
        .iter()
        .map(|agent| (agent.id.to_string(), agent.presence))
        .collect();
    assert_eq!(
        order,
        vec![
            ("claude_code".to_owned(), AgentPresence::Installed),
            ("codex".to_owned(), AgentPresence::ConfiguredOnly),
            ("omp".to_owned(), AgentPresence::NotInstalled),
        ]
    );
    assert_eq!(agents[0].version.as_deref(), Some("1.2.3"));
    assert!(agents[0].managed);
    assert_eq!(
        agents[0].primary_path.as_deref().and_then(Path::to_str),
        Some("/work/.mcp.json"),
        "the nearest scope wins"
    );
    assert_eq!(
        agents[1].primary_path.as_deref().and_then(Path::to_str),
        Some("/Users/someone/.codex/AGENTS.md")
    );
    assert_eq!(agents[2].primary_path, None);
}

#[test]
fn agent_settings_json_round_trips_and_rejects_invalid_documents() {
    let database = database();
    let connection = database.connection();
    upsert_agent(connection, &agent(AgentId::Cursor, "Cursor", true)).expect("agent");

    assert_eq!(
        agent_settings_json(connection, &AgentId::Cursor).expect("read"),
        None
    );
    assert!(
        set_agent_settings_json(connection, &AgentId::Cursor, Some(r#"{"theme":"dark"}"#))
            .expect("write")
    );
    assert_eq!(
        agent_settings_json(connection, &AgentId::Cursor).expect("read"),
        Some(r#"{"theme":"dark"}"#.to_owned())
    );

    assert!(matches!(
        set_agent_settings_json(connection, &AgentId::Cursor, Some("{oops")),
        Err(DbError::Json(_))
    ));
    assert_eq!(
        agent_settings_json(connection, &AgentId::Cursor).expect("read"),
        Some(r#"{"theme":"dark"}"#.to_owned()),
        "a rejected write leaves the stored blob alone"
    );

    assert!(set_agent_settings_json(connection, &AgentId::Cursor, None).expect("clear"));
    assert_eq!(
        agent_settings_json(connection, &AgentId::Cursor).expect("read"),
        None
    );
    assert!(!set_agent_settings_json(connection, &AgentId::Omp, Some("{}")).expect("unknown row"));
}

#[test]
fn the_repository_adds_no_migration() {
    let database = database();
    let before = database.schema_version().expect("version");
    let connection = database.connection();
    upsert_agent(connection, &agent(AgentId::Cursor, "Cursor", true)).expect("upsert");
    let _: Vec<AgentPath> = list_agent_paths(connection, &AgentId::Cursor).expect("paths");
    let _ = list_agents_on_mac(connection).expect("list");
    assert_eq!(database.schema_version().expect("version"), before);
    assert!(before >= 9);
}
