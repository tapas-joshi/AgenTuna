//! The Skills screen lists manager-installed link skills with their target and reason.
//!
//! Unix-only harness: the fixture layout is POSIX symlinks.

#![cfg(unix)]

mod support;

use std::fs;

use at_app::app::flow_harness;
use at_migrate::testkit::{DANGLING_PREFIX, SymlinkedSkillsHome};
use at_model::{AgentId, Scope};
use support::{Sandbox, TestResult};

const CC_FIRST: &str = "cc-01";

/// Installs the link layout in the sandbox home and refreshes the derived index.
fn prepared_sandbox() -> TestResult<(Sandbox, at_migrate::testkit::SymlinkedSkillsHome)> {
    let sandbox = Sandbox::new()?;
    let fixture = SymlinkedSkillsHome::install(&sandbox.home, sandbox.root(), &sandbox.project)?;
    let database = sandbox.database()?;
    let paths = sandbox.paths()?;
    at_engine::refresh::refresh_agents(database.connection(), &paths)?;
    at_engine::native_index::refresh_native(database.connection(), &paths)?;
    Ok((sandbox, fixture))
}

#[test]
fn linked_skills_show_their_path_and_the_folder_they_link_to() -> TestResult<()> {
    let (sandbox, fixture) = prepared_sandbox()?;
    let state = sandbox.state()?;
    let view = flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: None,
            project_id: None,
        },
    )?;

    let row = view
        .rows
        .iter()
        .find(|row| row.name == CC_FIRST)
        .ok_or("the manager-installed skill is not in the Skills screen")?;
    assert!(row.native, "it is a native finding, not a stored item");
    assert_eq!(row.native_state.as_deref(), Some("valid"));
    assert!(row.native_problem.is_none());
    let target = fs::canonicalize(
        fixture
            .library_path(CC_FIRST)
            .ok_or("the fixture wrote the library skill")?,
    )?;
    assert_eq!(
        row.native_link_target.as_deref(),
        Some(target.to_string_lossy().as_ref()),
        "the row names the library folder it links to"
    );
    assert!(
        row.native_path
            .as_deref()
            .is_some_and(|path| path.contains("/skills/cc-01")),
        "the row names one of the link paths: {:?}",
        row.native_path
    );
    for cell in &row.agents {
        assert!(
            cell.config_path
                .as_deref()
                .is_some_and(|path| path.ends_with("/cc-01")),
            "{} names its own link: {:?}",
            cell.agent,
            cell.config_path
        );
    }
    let agents = row
        .agents
        .iter()
        .map(|cell| (cell.agent.as_str(), cell.state.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        agents,
        vec![
            ("claude_code", "native"),
            ("codex", "native"),
            ("omp", "native")
        ],
        "every agent that links the library folder is a cell"
    );
    assert!(
        row.agents.iter().all(|cell| {
            cell.config_path.as_deref().is_some_and(|path| {
                let expected = match cell.agent.as_str() {
                    "claude_code" => ".claude/skills/cc-01",
                    "codex" => "skills/cc-01",
                    "omp" => ".omp/agent/managed-skills/cc-01",
                    _ => "skills/cc-01",
                };
                path.ends_with(expected)
            })
        }),
        "each cell names the folder that agent reads: {:?}",
        row.agents
    );
    Ok(())
}

#[test]
fn refused_and_broken_rows_stay_visible_with_their_reason() -> TestResult<()> {
    let (sandbox, _fixture) = prepared_sandbox()?;
    let state = sandbox.state()?;
    let view = flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: None,
            project_id: None,
        },
    )?;

    let dangling = view
        .rows
        .iter()
        .find(|row| row.name == format!("{DANGLING_PREFIX}-01"))
        .ok_or("a dangling link is a row, not a silent omission")?;
    assert_eq!(dangling.native_state.as_deref(), Some("broken_link"));
    assert_eq!(
        dangling.native_problem.as_deref(),
        Some("dangling symbolic link")
    );

    let inner = view
        .rows
        .iter()
        .find(|row| row.name == "cc-inner-link")
        .ok_or("the skill with a link inside its tree is a row")?;
    assert_eq!(inner.native_state.as_deref(), Some("invalid"));
    assert_eq!(
        inner.native_problem.as_deref(),
        Some("symlink inside skill folder: references/alias.txt")
    );

    let escaped = view
        .rows
        .iter()
        .find(|row| {
            row.native_state.as_deref() == Some("invalid")
                && row.native_link_target.is_none()
                && row.native_problem.as_deref()
                    == Some("symbolic link target is outside injected roots")
        })
        .or_else(|| {
            view.rows.iter().find(|row| {
                row.native_problem.as_deref()
                    == Some("symbolic link target is outside injected roots")
            })
        })
        .ok_or("the refused escaping link is a row")?;
    assert!(escaped.native, "it is still a native finding");
    Ok(())
}

