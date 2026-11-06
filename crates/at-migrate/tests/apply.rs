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
    let roots = ApplyRoots::new([paths.home.clone()])?;
    let backup_root = paths.support_dir.join("backups");
    let mut context = ApplyPlanContext {
        store_root: &paths.user_store,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
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
    let migrated_id = at_db::items::item_id_by_identity(
        database.connection(),
        at_model::ItemKind::Skill,
        Scope::User,
        None,
        NATIVE_SKILL,
    )?
    .ok_or("adopted canonical item row was not written")?;
    let migrated_id_text = migrated_id.to_string();
    let native_path = selected.path.to_string_lossy();
    let native = at_db::native::native_item_by_path(database.connection(), &native_path)?
        .ok_or("native adoption row was not written")?;
    assert_eq!(native.status, at_db::native::NativeStatus::Adopted);
    assert_eq!(
        native.adopted_item_id.as_deref(),
        Some(migrated_id_text.as_str())
    );
    let deployments =
        at_db::deployments::list_deployments_for_item(database.connection(), &migrated_id)?;
    assert_eq!(deployments.len(), selected.agents.len());
    assert!(deployments.iter().all(|deployment| {
        deployment.state == at_model::ItemState::Native
            && deployment.mode.as_deref() == Some("native")
    }));
    Ok(())
}

// Migrating an existing unmanaged native file (here `~/.codex/AGENTS.md`) must be refused until the
// adopt flow (TC-011) exists, so this scenario cannot pass yet.
#[test]
#[ignore = "TC-011: adoption of unmanaged native files is not implemented"]
fn renders_mcp_and_rule_migrations_through_adapter_projections() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
    let scan = scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?;
    let conflicts = detect_conflicts(&scan);
    let selected_mcp = conflicts
        .conflicts
        .iter()
        .find(|conflict| conflict.kind == at_model::ItemKind::McpServer)
        .and_then(|conflict| conflict.copies.first())
        .map(|copy| copy.path.clone())
        .ok_or("MCP conflict not discovered")?;
    let selected_rule = conflicts
        .conflicts
        .iter()
        .find(|conflict| conflict.kind == at_model::ItemKind::Rule)
        .and_then(|conflict| conflict.copies.first())
        .map(|copy| copy.path.clone())
        .or_else(|| {
            scan.items
                .iter()
                .find(|item| item.kind == at_model::ItemKind::Rule)
                .map(|item| item.path.clone())
        })
        .ok_or("native rule not discovered")?;
    let selected_paths = BTreeSet::from([selected_mcp.clone(), selected_rule.clone()]);
    let resolutions = conflicts
        .conflicts
        .iter()
        .map(|conflict| (conflict.name.clone(), ConflictResolution::KeepA))
        .collect::<BTreeMap<_, _>>();
    let selected_rule_item = scan
        .items
        .iter()
        .find(|item| item.path == selected_rule)
        .ok_or("selected rule not in scan")?;
    let rule_name = selected_rule_item.name.clone();
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
    assert!(
        plan.steps
            .iter()
            .any(|step| { step.action == at_migrate::plan::MigrationAction::RenderMcp })
    );
    assert!(
        plan.steps
            .iter()
            .any(|step| { step.action == at_migrate::plan::MigrationAction::RenderRule })
    );

    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;
    let roots = ApplyRoots::new([paths.home.clone()])?;
    let backup_root = paths.support_dir.join("backups");
    let mut context = ApplyPlanContext {
        store_root: &paths.user_store,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database: &mut database,
    };
    let applied = apply_plan(&plan, &mut context)?;

    let canonical_mcp = std::fs::read_to_string(paths.user_store.join("mcp.json"))?;
    assert!(canonical_mcp.contains("\"example\""));
    let canonical_rule = paths
        .user_store
        .join("rules")
        .join(format!("{rule_name}.md"));
    assert!(canonical_rule.is_file());
    assert!(!applied.operation_id.is_empty());
    Ok(())
}

#[test]
fn replaces_original_with_link_or_manifest_copy_in_one_changeset() -> Result<(), Box<dyn Error>> {
    for mode in [at_model::LinkMode::Link, at_model::LinkMode::Copy] {
        let fixture = MigrationFixture::new(mode)?;
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
        let copy_agents = if mode == at_model::LinkMode::Copy {
            BTreeSet::from([at_model::AgentId::Codex, at_model::AgentId::Omp])
        } else {
            BTreeSet::new()
        };
        let plan = build_plan(BuildPlanRequest {
            scan: &scan,
            conflicts: &conflicts,
            resolutions: &resolutions,
            selected_paths: &selected_paths,
            store_root: paths.user_store.clone(),
            copy_agents: &copy_agents,
            strategy: MigrationStrategy::ReplaceWithLinks,
            scope: Scope::User,
            backup_root: paths.support_dir.join("backups"),
        })?;
        let destination = plan
            .steps
            .iter()
            .find(|step| {
                matches!(
                    step.action,
                    at_migrate::plan::MigrationAction::WriteLink
                        | at_migrate::plan::MigrationAction::WriteCopy
                )
            })
            .and_then(|step| step.destination.clone())
            .ok_or("replacement destination missing")?;
        std::fs::create_dir_all(&paths.support_dir)?;
        let mut database = Database::open_file(&paths.db_path)?;
        let roots = ApplyRoots::new([paths.home.clone()])?;
        let backup_root = paths.support_dir.join("backups");
        let mut context = ApplyPlanContext {
            store_root: &paths.user_store,
            backup_root: &backup_root,
            authorized_roots: &roots,
            database: &mut database,
        };
        let result = apply_plan(&plan, &mut context)?;
        let detail = at_db::ops::operation_detail(database.connection_mut(), &result.operation_id)?
            .ok_or("migration operation was not recorded")?;
        assert!(
            detail.files.iter().any(|file| {
                file.path.as_deref() == Some(destination.to_str().unwrap_or_default())
                    && file.before_hash.is_some()
                    && file.backup_id.is_some()
            }),
            "the original tree has a hash-verified operation backup"
        );
        let metadata = std::fs::symlink_metadata(&destination)?;
        if mode == at_model::LinkMode::Link {
            assert!(metadata.file_type().is_symlink());
            assert_eq!(
                destination.canonicalize()?,
                paths
                    .user_store
                    .join("skills")
                    .join(NATIVE_SKILL)
                    .canonicalize()?
            );
        } else {
            assert!(metadata.is_dir());
            assert!(destination.join(at_fs::COPY_MANIFEST_FILE).is_file());
        }
        assert!(
            paths
                .user_store
                .join("skills")
                .join(NATIVE_SKILL)
                .join("SKILL.md")
                .is_file()
        );
    }
    Ok(())
}
