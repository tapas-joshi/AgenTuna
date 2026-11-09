//! A machine whose skills are installed by manager tools as links into central libraries.
//!
//! Unix-only harness: the layout is POSIX symlinks.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_fs::ApplyRoots;
use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::plan::{BuildPlanRequest, ConflictResolution, MigrationStrategy, build_plan};
use at_migrate::scan::{DiscoveryState, ParseStatus, ScanItem, ScanReport, ScanRequest, scan};
use at_migrate::testkit::{CC_SWITCH_SKILLS, SymlinkedSkillsExpectations, SymlinkedSkillsHome};
use at_model::{AgentId, Scope};

/// Scans the fixture exactly the way the app does, with no store inventory.
fn scan_fixture(fixture: &SymlinkedSkillsHome) -> Result<ScanReport, Box<dyn Error>> {
    let environment = AgentEnvironment {
        home: fixture.home().to_path_buf(),
        project: None,
        variables: BTreeMap::new(),
    };
    let adapters = AgentAdapterRegistry::register();
    let store_roots: Vec<std::path::PathBuf> = Vec::new();
    Ok(scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &store_roots,
        store_items: &[],
    })?)
}

fn find<'r>(report: &'r ScanReport, path: &std::path::Path) -> &'r ScanItem {
    report
        .items
        .iter()
        .find(|item| item.path == path)
        .unwrap_or_else(|| panic!("{} is not in the scan", path.display()))
}

#[test]
fn manager_installed_skills_are_parsed_through_their_resolved_folder() -> Result<(), Box<dyn Error>>
{
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;

    let links = fixture.links();
    // Every link an agent reads is a finding with the library content, never "unreadable".
    let mut parsed = 0usize;
    for ((agent, name), link) in links {
        // The two library skills with refused content are covered by the next test.
        if name == "cc-inner-link" || name == "cc-bad-frontmatter" {
            continue;
        }
        let item = find(&report, link);
        assert_eq!(item.name, *name, "{} is {}", link.display(), name);
        assert_eq!(item.agents, BTreeSet::from([agent.clone()]));
        assert_eq!(item.discovery, DiscoveryState::Valid, "{}", link.display());
        assert!(
            matches!(item.parse_status, ParseStatus::Parsed),
            "{}: {:?}",
            link.display(),
            item.parse_status
        );
        let target = fixture
            .library_path(name)
            .ok_or_else(|| format!("no library folder for {name}"))?;
        let resolved = std::fs::canonicalize(&target)?;
        assert_eq!(item.resolved.as_ref(), Some(&resolved));
        assert_eq!(
            item.link_target.as_deref(),
            Some(resolved.as_path()),
            "{} names the folder it links to",
            link.display()
        );
        assert_eq!(
            item.normalized_hash.as_deref(),
            Some(
                at_store::manifest::hash_skill_folder(&resolved)?
                    .to_string()
                    .as_str()
            ),
            "{} hashes the resolved folder",
            link.display()
        );
        parsed += 1;
    }
    let readable_links = links
        .keys()
        .filter(|(_, name)| name != "cc-inner-link" && name != "cc-bad-frontmatter")
        .count();
    assert_eq!(
        parsed, readable_links,
        "every created link with readable content is a parsed finding"
    );

    // A real folder inside an agent root is its own skill; the nested document is content.
    let superset = fixture.home().join(".claude/skills/superset");
    let item = find(&report, &superset);
    assert!(matches!(item.parse_status, ParseStatus::Parsed));
    assert!(item.resolved.is_none());
    assert!(
        !report
            .items
            .iter()
            .any(|candidate| candidate.path.ends_with("superset/nested-skill")),
        "a nested skill document is content, not a second skill"
    );
    assert!(
        !report
            .items
            .iter()
            .any(|candidate| candidate.path == fixture.system_dir()),
        "a folder without SKILL.md is not a skill"
    );
    Ok(())
}