#[test]
fn the_agents_screen_shows_the_omp_folder_that_was_scanned() -> TestResult<()> {
    let (sandbox, _fixture) = prepared_sandbox()?;
    let state = sandbox.state()?;
    let cards = at_app::app::workflows::agents_list_inner(&state)?;
    let omp = cards
        .iter()
        .find(|card| card.id == AgentId::Omp.to_string())
        .ok_or("the omp card is listed")?;
    assert!(
        omp.primary_path
            .as_deref()
            .is_some_and(|path| path.ends_with(".omp/agent/managed-skills")),
        "the card names the folder the scan read: {:?}",
        omp.primary_path
    );
    assert!(
        omp.paths
            .iter()
            .any(|path| path.ends_with(".omp/agent/managed-skills")),
        "the managed-skills folder is a listed configuration path"
    );
    assert!(
        !omp.paths
            .iter()
            .any(|path| path.ends_with(".omp/agent/skills")),
        "the absent secondary folder is not presented as the agent's folder"
    );
    // Native findings are not deployments: they reach the Skills screen from `native_items`,
    // and only an adoption creates a `native` deployment row.
    assert_eq!(omp.managed_skills, 0);
    assert_eq!(omp.native_skills, 0);
    Ok(())
}

#[test]
fn adopting_a_linked_skill_keeps_the_library_untouched() -> TestResult<()> {
    let (sandbox, fixture) = prepared_sandbox()?;
    let state = sandbox.state()?;
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, CC_FIRST.to_owned()))
        .ok_or("the fixture linked the skill")?
        .clone();
    let library = fixture
        .library_path(CC_FIRST)
        .ok_or("the fixture wrote the library")?;
    let before = at_store::manifest::hash_skill_folder(&library)?.to_string();
    assert!(!before.is_empty(), "the library has content to protect");

    // The adopt path hashes the resolved folder rather than refusing the link.
    let mut database = sandbox.database()?;
    let paths = sandbox.paths()?;
    let scan = at_migrate::scan::scan(at_migrate::scan::ScanRequest {
        environment: &at_engine::refresh::agent_environment(&paths),
        adapters: &at_adapters::adapter::AgentAdapterRegistry::register(),
        store_roots: &[],
        store_items: &[],
    })?;
    let item = scan
        .items
        .iter()
        .find(|item| item.path == link)
        .ok_or("the link is in the scan")?;
    assert!(item.resolved.is_some());
    let layout = sandbox.layout(Scope::User)?;
    at_store::layout::init_store(Scope::User, layout.root())?;
    let backup_root = sandbox.root().join("backups");
    fs::create_dir_all(&backup_root)?;
    let roots = at_fs::ApplyRoots::new([sandbox.home.clone()])?;
    let adopted = at_migrate::adopt::adopt_item(at_migrate::adopt::AdoptRequest {
        scan: &scan,
        path: &link,
        name: None,
        store_root: layout.root(),
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        scope: Scope::User,
        handling: at_migrate::adopt::AdoptHandling::parse("watch")?,
        project_id: None,
    })?;
    assert!(!adopted.operation_id.is_empty());

    // The adopted item is a normal Skills-screen row now.
    let view = flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: None,
            project_id: None,
        },
    )?;
    // The native copy already holds the name, so the adopted item takes a suffixed slug.
    assert!(
        view.rows
            .iter()
            .any(|row| !row.native && row.name.starts_with(CC_FIRST)),
        "the adopted skill is listed as a stored item: {:?}",
        view.rows
            .iter()
            .map(|row| (row.name.clone(), row.native, row.state.clone()))
            .collect::<Vec<_>>()
    );
    Ok(())
}
