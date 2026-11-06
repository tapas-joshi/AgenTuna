//! TF-048: migration fixtures, snapshots and the round-trip harness.
//!
//! The [`Scenario`] below drives the real engine: `at_migrate::scan` → `conflicts` →
//! `plan::build_plan` → `apply::apply_plan` → `undo::undo_operation`, with the at-fs journal
//! underneath. Nothing in the harness stands in for the implementation, so every assertion is
//! about what the production code did to the filesystem, not about a simulation of it.

#![cfg(feature = "testkit")]

use at_fs::{ApplyRoots, COPY_MANIFEST_FILE};
use at_migrate::testkit::{
    ApplyReport, ChangedField, DiffEntry, DryRunReport, MANAGED_SKILL, MigrationFixture,
    MigrationStep, MigrationUnderTest, NATIVE_SKILL, PlanReport, Refusal, RoundTrip, SKILL_FILE,
    ScanReport, UndoReport,
};
use at_model::{AgentId, LinkMode};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// TF-023's fixed undo window, until it lands as `UNDO_WINDOW_DAYS`.
const UNDO_WINDOW_DAYS: u64 = 30;

#[test]
fn fixture_builds_native_agents_and_conflicting_duplicates() -> Result<(), Box<dyn Error>> {
    for mode in [LinkMode::Link, LinkMode::Copy] {
        let fixture = MigrationFixture::new(mode)?;
        let paths = fixture.paths();

        assert_eq!(paths.agents.len(), 3, "claude, codex and omp are installed");
        let mut agents = BTreeSet::new();
        for agent in &paths.agents {
            agents.insert(agent.agent.clone());
            assert_eq!(agent.skill_name, NATIVE_SKILL);
            assert_eq!(agent.mcp_name, "example");
            assert!(agent.skill_path.is_file(), "{}", agent.skill_path.display());
            assert!(agent.mcp_path.is_file(), "{}", agent.mcp_path.display());
            assert!(
                agent.rules_path.as_ref().is_some_and(|path| path.is_file()),
                "{:?} has a rules file",
                agent.agent
            );
            let skill_dir = agent.skill_path.parent().expect("skill folder");
            let meta = fs::symlink_metadata(skill_dir)?;
            assert!(
                meta.is_dir() && !meta.is_symlink(),
                "native items are real folders"
            );
        }
        assert_eq!(agents.len(), 3);

        // Two project copies of the same skill name with different bytes, both differing from
        // the home copies.
        let home_skill = fs::read(&paths.agents[0].skill_path)?;
        let project_agent = fs::read(&paths.project_native_skill)?;
        let project_claude = fs::read(&paths.project_claude_skill)?;
        assert_ne!(home_skill, project_agent);
        assert_ne!(home_skill, project_claude);
        assert_ne!(project_agent, project_claude);

        // The project MCP server of the same name conflicts with the home one.
        let home_mcp = fs::read_to_string(&paths.agents[0].mcp_path)?;
        let project_mcp = fs::read_to_string(&paths.project_mcp)?;
        assert!(home_mcp.contains("\"example\""));
        assert!(project_mcp.contains("\"example\""));
        assert!(project_mcp.contains("\"uvx\""));
        assert_ne!(home_mcp, project_mcp);

        // Both stores are initialised and the store skill exists.
        assert!(paths.user_store.join("store.json").is_file());
        assert!(paths.project_store.join("store.json").is_file());
        assert!(paths.store_skill.is_file());
        let store_mode = fs::metadata(&paths.store_skill)?.permissions().mode() & 0o777;
        assert_eq!(store_mode, 0o600, "store content is private");

        // The already-deployed skill has the shape of the configured mode.
        let deployment = fs::symlink_metadata(&paths.managed_deployment)?;
        match mode {
            LinkMode::Link => {
                assert!(deployment.file_type().is_symlink());
                let target = fs::read_link(&paths.managed_deployment)?;
                let resolved = paths
                    .managed_deployment
                    .parent()
                    .expect("skills folder")
                    .join(target);
                let resolved = fs::canonicalize(&resolved)?;
                assert!(resolved.starts_with(fs::canonicalize(&paths.user_store)?));
                assert!(resolved.join(SKILL_FILE).is_file());
            }
            LinkMode::Copy => {
                assert!(deployment.is_dir() && !deployment.is_symlink());
                assert!(paths.managed_deployment.join(COPY_MANIFEST_FILE).is_file());
                assert!(paths.managed_deployment.join(SKILL_FILE).is_file());
            }
        }

        // Every fixture path maps to a snapshot key under one of the two roots.
        for path in [
            &paths.agents[0].skill_path,
            &paths.project_native_skill,
            &paths.managed_deployment,
        ] {
            assert!(fixture.key(path).is_some(), "{}", path.display());
        }
    }
    Ok(())
}