#[test]
fn refused_entries_stay_listed_with_a_precise_reason() -> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;

    // A link inside a library tree is named by its relative entry.
    let inner = find(
        &report,
        &fixture.home().join(".claude/skills/cc-inner-link"),
    );
    assert_eq!(inner.discovery, DiscoveryState::Invalid);
    assert_eq!(
        inner.parse_status,
        ParseStatus::Error("symlink inside skill folder: references/alias.txt".to_owned())
    );

    // Invalid frontmatter keeps its own error and stays listed.
    let bad = find(
        &report,
        &fixture.home().join(".claude/skills/cc-bad-frontmatter"),
    );
    assert_eq!(bad.discovery, DiscoveryState::Invalid);
    assert!(
        matches!(&bad.parse_status, ParseStatus::Error(message)
            if message.contains("invalid YAML frontmatter")),
        "{:?}",
        bad.parse_status
    );

    // Dangling links are broken links with their target, not silent omissions.
    for dangling in fixture.dangling_links() {
        let item = find(&report, dangling);
        assert_eq!(
            item.discovery,
            DiscoveryState::BrokenLink,
            "{}",
            dangling.display()
        );
        assert!(
            item.link_target.is_some(),
            "{} carries its target",
            dangling.display()
        );
        assert!(
            item.resolved.is_none(),
            "a dangling target is never resolved"
        );
    }

    // A link loop is reported as broken, never followed.
    for link in fixture.loop_links() {
        let item = find(&report, link);
        assert_eq!(item.discovery, DiscoveryState::BrokenLink);
        assert!(
            matches!(&item.parse_status, ParseStatus::Error(message)
                if message == "symbolic link loop"),
            "{:?}",
            item.parse_status
        );
    }

    // A target outside the injected home is refused before anything is read from it.
    let escape = find(&report, fixture.escape_link());
    assert_eq!(escape.discovery, DiscoveryState::Invalid);
    assert_eq!(
        escape.parse_status,
        ParseStatus::Error("symbolic link target is outside injected roots".to_owned())
    );
    assert!(escape.normalized_hash.is_none());
    assert_eq!(escape.link_target.as_deref(), Some(fixture.outside_skill()));
    assert!(
        report
            .items
            .iter()
            .all(|item| item.path != fixture.outside_skill().join("SKILL.md")),
        "the refused target is not scanned as a skill of its own"
    );
    Ok(())
}

#[test]
fn every_entry_is_counted_once_per_agent_and_per_physical_skill() -> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let expectations: SymlinkedSkillsExpectations = fixture.expectations();
    let report = scan_fixture(&fixture)?;

    let by_state = |state: DiscoveryState, agent: Option<&AgentId>| {
        report
            .items
            .iter()
            .filter(|item| item.discovery == state)
            .filter(|item| agent.is_none_or(|wanted| item.agents.contains(wanted)))
            .count()
    };
    let per_agent = |agent: &AgentId| {
        report
            .items
            .iter()
            .filter(|item| item.agents.contains(agent))
            .count()
    };
    // claude: 22 library links + 2 real folders + 2 loop links + the refused link.
    assert_eq!(per_agent(&AgentId::ClaudeCode), 27);
    // codex: 14 own links + 13 dangling + 21 shared-root links.
    assert_eq!(per_agent(&AgentId::Codex), 48);
    // omp: the 21 manager links under managed-skills.
    assert_eq!(per_agent(&AgentId::Omp), 21);
    assert_eq!(report.items.len(), 96);

    // One item per entry path: no entry is merged away by its target. The refused content of
    // two cc-switch skills shows up once per root that links to them (4 + 3 + ... entries).
    assert_eq!(by_state(DiscoveryState::Valid, None), 73);
    assert_eq!(by_state(DiscoveryState::BrokenLink, None), 15);
    assert_eq!(by_state(DiscoveryState::Invalid, None), 8);
    assert_eq!(expectations.unique_valid_skills, 22);
    assert_eq!(expectations.broken_links, 15);
    assert!(report.partial, "refused entries mark the scan partial");
    Ok(())
}

