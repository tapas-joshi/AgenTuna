//! Real scan-to-apply migration coverage using injected temporary roots.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_fs::ApplyRoots;
use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationAction, MigrationStrategy, build_plan, dry_run,
};
use at_migrate::scan::{ScanReport, ScanRequest, scan};
use at_migrate::testkit::{MigrationFixture, NATIVE_SKILL};
use at_model::{AgentId, Scope};

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

// Migrating an existing unmanaged native file (here `~/.codex/AGENTS.md`) adopts it in place:
// the adapter projection wraps the original bytes in a managed block instead of refusing.
#[test]
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

// -------------------------------------------------------------------------------------------
// TC-009: canonical store, deployment and adoption rows committed with the ChangeSet.
// -------------------------------------------------------------------------------------------

/// One reviewed replacement of the shared project skill that Codex and omp both read.
struct SharedRootReplacement {
    fixture: MigrationFixture,
    plan: at_migrate::plan::MigrationPlan,
    destination: PathBuf,
    agents: BTreeSet<AgentId>,
}

impl SharedRootReplacement {
    fn new() -> Result<Self, Box<dyn Error>> {
        let fixture = MigrationFixture::link()?;
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
            .ok_or("the shared project skill was not discovered")?
            .clone();
        assert_eq!(
            shared.agents,
            BTreeSet::from([AgentId::Codex, AgentId::Omp]),
            "codex and omp read one physical .agents/skills root"
        );
        let single = ScanReport {
            items: vec![shared.clone()],
            partial: false,
            skipped_roots: Vec::new(),
        };
        let conflicts = detect_conflicts(&single);
        assert!(
            conflicts.conflicts.is_empty(),
            "one physical item cannot conflict with itself"
        );
        let plan = build_plan(BuildPlanRequest {
            scan: &single,
            conflicts: &conflicts,
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
        assert_eq!(
            replacements.len(),
            1,
            "a shared root is planned once, not once per agent"
        );
        let destination = replacements[0]
            .destination
            .clone()
            .ok_or("replacement destination missing")?;
        Ok(Self {
            fixture,
            plan,
            destination,
            agents: shared.agents,
        })
    }

    fn paths(&self) -> &at_migrate::testkit::FixturePaths {
        self.fixture.paths()
    }
}

fn apply_with_database(
    replacement: &SharedRootReplacement,
    database: &mut Database,
) -> Result<at_migrate::apply::MigrationApplyOutcome, Box<dyn Error>> {
    let paths = replacement.paths();
    let roots = ApplyRoots::new([paths.home.clone()])?;
    let backup_root = paths.support_dir.join("backups");
    let mut context = ApplyPlanContext {
        store_root: &paths.user_store,
        backup_root: &backup_root,
        authorized_roots: &roots,
        database,
    };
    Ok(apply_plan(&replacement.plan, &mut context)?)
}

#[test]
fn replacement_writes_manifest_deployment_and_adoption_rows_with_the_changeset()
-> Result<(), Box<dyn Error>> {
    let replacement = SharedRootReplacement::new()?;
    let paths = replacement.paths().clone();
    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;
    let outcome = apply_with_database(&replacement, &mut database)?;
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    // The canonical manifest names the item by its lowercase slug and records the hash.
    let layout = at_store::layout::StoreLayout::new(Scope::User, &paths.user_store);
    let manifest = at_store::manifest::StoreManifest::load(&layout)?;
    let stored = manifest
        .manifest
        .items()
        .iter()
        .find(|item| item.slug.as_str() == NATIVE_SKILL)
        .ok_or("store manifest has no migrated skill")?;
    assert_eq!(stored.path.as_str(), format!("skills/{NATIVE_SKILL}"));
    let store_dir = paths.user_store.join("skills").join(NATIVE_SKILL);
    assert!(store_dir.join("SKILL.md").is_file(), "plain store content");
    let content_hash = at_store::manifest::hash_skill_folder(&store_dir)?.to_string();
    assert_eq!(stored.hash.as_str(), content_hash);

    // The item id is the one the database resolves by identity: stable across the apply.
    let item_id = at_db::items::item_id_by_identity(
        database.connection(),
        at_model::ItemKind::Skill,
        Scope::User,
        None,
        NATIVE_SKILL,
    )?
    .ok_or("canonical item row was not written")?;
    assert_eq!(stored.id, item_id);

    // One deployment row per agent reading the shared root, with baseline hashes.
    let deployments =
        at_db::deployments::list_deployments_for_item(database.connection(), &item_id)?;
    assert_eq!(deployments.len(), replacement.agents.len());
    let destination_text = replacement.destination.to_string_lossy().to_string();
    let store_dir_text = store_dir.to_string_lossy().to_string();
    for deployment in &deployments {
        assert!(
            replacement.agents.contains(&deployment.agent_id),
            "unexpected agent {}",
            deployment.agent_id
        );
        assert_eq!(deployment.mode.as_deref(), Some("symlink"));
        assert_eq!(deployment.state, at_model::ItemState::Linked);
        assert_eq!(
            deployment.target_path.as_deref(),
            Some(destination_text.as_str())
        );
        assert_eq!(
            deployment.link_target.as_deref(),
            Some(store_dir_text.as_str())
        );
        assert_eq!(
            deployment.store_hash.as_deref(),
            Some(content_hash.as_str())
        );
        assert_eq!(
            deployment.target_hash.as_deref(),
            Some(content_hash.as_str())
        );
        assert_eq!(
            deployment.baseline_hash.as_deref(),
            Some(content_hash.as_str())
        );
        assert_eq!(
            deployment.operation_id.as_deref(),
            Some(outcome.operation_id.as_str())
        );
    }

    // The native path is recorded as adopted into the new canonical item.
    let native_path = replacement.destination.to_string_lossy().to_string();
    let native = at_db::native::native_item_by_path(database.connection(), &native_path)?
        .ok_or("native adoption row was not written")?;
    assert_eq!(native.status, at_db::native::NativeStatus::Adopted);
    assert_eq!(
        native.adopted_item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );
    assert_eq!(native.content_hash.as_deref(), Some(content_hash.as_str()));

    // One Migrate operation records the replaced original with its pre and post hashes.
    let detail = at_db::ops::operation_detail(database.connection_mut(), &outcome.operation_id)?
        .ok_or("migration operation was not recorded")?;
    assert_eq!(
        detail.operation.kind,
        Some(at_db::ops::OperationKind::Migrate)
    );
    let replaced = detail
        .files
        .iter()
        .find(|file| {
            file.path.as_deref() == Some(destination_text.as_str())
                && file.before_hash.is_some()
                && file.backup_id.is_some()
        })
        .ok_or("the replaced original has no backed-up pre-migration row")?;
    assert!(replaced.after_hash.is_none(), "the original was removed");
    assert!(
        detail.files.iter().any(|file| {
            file.path.as_deref() == Some(destination_text.as_str()) && file.after_hash.is_some()
        }),
        "the deployment that replaced it records its post-migration hash"
    );
    assert!(
        detail.files.iter().any(|file| {
            file.path
                .as_deref()
                .is_some_and(|path| path == store_dir_text)
                && file.after_hash.is_some()
                && file.before_hash.is_none()
        }),
        "the store item records its post-migration hash"
    );

    // The backup in the operation folder holds the original tree, hash-verified.
    let backup_id = replaced
        .backup_id
        .as_deref()
        .ok_or("the replaced original has no backup id")?;
    let backup = at_db::backups::backup_by_id(database.connection(), backup_id)?
        .ok_or("the backup row is missing")?;
    let blob = backup
        .backup_path
        .as_deref()
        .ok_or("the backup row has no path")?;
    let blob_hash = at_fs::changeset::hash_node(std::path::Path::new(blob))?
        .ok_or("the backup blob is missing")?;
    assert_eq!(
        Some(blob_hash.as_str()),
        replaced.before_hash.as_deref(),
        "the backup is hash-identical to the original"
    );
    Ok(())
}

#[test]
fn failed_apply_leaves_no_store_manifest_deployment_or_adoption_rows() -> Result<(), Box<dyn Error>>
{
    let replacement = SharedRootReplacement::new()?;
    let paths = replacement.paths().clone();
    std::fs::create_dir_all(&paths.support_dir)?;
    let before_manifest = std::fs::read(paths.user_store.join("store.json"))?;
    let operation_count = replacement.plan.steps.len();
    for fail_index in 0..operation_count {
        let mut database = Database::open_file(&paths.db_path)?;
        let injector = std::sync::Arc::new(FailAtChangeSetOperation {
            index: fail_index,
            current: std::sync::atomic::AtomicUsize::new(0),
        });
        let outcome = at_fs::failpoint::with_injector(injector, || {
            apply_with_database(&replacement, &mut database)
        });
        if let Ok(applied) = &outcome {
            // The injected point may be past the last ChangeSet operation; nothing to assert.
            assert!(!applied.operation_id.is_empty());
        }
        assert!(
            at_db::items::item_id_by_identity(
                database.connection(),
                at_model::ItemKind::Skill,
                Scope::User,
                None,
                NATIVE_SKILL,
            )?
            .is_none(),
            "no canonical row survives a failure at operation {fail_index}"
        );
        let native_path = replacement.destination.to_string_lossy().to_string();
        assert!(
            at_db::native::native_item_by_path(database.connection(), &native_path)?.is_none(),
            "no adoption row survives a failure at operation {fail_index}"
        );
        assert!(
            at_db::deployments::list_deployments_for_agent(
                database.connection(),
                &AgentId::Codex,
            )?
            .is_empty(),
            "no deployment row survives a failure at operation {fail_index}"
        );
        assert_eq!(
            std::fs::read(paths.user_store.join("store.json"))?,
            before_manifest,
            "the canonical manifest is unchanged by a failed apply at {fail_index}"
        );
    }
    Ok(())
}

struct FailAtChangeSetOperation {
    index: usize,
    current: std::sync::atomic::AtomicUsize,
}

impl at_fs::failpoint::FailInjector for FailAtChangeSetOperation {
    fn fault(
        &self,
        point: at_fs::failpoint::FaultPoint,
        _path: &std::path::Path,
    ) -> Option<at_fs::failpoint::Fault> {
        use std::sync::atomic::Ordering;
        (point == at_fs::failpoint::FaultPoint::ChangeSetOperation
            && self.current.fetch_add(1, Ordering::SeqCst) == self.index)
            .then_some(at_fs::failpoint::Fault::Fail(5))
    }
}

// -------------------------------------------------------------------------------------------
// TC-032: link replacement, copy fallback and permission failures.
// -------------------------------------------------------------------------------------------

#[test]
fn rejected_symlink_falls_back_to_copy_and_records_copy_mode_with_a_warning()
-> Result<(), Box<dyn Error>> {
    let replacement = SharedRootReplacement::new()?;
    let paths = replacement.paths().clone();
    let original_skill = replacement.destination.join("SKILL.md");
    let original_bytes = std::fs::read(&original_skill)?;
    let original_mode = std::fs::metadata(&original_skill)?.permissions().mode() & 0o777;
    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;
    let injector = std::sync::Arc::new(at_fs::failpoint::TestInjector::new());
    injector.arm(
        at_fs::failpoint::FaultPoint::DeploySymlink,
        at_fs::failpoint::Fault::Fail(rustix::io::Errno::PERM.raw_os_error()),
    );
    let outcome = at_fs::failpoint::with_injector(injector, || {
        apply_with_database(&replacement, &mut database)
    })?;

    // The migration completed, reported the downgrade and left no partial link.
    assert_eq!(
        outcome.warnings,
        vec![at_migrate::apply::MigrationWarning::CopyFallback {
            path: replacement.destination.clone(),
            observed_mode: "copy",
        }]
    );
    let metadata = std::fs::symlink_metadata(&replacement.destination)?;
    assert!(metadata.is_dir() && !metadata.is_symlink());
    assert!(
        replacement
            .destination
            .join(at_fs::COPY_MANIFEST_FILE)
            .is_file()
    );

    // The deployment row records the mode that exists, so the UI cannot claim a link.
    let item_id = at_db::items::item_id_by_identity(
        database.connection(),
        at_model::ItemKind::Skill,
        Scope::User,
        None,
        NATIVE_SKILL,
    )?
    .ok_or("canonical item row was not written")?;
    let deployments =
        at_db::deployments::list_deployments_for_item(database.connection(), &item_id)?;
    assert_eq!(deployments.len(), replacement.agents.len());
    for deployment in &deployments {
        assert_eq!(deployment.mode.as_deref(), Some("copy"));
        assert!(deployment.link_target.is_none());
        assert_eq!(deployment.state, at_model::ItemState::Linked);
    }

    // Undo removes the copy and restores the original bytes and mode, with no dangling link.
    let roots = ApplyRoots::new([paths.home.clone()])?;
    let backup_root = paths.support_dir.join("backups");
    let undo = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: database.connection(),
            store_root: &paths.user_store,
            backup_root: &backup_root,
            authorized_roots: &roots,
            actor: at_db::ops::OperationActor::Migration,
        },
        &outcome.operation_id,
    )?;
    assert!(matches!(
        undo,
        at_migrate::undo::UndoOutcome::Restored { .. }
    ));
    let restored = std::fs::symlink_metadata(&replacement.destination)?;
    assert!(
        restored.is_dir() && !restored.is_symlink(),
        "undo restores a real folder, not a link"
    );
    assert!(
        !replacement
            .destination
            .join(at_fs::COPY_MANIFEST_FILE)
            .exists()
    );
    assert_eq!(std::fs::read(&original_skill)?, original_bytes);
    assert_eq!(
        std::fs::metadata(&original_skill)?.permissions().mode() & 0o777,
        original_mode
    );
    Ok(())
}