#[test]
fn snapshot_detects_mode_hash_link_and_presence_changes() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let paths = fixture.paths();
    let filter = fixture.filter();
    let before = filter.capture()?;
    assert!(!before.is_empty());

    let claude_skill = &paths.agents[0].skill_path;
    let claude_key = fixture.key(claude_skill).expect("key");

    // A single byte changes the hash.
    let original = fs::read(claude_skill)?;
    let mut edited = original.clone();
    edited.push(b'!');
    fs::write(claude_skill, &edited)?;
    let after = filter.capture()?;
    let diff = before.diff(&after);
    let entry = diff
        .entries()
        .iter()
        .find(|entry| entry.path() == claude_key)
        .expect("the edited file is in the diff");
    assert!(matches!(
        entry,
        DiffEntry::Changed { fields, .. } if fields.contains(&ChangedField::Hash)
    ));

    // A mode change is visible even when the bytes are unchanged.
    fs::write(claude_skill, &original)?;
    fs::set_permissions(claude_skill, fs::Permissions::from_mode(0o600))?;
    let after_mode = filter.capture()?;
    let mode_diff = before.diff(&after_mode);
    assert!(mode_diff.entries().iter().any(|entry| matches!(
        entry,
        DiffEntry::Changed { path, fields, .. }
            if path == &claude_key && fields == &vec![ChangedField::Mode]
    )));
    fs::set_permissions(claude_skill, fs::Permissions::from_mode(0o644))?;

    // Retargeting a symlink is visible even though the link is never followed.
    let deployment = &paths.managed_deployment;
    let deployment_key = fixture.key(deployment).expect("key");
    let original_target = fs::read_link(deployment)?;
    fs::remove_file(deployment)?;
    std::os::unix::fs::symlink(&paths.store_skill, deployment)?;
    let link_diff = before.diff(&filter.capture()?);
    assert!(link_diff.entries().iter().any(|entry| matches!(
        entry,
        DiffEntry::Changed { path, fields, .. }
            if path == &deployment_key
                && fields.contains(&ChangedField::LinkTarget)
                && fields.contains(&ChangedField::Hash)
    )));
    fs::remove_file(deployment)?;
    std::os::unix::fs::symlink(&original_target, deployment)?;

    // Removal and creation are listed by path.
    let removed = paths.project_mcp.clone();
    let removed_key = fixture.key(&removed).expect("key");
    fs::remove_file(&removed)?;
    let added = paths.home.join("home-only.txt");
    fs::write(&added, b"new")?;
    let added_key = fixture.key(&added).expect("key");
    let presence = before.diff(&filter.capture()?);
    let paths_changed = presence.paths();
    assert!(paths_changed.contains(&removed_key));
    assert!(paths_changed.contains(&added_key));
    assert!(presence.entries().iter().any(|entry| matches!(
        entry,
        DiffEntry::Removed { path, .. } if path == &removed_key
    )));
    assert!(presence.entries().iter().any(|entry| matches!(
        entry,
        DiffEntry::Added { path, .. } if path == &added_key
    )));

    // A symlink pointing outside the snapshot roots is recorded, never followed.
    let escape = paths.home.join("escape");
    std::os::unix::fs::symlink("/etc", &escape)?;
    let escaped = filter.capture()?;
    let escape_key = fixture.key(&escape).expect("key");
    let entry = escaped.entry(&escape_key).expect("symlink is captured");
    assert_eq!(entry.link_target.as_deref(), Some(Path::new("/etc")));
    assert!(
        escaped
            .keys()
            .all(|key| !key.starts_with(&format!("{escape_key}/"))),
        "the snapshot must not walk through a symlink"
    );

    Ok(())
}