#[test]
fn migrating_a_linked_skill_copies_the_resolved_folder_and_undo_restores_the_link()
-> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;
    let name = CC_SWITCH_SKILLS[0];
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, name.to_owned()))
        .ok_or("the fixture linked the skill")?
        .clone();
    let library = fixture
        .library_path(name)
        .ok_or("the fixture wrote the library skill")?;
    let selected = report
        .items
        .iter()
        .find(|item| item.path == link)
        .ok_or("the linked skill is in the scan")?;
    assert!(selected.resolved.is_some());

    let store_root = fixture.home().join(".agentuna");
    let backup_root = fixture.home().join(".agentuna-support/backups");
    std::fs::create_dir_all(&store_root)?;
    let plan = build_plan(BuildPlanRequest {
        scan: &report,
        conflicts: &at_migrate::conflicts::detect_conflicts(&report),
        resolutions: &BTreeMap::from([(name.to_owned(), ConflictResolution::KeepA)]),
        selected_paths: &BTreeSet::from([link.clone()]),
        store_root: store_root.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: backup_root.clone(),
    })?;
    let store_skill = store_root.join("skills").join(name);
    let store_step = plan
        .steps
        .iter()
        .find(|step| step.destination.as_ref() == Some(&store_skill))
        .ok_or("the plan copies the skill into the store")?;
    assert_eq!(
        store_step.source.as_deref(),
        Some(std::fs::canonicalize(&library)?.as_path()),
        "the store copy reads the resolved folder, never the link"
    );

    let mut database = Database::open_file(fixture.root().join("agentuna.db"))?;
    let roots = ApplyRoots::new([fixture.home().to_path_buf()])?;
    let mut context = ApplyPlanContext {
        store_root: &store_root,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let applied = apply_plan(&plan, &mut context)?;

    // The store holds plain content, and the agent path is now our link to it.
    assert!(store_skill.join("SKILL.md").is_file());
    let store_tree = std::fs::symlink_metadata(store_skill.join("SKILL.md"))?;
    assert!(store_tree.is_file(), "the store copy is real content");
    let deployed = std::fs::symlink_metadata(&link)?;
    assert!(deployed.is_symlink(), "the agent path is a link again");
    assert_eq!(
        std::fs::canonicalize(&link)?,
        std::fs::canonicalize(&store_skill)?,
        "the deployed link resolves to the store item"
    );
    // The library folder is untouched: migrating a link never edits its target.
    assert!(library.join("SKILL.md").is_file());
    assert!(std::fs::symlink_metadata(library.join("SKILL.md"))?.is_file());

    let undone = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: database.connection(),
            store_root: &store_root,
            backup_root: &backup_root,
            authorized_roots: &roots,
            actor: at_db::ops::OperationActor::Migration,
            force: false,
        },
        &applied.operation_id,
    )?;
    assert!(
        matches!(undone, at_migrate::undo::UndoOutcome::Restored { .. }),
        "{undone:?}"
    );
    let restored = std::fs::symlink_metadata(&link)?;
    assert!(restored.is_symlink(), "undo puts the original link back");
    assert_eq!(
        std::fs::canonicalize(&link)?,
        std::fs::canonicalize(&library)?,
        "undo restores a link to the original library folder"
    );
    assert!(library.join("SKILL.md").is_file());
    Ok(())
}

/// Builds a one-item plan for `path` from `report`, the way the migrate command does.
fn plan_for(
    fixture: &SymlinkedSkillsHome,
    report: &ScanReport,
    path: &std::path::Path,
) -> Result<at_migrate::plan::MigrationPlan, Box<dyn Error>> {
    let store_root = fixture.home().join(".agentuna");
    std::fs::create_dir_all(&store_root)?;
    Ok(build_plan(BuildPlanRequest {
        scan: report,
        conflicts: &at_migrate::conflicts::detect_conflicts(report),
        resolutions: &BTreeMap::new(),
        selected_paths: &BTreeSet::from([path.to_path_buf()]),
        store_root,
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: fixture.home().join(".agentuna-support/backups"),
    })?)
}

