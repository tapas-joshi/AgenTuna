//! TF-048: migration fixtures, snapshots and the round-trip harness.
//!
//! The [`Scenario`] below stands in for the TC-009/TC-010 engine until it lands: it applies a
//! small set of operations with the real `at-fs` primitives and restores from the backups those
//! writes produced. Every assertion is about what the harness observed on disk, not about the
//! scenario's own bookkeeping.

#![cfg(feature = "testkit")]

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_fs::{BackupPolicy, COPY_MANIFEST_FILE, ExpectedState, WriteRequest};
use at_migrate::testkit::{
    ApplyReport, ChangedField, DiffEntry, DryRunReport, MANAGED_SKILL, MigrationFixture,
    MigrationStep, MigrationUnderTest, NATIVE_SKILL, PlanReport, Refusal, RoundTrip, SKILL_FILE,
    ScanReport, UndoReport,
};
use at_model::{AgentId, LinkMode};

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
        let before = fs::read(&paths.project_native_skill)?;
        let deployed_key = fixture.key(&paths.managed_deployment).expect("key");
        let deployed_target = fixture.filter().capture()?.entry(&deployed_key).cloned();
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;

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

        // The replaced project skill holds its original bytes and mode again.
        assert_eq!(fs::read(&paths.project_native_skill)?, before);
        let restored_mode = fs::metadata(&paths.project_native_skill)?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(restored_mode, 0o644, "undo restores the original mode");
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

    let edited = paths.project_native_skill.clone();
    let edited_key = scenario.key(&edited);
    fs::write(&edited, b"edited outside AgenTuna\n")?;

    trip.record(MigrationStep::Undo, || scenario.undo())?;
    trip.ensure_refused(MigrationStep::Undo, &Refusal::Conflict)?;
    trip.ensure_refusal_named(MigrationStep::Undo, &edited_key)?;

    // Nothing was restored: the edit survives and the store item is still in place.
    assert_eq!(fs::read(&edited)?, b"edited outside AgenTuna\n");
    assert!(paths.store_skill.is_file(), "no partial restore");
    Ok(())
}

#[test]
fn undo_after_the_window_refuses_as_expired_and_restores_at_the_edge() -> Result<(), Box<dyn Error>>
{
    for (age, refuses) in [(31, true), (UNDO_WINDOW_DAYS, false)] {
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
            assert_eq!(
                trip.current(),
                &after_apply,
                "an expired undo restores nothing at all"
            );
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
        let original_project_skill = fs::read(&paths.project_native_skill)?;
        let filter = fixture.filter();
        let mut scenario = Scenario::new(fixture)?;
        // Fail on the last operation: the store item, the deployment and the first rewrite
        // (whose backup is the only way back) already exist at that point.
        scenario.fail_at(Some(scenario.ops_len() - 1));

        let mut trip = RoundTrip::begin(filter)?;
        trip.record(MigrationStep::Scan, || scenario.scan())?;
        trip.record(MigrationStep::Plan, || scenario.plan())?;
        trip.record(MigrationStep::DryRun, || scenario.dry_run())?;
        trip.record(MigrationStep::Apply, || scenario.apply())?;

        trip.ensure_failed_apply_rolled_back()?;
        let record = trip.last().expect("apply recorded");
        assert!(
            record.failure().is_some(),
            "the injected failure is recorded"
        );

        // No dangling link or copy, no store item, no created folder.
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
        // The rewrite that did run is back to its original bytes, restored from its backup.
        assert_eq!(
            fs::read(&paths.project_native_skill)?,
            original_project_skill
        );
        let restored_mode = fs::metadata(&paths.project_native_skill)?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(restored_mode, 0o644);
        // The failing operation's own target was never written.
        assert!(paths.project_claude_skill.is_file());
    }
    Ok(())
}

#[test]
fn a_second_undo_changes_nothing() -> Result<(), Box<dyn Error>> {
    let fixture = MigrationFixture::link()?;
    let filter = fixture.filter();
    let mut scenario = Scenario::new(fixture)?;

    let mut trip = at_migrate::testkit::run_round_trip(filter, &mut scenario)?;
    trip.ensure_undo_restored()?;
    let after_first_undo = trip.current().clone();

    trip.record(MigrationStep::Undo, || scenario.undo())?;
    trip.ensure_undo_idempotent()?;
    assert_eq!(trip.current(), &after_first_undo);
    assert_eq!(
        trip.last().expect("second undo").refusal(),
        Some(&Refusal::Other("already undone".to_owned()))
    );
    Ok(())
}

// -------------------------------------------------------------------------------------------
// Scenario driver (stands in for TC-009/TC-010 until they land)
// -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Mkdir,
    Create,
    Replace,
    Deploy,
}

#[derive(Debug, Clone)]
struct Op {
    action: Action,
    path: PathBuf,
    bytes: Vec<u8>,
    source: Option<PathBuf>,
    mode: Option<u32>,
}