#[test]
fn dry_run_writes_nothing_and_reports_exactly_the_apply_targets() -> Result<(), Box<dyn Error>> {
    for mode in [LinkMode::Link, LinkMode::Copy] {
        let fixture = MigrationFixture::new(mode)?;
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;

        let mut trip = RoundTrip::begin(filter)?;
        trip.record(MigrationStep::Scan, || scenario.scan())?;
        trip.record(MigrationStep::Plan, || scenario.plan())?;
        trip.record(MigrationStep::DryRun, || scenario.dry_run())?;
        assert!(
            !trip
                .last()
                .expect("dry run recorded")
                .outcome
                .reported
                .is_empty(),
            "the dry run reports the paths it would touch"
        );
        trip.record(MigrationStep::Apply, || scenario.apply())?;

        trip.ensure_step_untouched(MigrationStep::DryRun)?;
        trip.ensure_dry_run_matched_apply()?;
    }
    Ok(())
}

#[test]
fn apply_then_undo_restores_every_path_including_modes_and_links() -> Result<(), Box<dyn Error>> {
    for mode in [LinkMode::Link, LinkMode::Copy] {
        let fixture = MigrationFixture::new(mode)?;
        let paths = fixture.paths().clone();
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;
        let target_file = scenario.target_path().join(SKILL_FILE);
        let before = fs::read(&target_file)?;
        let deployed_key = scenario.key(&paths.managed_deployment);
        let deployed_target = filter.capture()?.entry(&deployed_key).cloned();

        let trip = at_migrate::testkit::run_round_trip(filter, &mut scenario)?;

        assert_eq!(trip.steps().len(), MigrationStep::ORDER.len());
        for step in MigrationStep::ORDER {
            let record = trip.last_record(step).expect("every step is recorded");
            assert!(
                record.completed(),
                "{step} must succeed: {:?}",
                record.outcome
            );
        }
        trip.ensure_dry_run_matched_apply()?;
        trip.ensure_undo_restored()?;

        // The replaced native skill holds its original bytes and mode again.
        assert_eq!(fs::read(&target_file)?, before);
        assert_eq!(
            file_mode(&target_file)?,
            0o644,
            "undo restores the original mode"
        );
        // The deployment the fixture created is untouched, link target included.
        assert_eq!(
            trip.current().entry(&deployed_key).cloned(),
            deployed_target,
            "the pre-existing deployment is byte-identical"
        );
        // Everything apply created is gone; the fixture's own store item stays.
        let created_store_skill = paths
            .user_store
            .join("skills")
            .join(NATIVE_SKILL)
            .join(SKILL_FILE);
        assert!(!created_store_skill.exists());
        assert!(paths.store_skill.is_file());
        let omp_target = scenario
            .fixture()
            .deployment_target(AgentId::Omp, MANAGED_SKILL)
            .expect("omp skills folder");
        assert!(
            fs::symlink_metadata(&omp_target).is_err(),
            "the deployment added by apply is removed"
        );
    }
    Ok(())
}

#[test]
fn undo_after_an_external_edit_refuses_and_keeps_the_edit() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let paths = fixture.paths().clone();
    let filter = fixture.filter();
    let mut scenario = Scenario::new(fixture)?;

    let mut trip = RoundTrip::begin(filter)?;
    trip.record(MigrationStep::Scan, || scenario.scan())?;
    trip.record(MigrationStep::Plan, || scenario.plan())?;
    trip.record(MigrationStep::DryRun, || scenario.dry_run())?;
    trip.record(MigrationStep::Apply, || scenario.apply())?;
    assert!(trip.last().expect("apply recorded").completed());

    let edited = scenario.target_path().to_path_buf();
    let edited_key = scenario.key(&edited);
    fs::remove_file(&edited)?;
    fs::create_dir(&edited)?;
    fs::write(edited.join(SKILL_FILE), b"edited outside AgenTuna\n")?;

    trip.record(MigrationStep::Undo, || scenario.undo())?;
    trip.ensure_refused(MigrationStep::Undo, &Refusal::Conflict)?;
    trip.ensure_refusal_named(MigrationStep::Undo, &edited_key)?;

    // Nothing was restored: the edit survives and the store item is still in place.
    assert_eq!(
        fs::read(edited.join(SKILL_FILE))?,
        b"edited outside AgenTuna\n"
    );
    assert!(paths.store_skill.is_file(), "no partial restore");
    Ok(())
}