#[test]
fn permission_denied_agent_folder_is_typed_and_leaves_originals_intact()
-> Result<(), Box<dyn Error>> {
    if rustix::process::geteuid().as_raw() == 0 {
        return Ok(());
    }
    let replacement = SharedRootReplacement::new()?;
    let paths = replacement.paths().clone();
    let skills_root = replacement
        .destination
        .parent()
        .ok_or("replacement has no parent folder")?
        .to_path_buf();
    let original_skill = replacement.destination.join("SKILL.md");
    let original_bytes = std::fs::read(&original_skill)?;
    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;

    std::fs::set_permissions(&skills_root, std::fs::Permissions::from_mode(0o500))?;
    let result = apply_with_database(&replacement, &mut database);
    std::fs::set_permissions(&skills_root, std::fs::Permissions::from_mode(0o755))?;

    let error = result
        .err()
        .ok_or("a read-only agent folder must be refused")?;
    let rendered = error.to_string();
    assert!(
        matches!(
            error.downcast_ref::<at_migrate::apply::ApplyPlanError>(),
            Some(at_migrate::apply::ApplyPlanError::PermissionDenied { .. })
        ),
        "expected a typed permission error, got {rendered}"
    );
    assert!(
        rendered.contains(&skills_root.to_string_lossy().to_string()),
        "the error names the folder: {rendered}"
    );
    assert!(rendered.contains("permission denied"), "{rendered}");
    // Nothing was written: the original, the store and the index are untouched.
    assert_eq!(std::fs::read(&original_skill)?, original_bytes);
    assert!(!paths.user_store.join("skills").join(NATIVE_SKILL).exists());
    assert!(
        at_db::items::item_id_by_identity(
            database.connection(),
            at_model::ItemKind::Skill,
            Scope::User,
            None,
            NATIVE_SKILL,
        )?
        .is_none()
    );

    // The same plan applies once the folder is writable, so the refusal was the permission.
    let outcome = apply_with_database(&replacement, &mut database)?;
    assert!(!outcome.operation_id.is_empty());
    assert!(
        paths
            .user_store
            .join("skills")
            .join(NATIVE_SKILL)
            .join("SKILL.md")
            .is_file()
    );
    Ok(())
}

