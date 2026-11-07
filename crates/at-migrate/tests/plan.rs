//! TC-008 plan and dry-run coverage.

#![cfg(feature = "testkit")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationAction, MigrationStrategy, PlanError,
    build_plan, dry_run, persist_plans, validate_plan,
};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport, ScanRequest, scan};
use at_migrate::testkit::MigrationFixture;
use at_model::{AgentId, LinkMode, Scope};

#[test]
fn dry_run_preserves_source_bytes_and_leave_and_watch_records_no_links()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let scanned = scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let conflicts = detect_conflicts(&scanned);
    let conflict = conflicts
        .conflicts
        .iter()
        .find(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or("fixture skill conflict missing")?;
    let source = conflict.copies[0].path.clone();
    let before = fs::read(source.join("SKILL.md"))?;
    let modified = fs::metadata(&source)?.modified()?;
    let selected_paths = BTreeSet::from([source.clone()]);
    let resolutions = BTreeMap::from([(conflict.name.clone(), ConflictResolution::KeepA)]);
    let copy_agents = BTreeSet::<AgentId>::new();
    let plan = build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected_paths,
        store_root: paths.user_store.clone(),
        copy_agents: &copy_agents,
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: paths.user_store.join("backups/operation"),
    })?;
    let rows = dry_run(&plan);
    assert_eq!(rows.len(), plan.steps.len());
    assert!(
        rows.iter()
            .all(|row| row.action != MigrationAction::WriteLink)
    );
    assert!(
        rows.iter()
            .any(|row| row.action == MigrationAction::RecordWatched)
    );
    assert_eq!(plan.counts.files_left_in_place, plan.counts.items);
    validate_plan(&plan)?;
    assert_eq!(fs::read(source.join("SKILL.md"))?, before);
    assert_eq!(fs::metadata(&source)?.modified()?, modified);
    let document = at_migrate::plan::plans_document(std::slice::from_ref(&plan))?;
    persist_plans(&paths.user_store, &document)?;
    let persisted: at_migrate::plan::PersistedPlans =
        serde_json::from_slice(&fs::read(document.path_in(&paths.user_store))?)?;
    assert_eq!(
        persisted.plan_hash,
        at_migrate::plan::plans_hash(std::slice::from_ref(&plan))?
    );
    assert_eq!(persisted.plans, vec![plan]);
    Ok(())
}

#[test]
fn empty_selection_and_changed_sources_are_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Copy)?;
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let scanned = scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let conflicts = detect_conflicts(&scanned);
    let selected_paths = BTreeSet::new();
    let resolutions = BTreeMap::new();
    let copy_agents = BTreeSet::new();
    let empty = build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected_paths,
        store_root: paths.user_store.clone(),
        copy_agents: &copy_agents,
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: paths.user_store.join("backups/operation"),
    });
    assert!(matches!(empty, Err(PlanError::EmptySelection)));

    let conflict = conflicts
        .conflicts
        .iter()
        .find(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or("fixture skill conflict missing")?;
    let selected_path = conflict.copies[0].path.clone();
    let selected_paths = BTreeSet::from([selected_path.clone()]);
    let plan = build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &conflicts,
        resolutions: &BTreeMap::from([(String::from("example-skill"), ConflictResolution::KeepA)]),
        selected_paths: &selected_paths,
        store_root: paths.user_store.clone(),
        copy_agents: &copy_agents,
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: paths.user_store.join("backups/operation"),
    })
    .map_err(|error| {
        format!(
            "{}; selected={selected_path:?}; copies={:?}",
            error, conflict.copies
        )
    })?;
    fs::write(selected_path.join("SKILL.md"), b"changed after preview")?;
    assert!(matches!(validate_plan(&plan), Err(PlanError::StalePlan)));
    Ok(())
}

// -------------------------------------------------------------------------------------------
// TC-032: planning rejects names and paths the filesystem cannot keep distinct.
// -------------------------------------------------------------------------------------------

/// A skill folder the scanner would report for `name`.
fn native_skill(dir: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    let folder = dir.join(name);
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("SKILL.md"), format!("---\nname: {name}\n---\n"))?;
    Ok(folder)
}

fn scanned_item(path: PathBuf, name: &str) -> ScanItem {
    ScanItem {
        kind: at_model::ItemKind::Skill,
        name: name.to_owned(),
        agents: BTreeSet::from([AgentId::Codex]),
        path,
        scope: Scope::User,
        content_hash: None,
        normalized_hash: None,
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    }
}