/// Replaces `link` so it resolves to `target`, the way a user or another tool would.
fn retarget(link: &std::path::Path, target: &std::path::Path) {
    std::fs::remove_file(link).expect("remove the reviewed link");
    std::os::unix::fs::symlink(target, link).expect("retarget the link");
}

/// One library skill copied under a new folder inside the same home.
fn copy_library_skill(
    fixture: &SymlinkedSkillsHome,
    name: &str,
    folder: &str,
) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let source = fixture
        .library_path(name)
        .ok_or("the fixture wrote the library skill")?;
    let copy = fixture.home().join(folder).join(name);
    std::fs::create_dir_all(&copy)?;
    std::fs::copy(source.join("SKILL.md"), copy.join("SKILL.md"))?;
    Ok(std::fs::canonicalize(copy)?)
}

#[test]
fn a_link_retargeted_between_review_and_apply_is_refused_as_stale() -> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;
    let name = CC_SWITCH_SKILLS[0];
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, name.to_owned()))
        .ok_or("the fixture linked the skill")?
        .clone();
    let plan = plan_for(&fixture, &report, &link)?;

    // The replacement target holds identical bytes, so only the entry identity can tell the
    // reviewed link from the user's new one.
    let copy = copy_library_skill(&fixture, name, "library-copy")?;
    retarget(&link, &copy);
    assert_eq!(
        at_store::hash_skill_folder(&copy)?.to_string(),
        plan.steps
            .iter()
            .find_map(|step| step.expected_source_hash.clone())
            .ok_or("the plan carries the reviewed hash")?,
        "identical content alone cannot detect the retarget"
    );

    let store_root = fixture.home().join(".agentuna");
    let backup_root = fixture.home().join(".agentuna-support/backups");
    let mut database = Database::open_file(fixture.root().join("retarget.db"))?;
    let roots = ApplyRoots::new([fixture.home().to_path_buf()])?;
    let mut context = ApplyPlanContext {
        store_root: &store_root,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let error = apply_plan(&plan, &mut context).expect_err("the retargeted link is refused");
    assert!(
        matches!(
            error,
            at_migrate::apply::ApplyPlanError::Plan(at_migrate::plan::PlanError::StalePlan)
        ),
        "{error}"
    );
    assert_eq!(
        std::fs::read_link(&link)?,
        copy,
        "the user's link is untouched"
    );
    assert!(
        !store_root.join("skills").join(name).exists(),
        "nothing was copied into the store"
    );
    Ok(())
}

#[test]
fn a_link_replaced_by_an_outside_link_before_apply_is_refused() -> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;
    let name = CC_SWITCH_SKILLS[1];
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, name.to_owned()))
        .ok_or("the fixture linked the skill")?
        .clone();
    let plan = plan_for(&fixture, &report, &link)?;

    // A whole different folder outside the injected home, with different content.
    let outside = fixture.root().join("outside-retarget");
    std::fs::create_dir_all(&outside)?;
    std::fs::write(outside.join("SKILL.md"), "# outside\n")?;
    let outside = std::fs::canonicalize(&outside)?;
    retarget(&link, &outside);

    let store_root = fixture.home().join(".agentuna");
    let backup_root = fixture.home().join(".agentuna-support/backups");
    let mut database = Database::open_file(fixture.root().join("outside.db"))?;
    let roots = ApplyRoots::new([fixture.home().to_path_buf()])?;
    let mut context = ApplyPlanContext {
        store_root: &store_root,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let error = apply_plan(&plan, &mut context).expect_err("the outside link is refused");
    assert!(
        matches!(
            error,
            at_migrate::apply::ApplyPlanError::Plan(at_migrate::plan::PlanError::StalePlan)
        ),
        "{error}"
    );
    assert_eq!(
        std::fs::read_link(&link)?,
        outside,
        "the outside link is preserved"
    );
    assert!(
        !store_root.join("skills").join(name).exists(),
        "nothing was copied into the store"
    );
    Ok(())
}

