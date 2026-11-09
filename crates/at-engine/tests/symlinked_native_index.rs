//! A machine whose skills are manager-installed links reaches the derived index as findings.
//!
//! Unix-only harness: the layout is POSIX symlinks.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;

use at_db::Database;
use at_db::native::{NativeItemFilter, NativeStatus, list_native_items};
use at_db::skills_index::SkillFilter;
use at_engine::native_index::{refresh_native, refresh_native_with_environment};
use at_engine::refresh::refresh_agents;
use at_migrate::testkit::{CC_SWITCH_SKILLS, DANGLING_LINKS, DANGLING_PREFIX, SymlinkedSkillsHome};
use at_model::{AgentId, Scope};
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};

/// A sandboxed home holding the manager layout, with its own support and store roots.
struct Sandbox {
    _temp: tempfile::TempDir,
    fixture: SymlinkedSkillsHome,
    paths: AppPaths,
    database: Database,
}

impl Sandbox {
    fn new() -> Result<Self, Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let home = root.join("home");
        let support = root.join("support");
        let project = root.join("project");
        for directory in [&home, &support, &project] {
            fs::create_dir_all(directory)?;
        }
        let fixture = SymlinkedSkillsHome::install(&home, &root, &project)?;
        let paths = AppPaths::resolve(
            &PathEnvironment::default()
                .with_home(&home)
                .with_project_root(&project)
                .with_overrides(PathOverrides {
                    home: Some(support),
                    user_store: Some(home.join(".agentuna")),
                    project_store: None,
                    global_store: Some(root.join("global-store")),
                }),
        )?;
        let database = Database::open_file(paths.db_path())?;
        Ok(Self {
            _temp: temp,
            fixture,
            paths,
            database,
        })
    }
}

#[test]
fn manager_installed_skills_are_indexed_as_new_findings() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new()?;
    let fixture = &sandbox.fixture;
    refresh_agents(sandbox.database.connection(), &sandbox.paths)?;
    let report = refresh_native(sandbox.database.connection(), &sandbox.paths)?;

    let rows = list_native_items(
        sandbox.database.connection(),
        &NativeItemFilter {
            scope: Some(Scope::User),
            agent_id: None,
            status: None,
            include_missing: true,
        },
    )?;
    let skills = rows
        .iter()
        .filter(|row| row.kind == at_model::ItemKind::Skill)
        .collect::<Vec<_>>();
    assert_eq!(skills.len(), 96, "one row per entry each agent reads");
    assert!(
        skills.iter().all(|row| row.status == NativeStatus::New),
        "no manager-installed skill is stored as missing"
    );
    assert_eq!(report.marked_missing, 0);
    assert!(report.upserted >= 96);
    let claude = skills
        .iter()
        .filter(|row| row.agent_id == AgentId::ClaudeCode)
        .count();
    let codex = skills
        .iter()
        .filter(|row| row.agent_id == AgentId::Codex)
        .count();
    let omp = skills
        .iter()
        .filter(|row| row.agent_id == AgentId::Omp)
        .count();
    assert_eq!((claude, codex, omp), (27, 48, 21));
    assert!(skills.iter().all(|row| row.discovery_state.is_some()));

    // A link row stores the folder it resolved to and the folder's content hash.
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, CC_SWITCH_SKILLS[0].to_owned()))
        .ok_or("the fixture linked the skill")?;
    let row = skills
        .iter()
        .find(|row| row.path == link.to_string_lossy())
        .ok_or("the claude link has a row")?;
    let target = fs::canonicalize(
        fixture
            .library_path(CC_SWITCH_SKILLS[0])
            .ok_or("the fixture wrote the library")?,
    )?;
    assert_eq!(
        row.link_target.as_deref(),
        Some(target.to_string_lossy().as_ref())
    );
    assert_eq!(row.discovery_state.as_deref(), Some("valid"));
    assert!(row.content_hash.is_some());

    // Broken and refused entries keep their own row and reason.
    let dangling = fixture
        .dangling_links()
        .first()
        .ok_or("dangling fixture link")?;
    let broken = skills
        .iter()
        .find(|row| row.path == dangling.to_string_lossy())
        .ok_or("a dangling link is listed")?;
    assert_eq!(broken.discovery_state.as_deref(), Some("broken_link"));
    assert_eq!(
        broken.discovery_note.as_deref(),
        Some("dangling symbolic link")
    );
    assert!(broken.link_target.is_some());
    assert!(broken.content_hash.is_none());

    let escaped = skills
        .iter()
        .find(|row| row.path == fixture.escape_link().to_string_lossy())
        .ok_or("the refused link is listed")?;
    assert_eq!(escaped.discovery_state.as_deref(), Some("invalid"));
    assert_eq!(
        escaped.discovery_note.as_deref(),
        Some("symbolic link target is outside injected roots")
    );
    Ok(())
}