#[test]
fn case_insensitive_names_are_rejected_before_the_plan_runs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let native = temp.path().join("native");
    let upper = native_skill(&native, "Foo")?;
    let lower = native_skill(&native, "foo")?;
    let scanned = ScanReport {
        roots: Vec::new(),
        items: vec![
            scanned_item(upper.clone(), "Foo"),
            scanned_item(lower.clone(), "foo"),
        ],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = at_migrate::conflicts::ConflictSet::default();
    let selected = BTreeSet::from([upper, lower]);
    let result = build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected,
        store_root: temp.path().join("store"),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: temp.path().join("backups"),
    });
    match result {
        Err(PlanError::CaseInsensitiveCollision { first, second }) => {
            assert_eq!((first.as_str(), second.as_str()), ("Foo", "foo"));
        }
        other => return Err(format!("expected a case collision, got {other:?}").into()),
    }

    // A plan that arrives with colliding steps is refused by the same rule before apply.
    let one = native_skill(&native, "bar")?;
    let store_root = temp.path().join("store");
    at_store::layout::init_store(Scope::User, &store_root)?;
    let single = ScanReport {
        roots: Vec::new(),
        items: vec![scanned_item(one.clone(), "bar")],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let mut plan = build_plan(BuildPlanRequest {
        scan: &single,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &BTreeSet::from([one]),
        store_root: store_root.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: temp.path().join("backups"),
    })?;
    let mut duplicate = plan
        .steps
        .iter()
        .find(|step| step.action == MigrationAction::CreateStoreItem)
        .ok_or("the plan has no store-item step")?
        .clone();
    duplicate.seq = plan.steps.len() + 1;
    duplicate.item_name = Some("BAR".to_owned());
    duplicate.destination = Some(store_root.join("skills/bar"));
    plan.steps.push(duplicate);
    let payload = serde_json::to_vec(&(plan.strategy, &plan.steps))?;
    plan.plan_hash = at_fs::sha256_hex(&payload);
    assert!(matches!(
        validate_plan(&plan),
        Err(PlanError::CaseInsensitiveCollision { .. })
    ));
    Ok(())
}

#[test]
fn over_long_destinations_are_rejected_before_the_plan_runs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let folder = native_skill(&temp.path().join("native"), "long-name")?;
    let scanned = ScanReport {
        roots: Vec::new(),
        items: vec![scanned_item(folder.clone(), "long-name")],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let long_root = temp
        .path()
        .join("s".repeat(at_migrate::plan::platform_path_limit()));
    let result = build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &at_migrate::conflicts::ConflictSet::default(),
        resolutions: &BTreeMap::new(),
        selected_paths: &BTreeSet::from([folder]),
        store_root: long_root,
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: temp.path().join("backups"),
    });
    match result {
        Err(PlanError::PathTooLong { limit, .. }) => {
            assert_eq!(limit, at_migrate::plan::platform_path_limit());
        }
        other => return Err(format!("expected an over-long path, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn shared_skills_root_is_planned_once_for_both_agents() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = MigrationFixture::new(LinkMode::Link)?;
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let scanned = scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let shared_folder = paths
        .project_native_skill
        .parent()
        .ok_or("the project skill has no folder")?
        .to_path_buf();
    let shared = scanned
        .items
        .iter()
        .find(|item| item.path == shared_folder)
        .ok_or("shared project skill not discovered")?
        .clone();
    let single = ScanReport {
        roots: Vec::new(),
        items: vec![shared.clone()],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let plan = build_plan(BuildPlanRequest {
        scan: &single,
        conflicts: &at_migrate::conflicts::ConflictSet::default(),
        resolutions: &BTreeMap::new(),
        selected_paths: &BTreeSet::from([shared.path.clone()]),
        store_root: paths.user_store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: paths.support_dir.join("backups"),
    })?;
    let replacements = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.action,
                MigrationAction::WriteLink | MigrationAction::WriteCopy
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(replacements.len(), 1, "one physical root, one write");
    assert_eq!(plan.counts.links_written, 1);
    assert_eq!(
        replacements[0].agents,
        BTreeSet::from([AgentId::Codex, AgentId::Omp]),
        "the coupled agents are named on the single step"
    );
    assert_eq!(
        replacements[0].destination.as_deref(),
        Some(shared_folder.as_path())
    );
    validate_plan(&plan)?;
    Ok(())
}