#[test]
fn undo_after_the_user_changed_the_migrated_link_refuses_and_preserves_it()
-> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;
    let name = CC_SWITCH_SKILLS[2];
    let link = fixture
        .links()
        .get(&(AgentId::ClaudeCode, name.to_owned()))
        .ok_or("the fixture linked the skill")?
        .clone();
    let plan = plan_for(&fixture, &report, &link)?;
    let store_root = fixture.home().join(".agentuna");
    let backup_root = fixture.home().join(".agentuna-support/backups");
    let mut database = Database::open_file(fixture.root().join("changed-link.db"))?;
    let roots = ApplyRoots::new([fixture.home().to_path_buf()])?;
    let mut context = ApplyPlanContext {
        store_root: &store_root,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let applied = apply_plan(&plan, &mut context)?;
    assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());

    // The user replaced the deployed link with one of their own before asking for undo.
    let newer = copy_library_skill(&fixture, name, "library-newer")?;
    retarget(&link, &newer);
    let outcome = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: database.connection(),
            store_root: &store_root,
            backup_root: &backup_root,
            authorized_roots: &roots,
            actor: at_db::ops::OperationActor::Migration,
            force: false,
        },
        &applied.operation_id,
    )?;
    match outcome {
        at_migrate::undo::UndoOutcome::Conflict { paths } => {
            assert_eq!(paths, vec![link.clone()], "the changed link is named");
        }
        other => panic!("undo must refuse the newer link, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_link(&link)?,
        newer,
        "the newer link is preserved"
    );
    Ok(())
}

#[test]
fn two_agents_linking_one_library_folder_share_one_store_write() -> Result<(), Box<dyn Error>> {
    let fixture = SymlinkedSkillsHome::new()?;
    let report = scan_fixture(&fixture)?;
    let name = CC_SWITCH_SKILLS[3];
    let claude = fixture
        .links()
        .get(&(AgentId::ClaudeCode, name.to_owned()))
        .ok_or("claude links the skill")?
        .clone();
    let codex = fixture
        .links()
        .get(&(AgentId::Codex, name.to_owned()))
        .ok_or("codex links the skill")?
        .clone();
    assert_eq!(
        std::fs::canonicalize(&claude)?,
        std::fs::canonicalize(&codex)?,
        "both agent links resolve to one library folder"
    );

    let store_root = fixture.home().join(".agentuna");
    std::fs::create_dir_all(&store_root)?;
    let backup_root = fixture.home().join(".agentuna-support/backups");
    let plan = build_plan(BuildPlanRequest {
        scan: &report,
        conflicts: &at_migrate::conflicts::detect_conflicts(&report),
        resolutions: &BTreeMap::new(),
        selected_paths: &BTreeSet::from([claude.clone(), codex.clone()]),
        store_root: store_root.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: backup_root.clone(),
    })?;
    assert_eq!(
        plan.steps
            .iter()
            .filter(|step| step.action == at_migrate::plan::MigrationAction::CreateStoreItem)
            .count(),
        1,
        "the shared library folder is written to the store once: {:?}",
        plan.counts
    );
    assert_eq!(
        plan.steps
            .iter()
            .filter(|step| step.action == at_migrate::plan::MigrationAction::WriteLink)
            .count(),
        2,
        "both agent paths are replaced"
    );

    let mut database = Database::open_file(fixture.root().join("two-agents.db"))?;
    let roots = ApplyRoots::new([fixture.home().to_path_buf()])?;
    let mut context = ApplyPlanContext {
        store_root: &store_root,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let applied = apply_plan(&plan, &mut context)?;
    let store_skill = store_root.join("skills").join(name);
    for link in [&claude, &codex] {
        assert_eq!(
            std::fs::canonicalize(link)?,
            std::fs::canonicalize(&store_skill)?,
            "{} now points at the one store item",
            link.display()
        );
    }
    let detail = at_db::ops::operation_detail(database.connection(), &applied.operation_id)?
        .ok_or("the operation row exists")?;
    let backups = detail
        .files
        .iter()
        .filter(|file| file.backup_id.is_some())
        .count();
    assert_eq!(
        backups, 2,
        "each replaced native link has exactly one backup entry"
    );
    Ok(())
}