#[test]
fn undo_after_the_window_refuses_as_expired_and_restores_at_the_edge() -> Result<(), Box<dyn Error>>
{
    for (age, refuses) in [(31, true), (29, false), (UNDO_WINDOW_DAYS, false)] {
        let fixture = MigrationFixture::copy()?;
        let paths = fixture.paths().clone();
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;

        let mut trip = RoundTrip::begin(filter)?;
        trip.record(MigrationStep::Scan, || scenario.scan())?;
        trip.record(MigrationStep::Plan, || scenario.plan())?;
        trip.record(MigrationStep::DryRun, || scenario.dry_run())?;
        trip.record(MigrationStep::Apply, || scenario.apply())?;
        assert!(trip.last().expect("apply recorded").completed());
        let after_apply = trip.current().clone();

        scenario.set_age_days(age);
        trip.record(MigrationStep::Undo, || scenario.undo())?;

        if refuses {
            trip.ensure_refused(MigrationStep::Undo, &Refusal::Expired)?;
            let created_store_skill = paths
                .user_store
                .join("skills")
                .join(NATIVE_SKILL)
                .join(SKILL_FILE);
            trip.ensure_refusal_named(MigrationStep::Undo, &scenario.key(&created_store_skill))?;
            after_apply
                .diff(trip.current())
                .excluding(scenario.fixture().filter().bookkeeping())
                .ensure_empty("an expired undo restores nothing")
                .map_err(|error| -> Box<dyn Error> { Box::new(error) })?;
        } else {
            assert!(trip.last().expect("undo recorded").completed());
            trip.ensure_undo_restored()?;
        }
    }
    Ok(())
}

#[test]
fn apply_failure_midway_rolls_back_every_touched_path() -> Result<(), Box<dyn Error>> {
    for mode in [LinkMode::Link, LinkMode::Copy] {
        let fixture = MigrationFixture::new(mode)?;
        let paths = fixture.paths().clone();
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;
        let native_target = scenario.target_path().to_path_buf();
        let original_native_skill = fs::read(native_target.join(SKILL_FILE))?;
        let operation_count = scenario.ops_len();
        for fail_index in 0..operation_count {
            scenario.fail_at(Some(fail_index));
            let mut trip = RoundTrip::begin(filter.clone())?;
            trip.record(MigrationStep::Scan, || scenario.scan())?;
            trip.record(MigrationStep::Plan, || scenario.plan())?;
            trip.record(MigrationStep::DryRun, || scenario.dry_run())?;
            trip.record(MigrationStep::Apply, || scenario.apply())?;

            trip.ensure_failed_apply_rolled_back()?;
            let record = trip.last().expect("apply recorded");
            assert!(
                record.failure().is_some(),
                "injected failure at operation {fail_index} is recorded"
            );
            let omp_target = scenario
                .fixture()
                .deployment_target(AgentId::Omp, MANAGED_SKILL)
                .expect("omp skills folder");
            assert!(fs::symlink_metadata(&omp_target).is_err());
            let created_store_skill = paths
                .user_store
                .join("skills")
                .join(NATIVE_SKILL)
                .join(SKILL_FILE);
            assert!(!created_store_skill.exists());
            assert!(!created_store_skill.parent().expect("skill folder").exists());
            assert_eq!(
                fs::read(native_target.join(SKILL_FILE))?,
                original_native_skill
            );
            assert_eq!(file_mode(&native_target.join(SKILL_FILE))?, 0o644);
            assert!(paths.project_claude_skill.is_file());
        }
    }
    Ok(())
}

