//! Real scan-to-apply migration coverage using injected temporary roots.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_fs::ApplyRoots;
use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationStrategy, build_plan, dry_run,
};
use at_migrate::scan::{ScanRequest, scan};
use at_migrate::testkit::{MigrationFixture, NATIVE_SKILL};
use at_model::Scope;

#[test]
fn applies_reviewed_plan_through_one_journalled_changeset() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let paths = fixture.paths();
    let env = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let scan = scan(ScanRequest {
        environment: &env,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let conflicts = detect_conflicts(&scan);
    let chosen_path = conflicts
        .conflicts
        .iter()
        .find(|conflict| conflict.name == NATIVE_SKILL)
        .and_then(|conflict| conflict.copies.first())
        .map(|copy| copy.path.clone())
        .ok_or("native skill conflict not discovered")?;
    let selected = scan
        .items
        .iter()
        .find(|item| item.path == chosen_path)
        .ok_or("conflict path not in scan")?;
    let selected_paths = BTreeSet::from([selected.path.clone()]);
    let resolutions = BTreeMap::from([(NATIVE_SKILL.to_owned(), ConflictResolution::KeepA)]);
    let plan = build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected_paths,
        store_root: paths.user_store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: paths.support_dir.join("backups"),
    })?;
    let mut targets = dry_run(&plan)
        .iter()
        .filter_map(|row| row.destination.clone())
        .collect::<Vec<PathBuf>>();
    targets.sort();
    targets.dedup();

    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection_mut(), 30);
    let roots = ApplyRoots::new([paths.home.clone()])?;
    let backup_root = paths.support_dir.join("backups");
    let mut context = ApplyPlanContext {
        store_root: &paths.user_store,
        backup_root: &backup_root,
        authorized_roots: &roots,
        recorder: &mut recorder,
    };
    let result = apply_plan(&plan, &mut context)?;
    assert_eq!(result.touched_paths, targets);

    assert!(!result.operation_id.is_empty());
    assert!(
        paths
            .user_store
            .join("skills")
            .join(NATIVE_SKILL)
            .join("SKILL.md")
            .is_file()
    );
    assert!(selected.path.exists());
    assert!(result.completed >= 1);
    let detail = at_db::ops::operation_detail(database.connection_mut(), &result.operation_id)?
        .ok_or("operation was not recorded")?;
    assert_eq!(
        detail.operation.kind,
        Some(at_db::ops::OperationKind::Migrate)
    );
    assert!(!detail.files.is_empty());
    Ok(())
}