#[test]
fn backup_failure_aborts_with_no_mutation() -> Result<(), Box<dyn Error>> {
    let replacement = SharedRootReplacement::new()?;
    let paths = replacement.paths().clone();
    let original_skill = replacement.destination.join("SKILL.md");
    let original_bytes = std::fs::read(&original_skill)?;
    let before_manifest = std::fs::read(paths.user_store.join("store.json"))?;
    std::fs::create_dir_all(&paths.support_dir)?;
    let mut database = Database::open_file(&paths.db_path)?;

    // The canonical manifest is replaced last, so its backup is the first backup this plan
    // writes. Refusing that write must undo every earlier step.
    let injector = std::sync::Arc::new(at_fs::failpoint::TestInjector::new());
    injector.arm(
        at_fs::failpoint::FaultPoint::BackupWrite,
        at_fs::failpoint::Fault::Fail(rustix::io::Errno::NOSPC.raw_os_error()),
    );
    let result = at_fs::failpoint::with_injector(injector, || {
        apply_with_database(&replacement, &mut database)
    });
    assert!(result.is_err(), "a refused backup must fail the apply");

    assert_eq!(std::fs::read(&original_skill)?, original_bytes);
    assert!(
        !paths.user_store.join("skills").join(NATIVE_SKILL).exists(),
        "no store item survives a backup failure"
    );
    assert_eq!(
        std::fs::read(paths.user_store.join("store.json"))?,
        before_manifest
    );
    assert!(
        at_db::items::item_id_by_identity(
            database.connection(),
            at_model::ItemKind::Skill,
            Scope::User,
            None,
            NATIVE_SKILL,
        )?
        .is_none()
    );
    Ok(())
}