#[test]
fn a_second_undo_changes_nothing() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let filter = fixture.filter();
    // SQLite rewrites its `-shm` sidecar whenever the index is opened; that is bookkeeping.
    let bookkeeping = filter.bookkeeping().to_vec();
    let mut scenario = Scenario::new(fixture)?;

    let mut trip = at_migrate::testkit::run_round_trip(filter, &mut scenario)?;
    trip.ensure_undo_restored()?;
    let after_first_undo = trip.current().clone();

    trip.record(MigrationStep::Undo, || scenario.undo())?;
    trip.ensure_undo_idempotent()?;
    trip.current()
        .diff(&after_first_undo)
        .excluding(&bookkeeping)
        .ensure_empty("a second undo must not change the payload")?;
    assert_eq!(
        trip.last().expect("second undo").refusal(),
        Some(&Refusal::Other("already undone".to_owned()))
    );
    Ok(())
}

// -------------------------------------------------------------------------------------------
// Real scan, plan, apply and undo driver.
// -------------------------------------------------------------------------------------------

struct Scenario {
    fixture: MigrationFixture,
    scan: at_migrate::scan::ScanReport,
    plan: at_migrate::plan::MigrationPlan,
    database: at_db::Database,
    fail_at: Option<usize>,
    age_days: u64,
    operation_id: Option<String>,
    target: PathBuf,
}

impl Scenario {
    fn new(fixture: MigrationFixture) -> Result<Self, Box<dyn Error>> {
        let paths = fixture.paths();
        let environment = at_adapters::adapter::AgentEnvironment {
            home: paths.home.clone(),
            project: Some(paths.project.clone()),
            variables: Default::default(),
        };
        let adapters = at_adapters::adapter::AgentAdapterRegistry::register();
        let stores = vec![paths.user_store.clone(), paths.project_store.clone()];
        let scan = at_migrate::scan::scan(at_migrate::scan::ScanRequest {
            environment: &environment,
            adapters: &adapters,
            store_roots: &stores,
            store_items: &[],
        })?;
        let conflicts = at_migrate::conflicts::detect_conflicts(&scan);
        let conflict = conflicts
            .conflicts
            .iter()
            .find(|conflict| {
                conflict.kind == at_model::ItemKind::Skill && conflict.name == NATIVE_SKILL
            })
            .ok_or("native skill conflict was not discovered")?;
        let selected = conflict
            .copies
            .first()
            .ok_or("native skill conflict has no copies")?;
        let selected_item = scan
            .items
            .iter()
            .find(|item| item.path == selected.path)
            .ok_or("selected conflict copy was not in scan")?;
        let selected_paths = BTreeSet::from([selected.path.clone()]);
        let resolutions = BTreeMap::from([(
            conflict.name.clone(),
            at_migrate::plan::ConflictResolution::KeepA,
        )]);
        let copy_agents = if fixture.mode() == LinkMode::Copy {
            selected_item.agents.clone()
        } else {
            BTreeSet::new()
        };
        let target = selected_item.path.clone();
        let plan = at_migrate::plan::build_plan(at_migrate::plan::BuildPlanRequest {
            scan: &scan,
            conflicts: &conflicts,
            resolutions: &resolutions,
            selected_paths: &selected_paths,
            store_root: paths.user_store.clone(),
            copy_agents: &copy_agents,
            strategy: at_migrate::plan::MigrationStrategy::ReplaceWithLinks,
            scope: at_model::Scope::User,
            backup_root: paths.support_dir.join("backups"),
        })?;
        fs::create_dir_all(&paths.support_dir)?;
        let database = at_db::Database::open_file(&paths.db_path)?;
        Ok(Self {
            fixture,
            scan,
            plan,
            database,
            fail_at: None,
            age_days: 0,
            operation_id: None,
            target,
        })
    }

    fn fixture(&self) -> &MigrationFixture {
        &self.fixture
    }

    fn key(&self, path: &Path) -> String {
        self.fixture
            .key(path)
            .expect("scenario paths live inside the fixture")
    }

    fn target_path(&self) -> &Path {
        &self.target
    }