#[test]
fn a_rescan_repairs_rows_the_old_scan_marked_missing() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new()?;
    let connection = sandbox.database.connection();
    refresh_agents(connection, &sandbox.paths)?;

    // The state the released app left behind: a row per linked skill, all marked missing.
    let claude_links = sandbox
        .fixture
        .links()
        .iter()
        .filter(|((agent, _), _)| agent == &AgentId::ClaudeCode)
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    assert_eq!(claude_links.len(), 22);
    for (index, link) in claude_links.iter().enumerate() {
        connection.execute(
            "INSERT INTO native_items (id, agent_id, scope, kind, name, path, content_hash, status) \
             VALUES (?1, 'claude_code', 'user', 'skill', ?2, ?3, NULL, 'missing')",
            rusqlite::params![
                format!("nat_missing_{index}"),
                format!("stale-{index}"),
                link.to_string_lossy(),
            ],
        )?;
    }

    let before = list_native_items(
        connection,
        &NativeItemFilter {
            scope: Some(Scope::User),
            agent_id: Some(AgentId::ClaudeCode),
            status: Some(NativeStatus::Missing),
            include_missing: true,
        },
    )?;
    assert_eq!(before.len(), 22, "the stale rows exist before the rescan");

    let report = refresh_native(connection, &sandbox.paths)?;
    assert_eq!(report.resurrected, 22, "every stale row is seen again");
    assert_eq!(report.marked_missing, 0);

    let after = list_native_items(
        connection,
        &NativeItemFilter {
            scope: Some(Scope::User),
            agent_id: Some(AgentId::ClaudeCode),
            status: None,
            include_missing: true,
        },
    )?;
    assert_eq!(
        after.len(),
        27,
        "the 22 repaired rows plus the five new entries"
    );
    assert!(
        after.iter().all(|row| row.status == NativeStatus::New),
        "every claude row is new again"
    );
    let repaired = after
        .iter()
        .find(|row| row.path == claude_links[0].to_string_lossy())
        .ok_or("the repaired row exists")?;
    assert_eq!(repaired.discovery_state.as_deref(), Some("valid"));
    assert!(repaired.content_hash.is_some());
    assert_eq!(repaired.name, CC_SWITCH_SKILLS[0]);
    Ok(())
}

#[test]
fn the_skills_screen_lists_one_row_per_physical_skill_with_every_reader()
-> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new()?;
    let connection = sandbox.database.connection();
    refresh_agents(connection, &sandbox.paths)?;
    refresh_native(connection, &sandbox.paths)?;

    let rows = at_db::skills_index::list_skills(
        connection,
        &SkillFilter {
            include_native: true,
            ..SkillFilter::default()
        },
    )?;
    let cc_first = rows
        .iter()
        .find(|row| row.name == CC_SWITCH_SKILLS[0])
        .ok_or("the linked skill is listed")?;
    assert!(
        cc_first.item_id.is_none(),
        "a manager-installed skill is a native row"
    );
    assert_eq!(
        cc_first.deployed.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([AgentId::ClaudeCode, AgentId::Codex, AgentId::Omp]),
        "one row, every agent that links to the library folder"
    );
    assert_eq!(cc_first.native_state.as_deref(), Some("valid"));
    let target = fs::canonicalize(
        sandbox
            .fixture
            .library_path(CC_SWITCH_SKILLS[0])
            .ok_or("library folder")?,
    )?;
    assert_eq!(
        cc_first.native_link_target.as_deref(),
        Some(target.to_string_lossy().as_ref()),
        "the row names the library folder it links to"
    );
    assert!(
        cc_first.native_problem.is_none(),
        "a valid link carries no problem"
    );

    // The broken links are their own rows with their reason, never hidden or merged.
    let broken = rows
        .iter()
        .filter(|row| row.native_state.as_deref() == Some("broken_link"))
        .count();
    assert_eq!(
        broken,
        DANGLING_LINKS + 2,
        "13 dangling plus the two-link loop"
    );
    let dangling_row = rows
        .iter()
        .find(|row| row.name == format!("{DANGLING_PREFIX}-01"))
        .ok_or("a dangling link is listed")?;
    assert_eq!(
        dangling_row.native_problem.as_deref(),
        Some("dangling symbolic link")
    );

    let invalid = rows
        .iter()
        .find(|row| row.name == "cc-inner-link")
        .ok_or("the skill with a link inside its tree is listed")?;
    assert_eq!(invalid.native_state.as_deref(), Some("invalid"));
    assert_eq!(
        invalid.native_problem.as_deref(),
        Some("symlink inside skill folder: references/alias.txt")
    );
    // 22 unique valid physical skills; the two refused cc-switch skills, the 15 broken links and
    // the refused escaping link are listed next to them rather than dropped.
    let valid = rows
        .iter()
        .filter(|row| row.native_state.as_deref() == Some("valid"))
        .count();
    assert_eq!(valid, 22);
    Ok(())
}

#[test]
fn the_agents_card_reads_the_omp_folder_that_was_scanned() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new()?;
    let connection = sandbox.database.connection();
    refresh_agents(connection, &sandbox.paths)?;

    let paths = at_db::agents::list_agent_paths(connection, &AgentId::Omp)?;
    let primary = paths
        .iter()
        .find(|path| path.is_primary == Some(true))
        .ok_or("omp records a primary skills path")?;
    let primary_path = primary.path.clone().ok_or("primary path value")?;
    assert!(
        primary_path.ends_with(".omp/agent/managed-skills"),
        "the scanned folder is primary: {}",
        primary_path.display()
    );
    assert_eq!(primary.exists, Some(true));
    assert!(primary.verified);
    let legacy = paths
        .iter()
        .find(|path| {
            path.path
                .as_deref()
                .is_some_and(|path| path.ends_with(".omp/agent/skills"))
        })
        .ok_or("the secondary folder is still recorded")?;
    assert_eq!(legacy.is_primary, Some(false));
    assert_eq!(legacy.exists, Some(false));
    Ok(())
}

#[test]
fn a_project_scoped_scan_leaves_user_rows_alone() -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new()?;
    let connection = sandbox.database.connection();
    refresh_agents(connection, &sandbox.paths)?;
    let environment = at_engine::refresh::agent_environment(&sandbox.paths);
    refresh_native_with_environment(connection, &sandbox.paths, &environment)?;
    let user_rows = list_native_items(
        connection,
        &NativeItemFilter {
            scope: Some(Scope::User),
            agent_id: None,
            status: None,
            include_missing: true,
        },
    )?;
    assert!(user_rows.iter().all(|row| row.scope == Scope::User));
    Ok(())
}
