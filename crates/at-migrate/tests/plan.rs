//! TC-008 plan and dry-run coverage.

#![cfg(feature = "testkit")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationAction, MigrationStrategy, PlanError,
    build_plan, dry_run, persist_plan, validate_plan,
};
use at_migrate::scan::{ScanRequest, scan};
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
    let plan_path = persist_plan(&paths.user_store, &plan)?;
    let persisted: at_migrate::plan::MigrationPlan = serde_json::from_slice(&fs::read(plan_path)?)?;
    assert_eq!(persisted, plan);
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