#[derive(Debug, Clone)]
struct JournalEntry {
    action: Action,
    path: PathBuf,
    after_hash: Option<String>,
    backup: Option<PathBuf>,
    before_mode: Option<u32>,
}

struct Scenario {
    fixture: MigrationFixture,
    ops: Vec<Op>,
    operation_dir: PathBuf,
    journal: Vec<JournalEntry>,
    fail_at: Option<usize>,
    age_days: u64,
    undone: bool,
}

impl Scenario {
    fn new(fixture: MigrationFixture) -> Result<Self, Box<dyn Error>> {
        let paths = fixture.paths();
        let duplicate = fs::read(&paths.project_native_skill)?;
        let mut canonical = duplicate.clone();
        canonical.extend_from_slice(b"\n<!-- migrated into the AgenTuna store -->\n");

        let store_skill_dir = paths.user_store.join("skills").join(NATIVE_SKILL);
        let store_skill = store_skill_dir.join(SKILL_FILE);
        let managed_source = paths
            .store_skill
            .parent()
            .expect("managed skill folder")
            .to_path_buf();
        let deployment = fixture
            .deployment_target(AgentId::Omp, MANAGED_SKILL)
            .ok_or("the fixture has no omp skills folder")?;
        let operation_dir = paths.support_dir.join("backups").join("op-fixture");
        fs::create_dir_all(&operation_dir)?;

        let ops = vec![
            Op {
                action: Action::Mkdir,
                path: store_skill_dir,
                bytes: Vec::new(),
                source: None,
                mode: None,
            },
            Op {
                action: Action::Create,
                path: store_skill,
                bytes: canonical.clone(),
                source: None,
                mode: Some(0o600),
            },
            Op {
                action: Action::Deploy,
                path: deployment,
                bytes: Vec::new(),
                source: Some(managed_source),
                mode: None,
            },
            Op {
                action: Action::Replace,
                path: paths.project_native_skill.clone(),
                bytes: canonical.clone(),
                source: None,
                mode: Some(0o600),
            },
            Op {
                action: Action::Replace,
                path: paths.project_claude_skill.clone(),
                bytes: canonical,
                source: None,
                mode: Some(0o600),
            },
        ];
        Ok(Self {
            fixture,
            ops,
            operation_dir,
            journal: Vec::new(),
            fail_at: None,
            age_days: 0,
            undone: false,
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

    fn ops_len(&self) -> usize {
        self.ops.len()
    }

    fn fail_at(&mut self, index: Option<usize>) {
        self.fail_at = index;
    }

    fn set_age_days(&mut self, days: u64) {
        self.age_days = days;
    }

    /// Item labels the scenario would find, in the shape a scanner reports.
    fn native_items(&self) -> Vec<String> {
        let paths = self.fixture.paths();
        let mut items: Vec<String> = paths
            .agents
            .iter()
            .map(|agent| format!("skill:{}@{}", agent.skill_name, agent.agent))
            .collect();
        items.push(format!("skill:{NATIVE_SKILL}@project"));
        items.push(format!("path:{}", self.key(&paths.project_mcp)));
        items
    }

    /// Every path apply touches, as snapshot keys.
    fn planned_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for op in &self.ops {
            match op.action {
                Action::Mkdir | Action::Create | Action::Replace => keys.push(self.key(&op.path)),
                Action::Deploy => {
                    keys.push(self.key(&op.path));
                    if self.fixture.mode() == LinkMode::Copy {
                        let source = op.source.as_ref().expect("deploy source");
                        for relative in walk_files(source).expect("read deploy source") {
                            keys.push(self.key(&op.path.join(relative)));
                        }
                        keys.push(self.key(&op.path.join(COPY_MANIFEST_FILE)));
                    }
                }
            }
        }
        keys
    }

    fn apply_ops(&mut self) -> Result<(), String> {
        for index in 0..self.ops.len() {
            if self.fail_at == Some(index) {
                self.rollback()?;
                return Err(format!("injected failure at operation {index}"));
            }
            self.run_op(index)?;
        }
        Ok(())
    }

    fn run_op(&mut self, index: usize) -> Result<(), String> {
        let op = self.ops[index].clone();
        match op.action {
            Action::Mkdir => {
                fs::create_dir_all(&op.path).map_err(|error| error.to_string())?;
                self.journal.push(JournalEntry {
                    action: op.action,
                    path: op.path,
                    after_hash: None,
                    backup: None,
                    before_mode: None,
                });
            }
            Action::Create => {
                at_fs::apply(&WriteRequest::create(&op.path, op.bytes.clone()))
                    .map_err(|error| error.to_string())?;
                if let Some(mode) = op.mode {
                    set_mode(&op.path, mode).map_err(|error| error.to_string())?;
                }
                self.journal.push(JournalEntry {
                    action: op.action,
                    path: op.path,
                    after_hash: Some(at_fs::sha256_hex(&op.bytes)),
                    backup: None,
                    before_mode: None,
                });
            }
            Action::Replace => {
                let current = fs::read(&op.path).map_err(|error| error.to_string())?;
                let before_mode = file_mode(&op.path).map_err(|error| error.to_string())?;
                let mut request = WriteRequest::replace(
                    &op.path,
                    op.bytes.clone(),
                    ExpectedState::Hash(at_fs::sha256_hex(&current)),
                );
                request.backup = BackupPolicy::OperationFolder(self.operation_dir.clone());
                let outcome = at_fs::apply(&request).map_err(|error| error.to_string())?;
                if let Some(mode) = op.mode {
                    set_mode(&op.path, mode).map_err(|error| error.to_string())?;
                }
                self.journal.push(JournalEntry {
                    action: op.action,
                    path: op.path,
                    after_hash: Some(at_fs::sha256_hex(&op.bytes)),
                    backup: outcome.backup_path,
                    before_mode: Some(before_mode),
                });
            }
            Action::Deploy => {
                let source = op.source.as_ref().ok_or("deploy op without a source")?;
                at_fs::deploy(&at_fs::DeployRequest::new(
                    source,
                    &op.path,
                    self.fixture.mode(),
                ))
                .map_err(|error| error.to_string())?;
                self.journal.push(JournalEntry {
                    action: op.action,
                    path: op.path,
                    after_hash: None,
                    backup: None,
                    before_mode: None,
                });
            }
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), String> {
        let entries = std::mem::take(&mut self.journal);
        for entry in entries.into_iter().rev() {
            self.restore(entry)?;
        }
        Ok(())
    }

    fn restore(&self, entry: JournalEntry) -> Result<(), String> {
        let result = match entry.action {
            Action::Mkdir => match fs::remove_dir_all(&entry.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
            Action::Create => match fs::remove_file(&entry.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
            Action::Replace => {
                let backup = entry.backup.as_ref().ok_or("replace without a backup")?;
                let bytes = fs::read(backup).map_err(|error| error.to_string())?;
                let current = fs::read(&entry.path).map_err(|error| error.to_string())?;
                let request = WriteRequest::replace(
                    &entry.path,
                    bytes,
                    ExpectedState::Hash(at_fs::sha256_hex(&current)),
                );
                at_fs::apply(&request).map_err(|error| error.to_string())?;
                if let Some(mode) = entry.before_mode {
                    set_mode(&entry.path, mode).map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            Action::Deploy => match fs::symlink_metadata(&entry.path) {
                Ok(meta) if meta.file_type().is_symlink() => fs::remove_file(&entry.path),
                Ok(meta) if meta.is_dir() => fs::remove_dir_all(&entry.path),
                Ok(_) => fs::remove_file(&entry.path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        };
        result.map_err(|error| error.to_string())
    }

    fn undo(&mut self) -> Result<UndoReport, String> {
        if self.undone {
            return Ok(UndoReport::refused(
                Refusal::Other("already undone".to_owned()),
                Vec::new(),
            ));
        }
        let affected: Vec<String> = self
            .journal
            .iter()
            .map(|entry| self.key(&entry.path))
            .collect();
        if self.age_days > UNDO_WINDOW_DAYS {
            return Ok(UndoReport::refused(Refusal::Expired, affected));
        }
        let conflicted: Vec<String> = self
            .journal
            .iter()
            .filter(|entry| {
                entry.after_hash.as_ref().is_some_and(|expected| {
                    fs::read(&entry.path)
                        .ok()
                        .map(|bytes| at_fs::sha256_hex(&bytes))
                        .as_deref()
                        != Some(expected.as_str())
                })
            })
            .map(|entry| self.key(&entry.path))
            .collect();
        if !conflicted.is_empty() {
            return Ok(UndoReport::refused(Refusal::Conflict, conflicted));
        }
        let entries = std::mem::take(&mut self.journal);
        for entry in entries.into_iter().rev() {
            self.restore(entry)?;
        }
        self.undone = true;
        Ok(UndoReport::restored(affected))
    }
}

impl MigrationUnderTest for Scenario {
    type Error = String;

    fn scan(&mut self) -> Result<ScanReport, Self::Error> {
        Ok(ScanReport::new(self.native_items()))
    }

    fn plan(&mut self) -> Result<PlanReport, Self::Error> {
        Ok(PlanReport::new(self.planned_keys()))
    }

    fn dry_run(&mut self) -> Result<DryRunReport, Self::Error> {
        Ok(DryRunReport::new(self.planned_keys()))
    }

    fn apply(&mut self) -> Result<ApplyReport, Self::Error> {
        self.apply_ops()?;
        Ok(ApplyReport::new(self.planned_keys()).with_operation("op-fixture"))
    }

    fn undo(&mut self) -> Result<UndoReport, Self::Error> {
        Scenario::undo(self)
    }
}

fn walk_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), PathBuf::new())];
    while let Some((dir, relative)) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = relative.join(entry.file_name());
            if fs::symlink_metadata(&path)?.is_dir() {
                stack.push((path, relative));
            } else {
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn file_mode(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}