    fn ops_len(&self) -> usize {
        self.plan
            .steps
            .iter()
            .map(|step| match step.action {
                at_migrate::plan::MigrationAction::CreateStoreItem
                | at_migrate::plan::MigrationAction::UpdateManifest => 1,
                at_migrate::plan::MigrationAction::WriteLink
                | at_migrate::plan::MigrationAction::WriteCopy => 2,
                at_migrate::plan::MigrationAction::RenderMcp
                | at_migrate::plan::MigrationAction::RenderRule => 2,
                _ => 0,
            })
            .sum()
    }

    fn fail_at(&mut self, index: Option<usize>) {
        self.fail_at = index;
    }

    fn set_age_days(&mut self, days: u64) {
        self.age_days = days;
    }

    fn planned_keys(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut keys = BTreeSet::new();
        for step in &self.plan.steps {
            match step.action {
                at_migrate::plan::MigrationAction::CreateStoreItem => {
                    let source = step.source.as_ref().ok_or("store-item source missing")?;
                    let destination = step
                        .destination
                        .as_ref()
                        .ok_or("store-item destination missing")?;
                    keys.insert(self.key(destination));
                    for relative in walk_tree(source)? {
                        keys.insert(self.key(&destination.join(relative)));
                    }
                }
                at_migrate::plan::MigrationAction::UpdateManifest => {
                    keys.insert(
                        self.key(
                            step.destination
                                .as_ref()
                                .ok_or("manifest destination missing")?,
                        ),
                    );
                }
                at_migrate::plan::MigrationAction::WriteLink
                | at_migrate::plan::MigrationAction::WriteCopy => {
                    let destination = step
                        .destination
                        .as_ref()
                        .ok_or("deployment destination missing")?;
                    if step.action == at_migrate::plan::MigrationAction::WriteCopy {
                        keys.insert(self.key(&destination.join(COPY_MANIFEST_FILE)));
                    } else {
                        keys.insert(self.key(destination));
                        for relative in walk_tree(destination)? {
                            keys.insert(self.key(&destination.join(relative)));
                        }
                    }
                }
                at_migrate::plan::MigrationAction::RenderMcp
                | at_migrate::plan::MigrationAction::RenderRule => {
                    keys.insert(self.key(step.source.as_ref().ok_or("render source missing")?));
                    keys.insert(
                        self.key(
                            step.destination
                                .as_ref()
                                .ok_or("render destination missing")?,
                        ),
                    );
                }
                at_migrate::plan::MigrationAction::BackupOriginal
                | at_migrate::plan::MigrationAction::RecordWatched => {}
                _ => return Err("unknown migration action".into()),
            }
        }
        Ok(keys.into_iter().collect())
    }

    fn apply_real(&mut self) -> Result<at_migrate::apply::MigrationApplyOutcome, String> {
        let paths = self.fixture.paths();
        let roots = ApplyRoots::new([paths.home.clone()]).map_err(|error| error.to_string())?;
        let backup_root = paths.support_dir.join("backups");
        let mut context = at_migrate::apply::ApplyPlanContext {
            store_root: &paths.user_store,
            backup_root: &backup_root,
            authorized_roots: &roots,
            database: &mut self.database,
        };
        match self.fail_at {
            Some(index) => {
                let injector = std::sync::Arc::new(FailAtChangeSetOperation {
                    index,
                    current: std::sync::atomic::AtomicUsize::new(0),
                });
                at_fs::failpoint::with_injector(injector, || {
                    at_migrate::apply::apply_plan(&self.plan, &mut context)
                })
                .map_err(|error| error.to_string())
            }
            None => at_migrate::apply::apply_plan(&self.plan, &mut context)
                .map_err(|error| error.to_string()),
        }
    }

    fn undo_real(&mut self) -> Result<UndoReport, String> {
        let operation_id = self
            .operation_id
            .as_deref()
            .ok_or_else(|| "apply operation id is missing".to_owned())?;
        if self.age_days > UNDO_WINDOW_DAYS {
            self.database
                .connection()
                .execute(
                    "UPDATE operations SET started_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                    [operation_id],
                )
                .map_err(|error| error.to_string())?;
        }
        let paths = self.fixture.paths();
        let roots = ApplyRoots::new([paths.home.clone()]).map_err(|error| error.to_string())?;
        let backup_root = paths.support_dir.join("backups");
        let result = at_migrate::undo::undo_operation(
            at_migrate::undo::UndoOperationRequest {
                connection: self.database.connection(),
                store_root: &paths.user_store,
                backup_root: &backup_root,
                authorized_roots: &roots,
                actor: at_db::ops::OperationActor::Migration,
                force: false,
            },
            operation_id,
        )
        .map_err(|error| format!("{error:?}"))?;
        let affected = self.planned_keys().map_err(|error| error.to_string())?;
        match result {
            at_migrate::undo::UndoOutcome::Restored { .. } => Ok(UndoReport::restored(affected)),
            at_migrate::undo::UndoOutcome::Conflict { paths } => Ok(UndoReport::refused(
                Refusal::Conflict,
                paths.iter().map(|path| self.key(path)),
            )),
            at_migrate::undo::UndoOutcome::Expired => {
                let store_skill = paths
                    .user_store
                    .join("skills")
                    .join(NATIVE_SKILL)
                    .join(SKILL_FILE);
                Ok(UndoReport::refused(
                    Refusal::Expired,
                    [self.key(&store_skill)],
                ))
            }
            at_migrate::undo::UndoOutcome::AlreadyUndone => Ok(UndoReport::refused(
                Refusal::Other("already undone".to_owned()),
                Vec::new(),
            )),
            at_migrate::undo::UndoOutcome::Partial { paths } => Ok(UndoReport::refused(
                Refusal::Other("partial undo".to_owned()),
                paths.iter().map(|path| self.key(path)),
            )),
            _ => Err("unknown undo outcome".to_owned()),
        }
    }
}

struct FailAtChangeSetOperation {
    index: usize,
    current: std::sync::atomic::AtomicUsize,
}

impl at_fs::failpoint::FailInjector for FailAtChangeSetOperation {
    fn fault(
        &self,
        point: at_fs::failpoint::FaultPoint,
        _path: &Path,
    ) -> Option<at_fs::failpoint::Fault> {
        use std::sync::atomic::Ordering;
        (point == at_fs::failpoint::FaultPoint::ChangeSetOperation
            && self.current.fetch_add(1, Ordering::SeqCst) == self.index)
            .then_some(at_fs::failpoint::Fault::Fail(5))
    }
}

impl MigrationUnderTest for Scenario {
    type Error = String;

    fn scan(&mut self) -> Result<ScanReport, Self::Error> {
        Ok(ScanReport::new(self.scan.items.iter().map(|item| {
            format!(
                "{}:{}@{}",
                item.kind,
                item.name,
                item.agents
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+")
            )
        })))
    }

    fn plan(&mut self) -> Result<PlanReport, Self::Error> {
        self.planned_keys()
            .map(PlanReport::new)
            .map_err(|error| error.to_string())
    }

    fn dry_run(&mut self) -> Result<DryRunReport, Self::Error> {
        let rows = at_migrate::plan::dry_run(&self.plan);
        if rows.is_empty() {
            return Err("real migration dry run returned no rows".to_owned());
        }
        self.planned_keys()
            .map(DryRunReport::new)
            .map_err(|error| error.to_string())
    }

    fn apply(&mut self) -> Result<ApplyReport, Self::Error> {
        let result = self.apply_real()?;
        self.operation_id = Some(result.operation_id.clone());
        self.planned_keys()
            .map(|paths| ApplyReport::new(paths).with_operation(result.operation_id))
            .map_err(|error| error.to_string())
    }

    fn undo(&mut self) -> Result<UndoReport, Self::Error> {
        self.undo_real()
    }
}

fn walk_tree(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut stack = vec![(root.to_path_buf(), PathBuf::new())];
    while let Some((directory, relative)) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let child_path = entry.path();
            let child_relative = relative.join(entry.file_name());
            paths.push(child_relative.clone());
            if fs::symlink_metadata(&child_path)?.is_dir() {
                stack.push((child_path, child_relative));
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn file_mode(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}
