//! Fault-injection tests for migration apply (TC-036).
//!
//! Each scenario runs on a temporary fixture. Faults fire at the real failpoint sites in at-fs,
//! and every outcome is checked against a tree snapshot taken before the apply. Nothing here
//! touches the real HOME, the Keychain, the network, or an agent configuration.
//!
//! The change-set writer keeps its journals in `<store>/.state/journal`, which
//! `layout::JOURNALS_DIR` names, so these tests scan that directory.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::Database;
use at_fs::backup::{BackupSidecar, verify as verify_backup};
use at_fs::failpoint::{FailInjector, Fault, FaultPoint, with_injector};
use at_fs::recovery::{
    JournalEntry, PendingJournal, RecoveryCallbacks, RecoveryPolicy, recover_journals,
};
use at_fs::{ApplyRoots, COPY_MANIFEST_FILE, sha256_hex};
use at_migrate::apply::{
    ApplyPlanContext, ApplyPlanError, MigrationApplyOutcome, MigrationWarning, apply_plan,
};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, ConflictResolution, MigrationAction, MigrationPlan, MigrationStrategy,
    PlanError, PlanStep, build_plan,
};
use at_migrate::scan::{ScanReport, ScanRequest, scan};
use at_migrate::testkit::{MigrationFixture, SnapshotFilter, TreeSnapshot};
use at_model::{AgentId, ItemKind, Scope};
use at_store::layout;

/// `EPERM`: a link the platform refuses falls back to a copy, and the copy is reported.
const EPERM: i32 = 1;
/// `EIO`: the failing step aborts and the apply rolls back.
const EIO: i32 = 5;
/// `ENOSPC`: a full disk.
const ENOSPC: i32 = 28;

/// Builds a fresh scenario, so each fault starts from an identical tree.
type Build = fn() -> Result<Scenario, Box<dyn Error>>;

/// A migration fixture, its plan, the authorized roots, and the backup root.
struct Scenario {
    fixture: MigrationFixture,
    plan: MigrationPlan,
    roots: ApplyRoots,
    backup_root: PathBuf,
}

impl Scenario {
    /// Replace-with-links plan for the project skill that Codex and omp both read.
    fn replace_with_links() -> Result<Self, Box<dyn Error>> {
        let fixture = MigrationFixture::link()?;
        let plan = replace_plan(&fixture)?;
        Self::assemble(fixture, plan)
    }

    /// Leave-and-watch plan for the fixture's conflicting MCP server and one rule.
    fn leave_and_watch() -> Result<Self, Box<dyn Error>> {
        let fixture = MigrationFixture::link()?;
        let plan = leave_plan(&fixture)?;
        Self::assemble(fixture, plan)
    }

    fn assemble(fixture: MigrationFixture, plan: MigrationPlan) -> Result<Self, Box<dyn Error>> {
        let support_dir = fixture.paths().support_dir.clone();
        fs::create_dir_all(&support_dir)?;
        let roots = ApplyRoots::new([fixture.paths().home.clone()])?;
        Ok(Self {
            fixture,
            plan,
            roots,
            backup_root: support_dir.join(layout::BACKUPS_DIR),
        })
    }

    fn database(&self) -> Result<Database, Box<dyn Error>> {
        Ok(Database::open_file(&self.fixture.paths().db_path)?)
    }

    fn apply(&self, database: &mut Database) -> Result<MigrationApplyOutcome, ApplyPlanError> {
        let paths = self.fixture.paths();
        let mut context = ApplyPlanContext {
            store_root: &paths.user_store,
            backup_root: &self.backup_root,
            authorized_roots: &self.roots,
            database,
            project_id: None,
        };
        apply_plan(&self.plan, &mut context)
    }

    fn freeze(&self) -> Result<Frozen, Box<dyn Error>> {
        let paths = self.fixture.paths();
        let mut bookkeeping = self.fixture.filter().bookkeeping().to_vec();
        // at-db holds `agentuna.unclean` while a database handle is open; not user state.
        bookkeeping.push(format!("{}/agentuna.unclean", paths.support_key));
        let filter = self.fixture.filter().with_bookkeeping(bookkeeping);
        let before = filter.capture()?;
        Ok(Frozen { filter, before })
    }

    /// Directory where the change-set writer keeps its journals.
    fn journal_dir(&self) -> PathBuf {
        self.fixture
            .paths()
            .user_store
            .join(layout::STATE_DIR)
            .join("journal")
    }
}

/// A tree snapshot taken before an apply, with the filter that produced it.
struct Frozen {
    filter: SnapshotFilter,
    before: TreeSnapshot,
}

impl Frozen {
    /// Fails unless the tree matches the snapshot outside store bookkeeping.
    fn assert_unchanged(&self, context: &str) -> Result<(), Box<dyn Error>> {
        let after = self.filter.capture()?;
        self.before
            .diff(&after)
            .excluding(self.filter.bookkeeping())
            .ensure_empty(context)?;
        Ok(())
    }
}

/// Environment, adapters, and a full scan of the fixture.
fn scan_fixture(fixture: &MigrationFixture) -> Result<ScanReport, Box<dyn Error>> {
    let paths = fixture.paths();
    let environment = AgentEnvironment {
        home: paths.home.clone(),
        project: Some(paths.project.clone()),
        variables: Default::default(),
    };
    let adapters = AgentAdapterRegistry::register();
    let stores = [paths.user_store.clone(), paths.project_store.clone()];
    Ok(scan(ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &stores,
        store_items: &[],
    })?)
}

/// Replace-with-links plan for the project skill shared by Codex and omp.
fn replace_plan(fixture: &MigrationFixture) -> Result<MigrationPlan, Box<dyn Error>> {
    let paths = fixture.paths();
    let scanned = scan_fixture(fixture)?;
    let shared_folder = paths
        .project_native_skill
        .parent()
        .ok_or("the project skill has no folder")?;
    let shared = scanned
        .items
        .iter()
        .find(|item| item.path == shared_folder)
        .ok_or("the shared project skill was not discovered")?
        .clone();
    if shared.agents != BTreeSet::from([AgentId::Codex, AgentId::Omp]) {
        return Err("the project skill must be shared by Codex and omp".into());
    }
    let single = ScanReport {
        roots: Vec::new(),
        items: vec![shared.clone()],
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&single);
    if !conflicts.conflicts.is_empty() {
        return Err("the shared skill must not conflict".into());
    }
    let selected = BTreeSet::from([shared.path.clone()]);
    Ok(build_plan(BuildPlanRequest {
        scan: &single,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected,
        store_root: paths.user_store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::ReplaceWithLinks,
        scope: Scope::User,
        backup_root: paths.support_dir.join(layout::BACKUPS_DIR),
    })?)
}

/// Leave-and-watch plan for the fixture's conflicting MCP server and one rule.
fn leave_plan(fixture: &MigrationFixture) -> Result<MigrationPlan, Box<dyn Error>> {
    let paths = fixture.paths();
    let scanned = scan_fixture(fixture)?;
    let conflicts = detect_conflicts(&scanned);
    let selected_mcp = conflicts
        .conflicts
        .iter()
        .find(|conflict| conflict.kind == ItemKind::McpServer)
        .and_then(|conflict| conflict.copies.first())
        .map(|copy| copy.path.clone())
        .ok_or("the fixture has no conflicting MCP server")?;
    let selected_rule = conflicts
        .conflicts
        .iter()
        .find(|conflict| conflict.kind == ItemKind::Rule)
        .and_then(|conflict| conflict.copies.first())
        .map(|copy| copy.path.clone())
        .or_else(|| {
            scanned
                .items
                .iter()
                .find(|item| item.kind == ItemKind::Rule)
                .map(|item| item.path.clone())
        })
        .ok_or("the fixture has no rule")?;
    let selected = BTreeSet::from([selected_mcp, selected_rule]);
    let resolutions = conflicts
        .conflicts
        .iter()
        .map(|conflict| (conflict.name.clone(), ConflictResolution::KeepA))
        .collect::<BTreeMap<_, _>>();
    Ok(build_plan(BuildPlanRequest {
        scan: &scanned,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected,
        store_root: paths.user_store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy: MigrationStrategy::LeaveAndWatch,
        scope: Scope::User,
        backup_root: paths.support_dir.join(layout::BACKUPS_DIR),
    })?)
}

/// The first step of `action` in the plan.
fn step_of<'a>(
    plan: &'a MigrationPlan,
    action: &MigrationAction,
) -> Result<&'a PlanStep, Box<dyn Error>> {
    plan.steps
        .iter()
        .find(|step| step.action == *action)
        .ok_or_else(|| format!("the plan has no {action:?} step").into())
}

/// The destination of the first `action` step.
fn destination_of(
    plan: &MigrationPlan,
    action: &MigrationAction,
) -> Result<PathBuf, Box<dyn Error>> {
    step_of(plan, action)?
        .destination
        .clone()
        .ok_or_else(|| format!("{action:?} has no destination").into())
}

/// The source of the first `action` step.
fn source_of(plan: &MigrationPlan, action: &MigrationAction) -> Result<PathBuf, Box<dyn Error>> {
    step_of(plan, action)?
        .source
        .clone()
        .ok_or_else(|| format!("{action:?} has no source").into())
}

/// Recomputes `plan_hash` after a test edits steps, so the edit reaches the later checks.
fn seal(plan: &mut MigrationPlan) -> Result<(), Box<dyn Error>> {
    plan.plan_hash = sha256_hex(&serde_json::to_vec(&(plan.strategy, &plan.steps))?);
    Ok(())
}

/// Joins an error with its sources so assertions can match any layer.
fn chain(error: &dyn Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(next) = source {
        parts.push(next.to_string());
        source = next.source();
    }
    parts.join(" | ")
}

/// Files with `extension` under `root`, recursively. Empty when `root` does not exist.
fn files_with_extension(root: &Path, extension: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    if !root.exists() {
        return Ok(found);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            found.extend(files_with_extension(&path, extension)?);
        } else if path.extension().is_some_and(|value| value == extension) {
            found.push(path);
        }
    }
    Ok(found)
}

/// Verifies every backup sidecar under `root` against its blob. Returns how many passed.
///
/// A backed-up `.json` original is a blob named `<name>.json`, and its own sidecar is
/// `<name>.json.json`. A file with a sidecar beside it is therefore a blob, not a sidecar.
fn verify_backups(root: &Path) -> Result<usize, Box<dyn Error>> {
    let mut verified = 0;
    for path in files_with_extension(root, "json")? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.with_file_name(format!("{name}.json")).exists() {
            continue;
        }
        let sidecar: BackupSidecar = serde_json::from_slice(&fs::read(&path)?)?;
        verify_backup(&path.with_extension(""), &sidecar.content_hash)?;
        verified += 1;
    }
    Ok(verified)
}

/// Counts crossings of one failpoint and never faults.
struct Counter {
    point: FaultPoint,
    seen: AtomicUsize,
}

impl FailInjector for Counter {
    fn fault(&self, point: FaultPoint, _path: &Path) -> Option<Fault> {
        if point == self.point {
            self.seen.fetch_add(1, Ordering::SeqCst);
        }
        None
    }
}

/// What an armed failpoint does when its target crossing is reached.
#[derive(Clone, Copy)]
enum Failure {
    /// Fail the step with this OS error code.
    Errno(i32),
    /// Unwind out of the apply, as a process that dies mid-migration leaves it.
    Crash,
}

/// Arms one crossing of a failpoint. Crossings count only when they match `point` and, if set,
/// end with `suffix`. The `nth` matching crossing is the one that fails.
struct FaultAt {
    point: FaultPoint,
    suffix: Option<PathBuf>,
    nth: usize,
    failure: Failure,
    seen: AtomicUsize,
    triggered: AtomicBool,
}

impl FaultAt {
    fn new(point: FaultPoint, suffix: Option<PathBuf>, nth: usize, failure: Failure) -> Arc<Self> {
        Arc::new(Self {
            point,
            suffix,
            nth,
            failure,
            seen: AtomicUsize::new(0),
            triggered: AtomicBool::new(false),
        })
    }

    fn triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }
}

impl FailInjector for FaultAt {
    fn fault(&self, point: FaultPoint, path: &Path) -> Option<Fault> {
        if point != self.point {
            return None;
        }
        if self
            .suffix
            .as_ref()
            .is_some_and(|suffix| !path.ends_with(suffix))
        {
            return None;
        }
        if self.seen.fetch_add(1, Ordering::SeqCst) != self.nth {
            return None;
        }
        self.triggered.store(true, Ordering::SeqCst);
        match self.failure {
            Failure::Errno(code) => Some(Fault::Fail(code)),
            Failure::Crash => std::panic::resume_unwind(Box::new("simulated crash during apply")),
        }
    }
}

/// Recovery seam for the tests. It reads original bytes from the backups table first and from
/// sidecars under the backup root second, as the app does. Finish calls are recorded in memory.
struct TestRecovery {
    db_path: PathBuf,
    backup_root: PathBuf,
    finished: Vec<(String, bool, bool)>,
}

impl TestRecovery {
    /// Backup path recorded in the backups table for this operation and original path, if any.
    fn backup_row_path(
        &self,
        operation_id: &str,
        path: &Path,
        expected: &str,
    ) -> Result<Option<PathBuf>, String> {
        let database = Database::open_file(&self.db_path).map_err(|error| error.to_string())?;
        let rows = at_db::backups::backups_for_operation(database.connection(), operation_id)
            .map_err(|error| error.to_string())?;
        let original = path.display().to_string();
        Ok(rows
            .into_iter()
            .filter(|row| row.original_path.as_deref() == Some(original.as_str()))
            .filter(|row| row.content_hash.as_deref() == Some(expected))
            .find_map(|row| row.backup_path)
            .map(PathBuf::from))
    }

    /// Original bytes recorded in the backups table for this operation and path, if any.
    fn backup_row_bytes(
        &self,
        operation_id: &str,
        path: &Path,
        expected: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(backup_path) = self.backup_row_path(operation_id, path, expected)? else {
            return Ok(None);
        };
        let bytes = fs::read(&backup_path).map_err(|error| error.to_string())?;
        if sha256_hex(&bytes) == expected {
            Ok(Some(bytes))
        } else {
            Err(format!(
                "backup {} does not match {expected}",
                backup_path.display()
            ))
        }
    }

    /// Original bytes from the first sidecar under the backup root that matches `path`.
    fn sidecar_bytes(&self, path: &Path, expected: &str) -> Result<Vec<u8>, String> {
        let sidecars =
            files_with_extension(&self.backup_root, "json").map_err(|error| error.to_string())?;
        for sidecar_path in sidecars {
            let Ok(raw) = fs::read(&sidecar_path) else {
                continue;
            };
            let Ok(sidecar) = serde_json::from_slice::<BackupSidecar>(&raw) else {
                continue;
            };
            if sidecar.original_path.as_path() != path || sidecar.content_hash != expected {
                continue;
            }
            let blob = sidecar_path.with_extension("");
            verify_backup(&blob, expected).map_err(|error| error.to_string())?;
            return fs::read(&blob).map_err(|error| error.to_string());
        }
        Err(format!(
            "no backup holds the original bytes of {}",
            path.display()
        ))
    }
}

impl RecoveryCallbacks for TestRecovery {
    fn before_bytes(
        &mut self,
        journal: &PendingJournal,
        _op_index: usize,
        entry: &JournalEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(expected) = entry.before_hash.as_deref() else {
            return Ok(None);
        };
        if let Some(bytes) = self.backup_row_bytes(&journal.operation_id, &entry.path, expected)? {
            return Ok(Some(bytes));
        }
        self.sidecar_bytes(&entry.path, expected).map(Some)
    }

    fn delete_backup(
        &mut self,
        journal: &PendingJournal,
        _op_index: usize,
        entry: &JournalEntry,
    ) -> Result<Option<PathBuf>, String> {
        let Some(expected) = entry.before_hash.as_deref() else {
            return Ok(None);
        };
        self.backup_row_path(&journal.operation_id, &entry.path, expected)
    }

    fn finish(
        &mut self,
        operation_id: &str,
        succeeded: bool,
        manual_review: bool,
    ) -> Result<(), String> {
        self.finished
            .push((operation_id.to_owned(), succeeded, manual_review));
        Ok(())
    }
}

/// Faults crossing `nth` of `point` (for paths ending in `suffix`) with `errno`. Then checks that
/// the apply fails with that cause, the tree is unchanged, and no journal is left behind.
fn assert_fault_aborts(
    scenario: &Scenario,
    point: FaultPoint,
    suffix: Option<PathBuf>,
    nth: usize,
    errno: i32,
    context: &str,
) -> Result<(), Box<dyn Error>> {
    let frozen = scenario.freeze()?;
    let mut database = scenario.database()?;
    let injector = FaultAt::new(point, suffix, nth, Failure::Errno(errno));
    let result = with_injector(injector.clone(), || scenario.apply(&mut database));
    let Err(error) = result else {
        return Err(format!("{context}: the apply succeeded despite the fault").into());
    };
    assert!(
        injector.triggered(),
        "{context}: the fault point was never reached"
    );
    let text = chain(&error);
    assert!(
        text.contains(&format!("os error {errno}"))
            || (errno == ENOSPC && text.contains("the disk is full")),
        "{context}: the cause is missing from `{text}`"
    );
    frozen.assert_unchanged(context)?;
    assert!(
        files_with_extension(&scenario.journal_dir(), "journal")?.is_empty(),
        "{context}: a journal was left behind"
    );
    verify_backups(&scenario.backup_root)?;
    Ok(())
}

/// Change-set operations in a fresh scenario, counted without faulting.
fn count_operations(build: Build) -> Result<usize, Box<dyn Error>> {
    let scenario = build()?;
    let mut database = scenario.database()?;
    let counter = Arc::new(Counter {
        point: FaultPoint::ChangeSetOperation,
        seen: AtomicUsize::new(0),
    });
    with_injector(counter.clone(), || scenario.apply(&mut database))?;
    let total = counter.seen.load(Ordering::SeqCst);
    assert!(total >= 2, "expected at least two operations, saw {total}");
    Ok(total)
}

/// Faults operation `index` with EIO, checks the rollback, then checks that a retry completes.
fn fault_then_retry(build: Build, index: usize) -> Result<(), Box<dyn Error>> {
    let scenario = build()?;
    let context = format!("operation {index}");
    assert_fault_aborts(
        &scenario,
        FaultPoint::ChangeSetOperation,
        None,
        index,
        EIO,
        &context,
    )?;
    let mut database = scenario.database()?;
    let outcome = scenario.apply(&mut database)?;
    assert!(
        outcome.completed > 0,
        "{context}: the retry completed nothing"
    );
    assert!(
        !outcome.touched_paths.is_empty(),
        "{context}: the retry touched nothing"
    );
    Ok(())
}

/// Crashes the apply at operation `index`, restarts with recovery, and checks the pre-apply tree.
fn crash_then_recover(build: Build, index: usize) -> Result<(), Box<dyn Error>> {
    let scenario = build()?;
    let frozen = scenario.freeze()?;
    let mut database = scenario.database()?;
    let injector = FaultAt::new(FaultPoint::ChangeSetOperation, None, index, Failure::Crash);
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        with_injector(injector.clone(), || scenario.apply(&mut database))
    }));
    assert!(crashed.is_err(), "operation {index} did not crash");
    assert!(injector.triggered(), "operation {index} was never reached");
    // The process died, so its database handle goes with it. Recovery reopens the file.
    drop(database);

    let journal_dir = scenario.journal_dir();
    assert!(
        !files_with_extension(&journal_dir, "journal")?.is_empty(),
        "operation {index} left no journal to recover"
    );
    let paths = scenario.fixture.paths();
    let mut recovery = TestRecovery {
        db_path: paths.db_path.clone(),
        backup_root: scenario.backup_root.clone(),
        finished: Vec::new(),
    };
    let report = recover_journals(
        &journal_dir,
        &RecoveryPolicy::new(
            &paths.user_store,
            [paths.home.clone()],
            [scenario.backup_root.clone()],
        ),
        &mut recovery,
    )?;
    let reasons: Vec<String> = report
        .manual_review
        .iter()
        .map(|review| review.reason.to_string())
        .collect();
    assert_eq!(
        report.rolled_back.len(),
        1,
        "operation {index}: recovery must roll the journal back; manual review: {reasons:?}"
    );
    assert!(
        report.manual_review.is_empty(),
        "operation {index}: recovery asked for manual review"
    );
    frozen.assert_unchanged(&format!("recovery after a crash at operation {index}"))?;
    assert!(
        files_with_extension(&journal_dir, "journal")?.is_empty(),
        "operation {index}: a journal survived recovery"
    );
    assert!(
        recovery.finished.iter().all(|(_, succeeded, _)| !succeeded),
        "operation {index}: recovery reported success for a rolled-back operation"
    );
    verify_backups(&scenario.backup_root)?;
    Ok(())
}

#[test]
fn backup_copy_enospc_aborts_replacement_without_mutation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::replace_with_links()?;
    assert_fault_aborts(
        &scenario,
        FaultPoint::BackupWrite,
        None,
        0,
        ENOSPC,
        "backup copy ENOSPC",
    )
}

#[test]
fn symlink_eio_aborts_replacement_without_mutation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::replace_with_links()?;
    assert_fault_aborts(
        &scenario,
        FaultPoint::DeploySymlink,
        None,
        0,
        EIO,
        "symlink EIO",
    )
}

#[test]
fn symlink_eperm_falls_back_to_copy_with_warning() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::replace_with_links()?;
    let paths = scenario.fixture.paths();
    let skill_file = paths.project_native_skill.clone();
    let shared = skill_file
        .parent()
        .ok_or("the native skill has no folder")?
        .to_path_buf();
    let file_name = skill_file
        .file_name()
        .ok_or("the native skill has no file name")?;
    let original = fs::read(&skill_file)?;
    let mut database = scenario.database()?;
    let injector = FaultAt::new(FaultPoint::DeploySymlink, None, 0, Failure::Errno(EPERM));
    let outcome = with_injector(injector.clone(), || scenario.apply(&mut database))?;

    assert!(injector.triggered(), "the symlink was never attempted");
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| matches!(warning, MigrationWarning::CopyFallback { .. })),
        "a link refused with EPERM must be reported as a copy fallback"
    );
    let metadata = fs::symlink_metadata(&shared)?;
    assert!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "after the fallback the shared skill must be a real folder"
    );
    assert_eq!(
        fs::read(shared.join(file_name))?,
        original,
        "the copy must carry the original bytes"
    );
    assert!(
        shared.join(COPY_MANIFEST_FILE).is_file(),
        "the copy fallback must write its manifest"
    );
    Ok(())
}

#[test]
fn manifest_write_enospc_aborts_replacement_without_mutation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::replace_with_links()?;
    assert_fault_aborts(
        &scenario,
        FaultPoint::BeforeRename,
        Some(PathBuf::from(layout::MANIFEST_FILE)),
        0,
        ENOSPC,
        "store manifest ENOSPC",
    )
}

#[test]
fn store_rule_write_eio_aborts_leave_and_watch_without_mutation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::leave_and_watch()?;
    let rule = destination_of(&scenario.plan, &MigrationAction::RenderRule)?;
    assert_fault_aborts(
        &scenario,
        FaultPoint::BeforeRename,
        Some(rule),
        0,
        EIO,
        "rule write EIO",
    )
}

#[test]
fn store_mcp_write_enospc_aborts_leave_and_watch_without_mutation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::leave_and_watch()?;
    let mcp = destination_of(&scenario.plan, &MigrationAction::RenderMcp)?;
    assert_fault_aborts(
        &scenario,
        FaultPoint::BeforeRename,
        Some(mcp),
        0,
        ENOSPC,
        "MCP write ENOSPC",
    )
}

#[test]
fn mcp_source_changed_after_review_is_refused_before_any_write() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::leave_and_watch()?;
    let source = source_of(&scenario.plan, &MigrationAction::RenderMcp)?;
    fs::write(&source, b"{ not json")?;
    let frozen = scenario.freeze()?;
    let mut database = scenario.database()?;
    let Err(error) = scenario.apply(&mut database) else {
        return Err("an MCP source changed after review must be refused".into());
    };
    assert!(
        matches!(error, ApplyPlanError::Plan(PlanError::StalePlan)),
        "expected a stale plan, got: {}",
        chain(&error)
    );
    frozen.assert_unchanged("refused changed MCP source")?;
    Ok(())
}

#[test]
fn mcp_parse_failure_with_forged_hashes_is_refused_before_any_write() -> Result<(), Box<dyn Error>>
{
    let mut scenario = Scenario::leave_and_watch()?;
    let source = source_of(&scenario.plan, &MigrationAction::RenderMcp)?;
    let tampered = b"{ not json".to_vec();
    fs::write(&source, &tampered)?;
    // Every step that validates the tampered file must carry its forged hash, or the plan
    // validator refuses as stale before the projection parse runs.
    for step in &mut scenario.plan.steps {
        if matches!(
            step.action,
            MigrationAction::RenderMcp | MigrationAction::RecordWatched
        ) && step.source.as_ref() == Some(&source)
        {
            step.expected_source_hash = Some(sha256_hex(&tampered));
        }
    }
    seal(&mut scenario.plan)?;
    let frozen = scenario.freeze()?;
    let mut database = scenario.database()?;
    let Err(error) = scenario.apply(&mut database) else {
        return Err("an unparseable MCP source must be refused".into());
    };
    let ApplyPlanError::Store(message) = &error else {
        return Err(format!("expected a parse refusal, got: {}", chain(&error)).into());
    };
    assert!(
        message.contains("cannot parse"),
        "the parse refusal is missing: {message}"
    );
    frozen.assert_unchanged("refused unparseable MCP source")?;
    Ok(())
}

#[test]
fn replace_fault_at_each_operation_rolls_back_and_retry_succeeds() -> Result<(), Box<dyn Error>> {
    let total = count_operations(Scenario::replace_with_links)?;
    for index in 0..total {
        fault_then_retry(Scenario::replace_with_links, index)?;
    }
    Ok(())
}

#[test]
fn leave_fault_at_each_operation_rolls_back_and_retry_succeeds() -> Result<(), Box<dyn Error>> {
    let total = count_operations(Scenario::leave_and_watch)?;
    for index in 0..total {
        fault_then_retry(Scenario::leave_and_watch, index)?;
    }
    Ok(())
}

#[test]
fn replace_crash_at_each_operation_recovers_pre_migration_tree() -> Result<(), Box<dyn Error>> {
    let total = count_operations(Scenario::replace_with_links)?;
    for index in 0..total {
        crash_then_recover(Scenario::replace_with_links, index)?;
    }
    Ok(())
}

#[test]
fn leave_crash_at_each_operation_recovers_pre_migration_tree() -> Result<(), Box<dyn Error>> {
    let total = count_operations(Scenario::leave_and_watch)?;
    for index in 0..total {
        crash_then_recover(Scenario::leave_and_watch, index)?;
    }
    Ok(())
}

#[test]
fn failed_apply_keeps_backups_and_reports_the_failing_cause() -> Result<(), Box<dyn Error>> {
    let total = count_operations(Scenario::replace_with_links)?;
    let scenario = Scenario::replace_with_links()?;
    let mut database = scenario.database()?;
    let injector = FaultAt::new(
        FaultPoint::ChangeSetOperation,
        None,
        total - 1,
        Failure::Errno(EIO),
    );
    let result = with_injector(injector.clone(), || scenario.apply(&mut database));
    let Err(error) = result else {
        return Err("the final operation must fail".into());
    };
    assert!(
        injector.triggered(),
        "the final operation was never reached"
    );
    assert!(
        chain(&error).contains("os error 5"),
        "the cause is missing: {}",
        chain(&error)
    );
    let rows = at_db::backups::backups_between(database.connection(), None, None)?;
    assert!(!rows.is_empty(), "the failed apply recorded no backups");
    for row in &rows {
        let blob = row.backup_path.as_deref().map(Path::new);
        let kept = blob.is_some_and(Path::is_file);
        let original = row.original_path.as_deref().map(Path::new);
        let restored = original.is_some_and(Path::exists);
        assert!(
            kept || restored,
            "backup {} lost both its blob and its original",
            row.id
        );
        if let (Some(blob), Some(hash), true) = (blob, row.content_hash.as_deref(), kept) {
            assert_eq!(
                sha256_hex(&fs::read(blob)?),
                hash,
                "backup {} no longer holds the recorded content",
                row.id
            );
        }
    }
    Ok(())
}

#[test]
fn metadata_failure_rolls_back_files_and_closes_the_operation() -> Result<(), Box<dyn Error>> {
    let scenario = Scenario::replace_with_links()?;
    let frozen = scenario.freeze()?;
    let mut database = scenario.database()?;
    database.connection().execute_batch(
        "CREATE TRIGGER refuse_item_insert BEFORE INSERT ON items \
         BEGIN SELECT RAISE(ABORT, 'refused by the test'); END;",
    )?;

    let Err(error) = scenario.apply(&mut database) else {
        return Err("the metadata commit must fail".into());
    };

    let ApplyPlanError::Metadata(message) = &error else {
        return Err(format!("expected a metadata failure, got: {}", chain(&error)).into());
    };
    assert!(
        message.contains("refused by the test"),
        "the database refusal is the cause: {message}"
    );
    frozen.assert_unchanged("metadata rollback")?;
    let operations: Vec<(String, Option<String>, Option<String>)> = {
        let mut statement = database
            .connection()
            .prepare("SELECT id, status, error_kind FROM operations ORDER BY id")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    assert!(!operations.is_empty(), "the applied migration is recorded");
    for (id, status, error_kind) in &operations {
        assert_eq!(status.as_deref(), Some("failed"), "operation {id}");
        assert_eq!(error_kind.as_deref(), Some("rolled_back"), "operation {id}");
    }
    let rows: Vec<(Option<String>, Option<String>)> = {
        let mut statement = database
            .connection()
            .prepare("SELECT before_hash, after_hash FROM operation_files")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    assert!(
        !rows.is_empty(),
        "the migration recorded the files it wrote"
    );
    for (before, after) in rows {
        assert_eq!(before, after, "each row describes the restored state");
    }
    let paths = scenario.fixture.paths();
    for (id, _, _) in operations {
        let undo = at_migrate::undo::undo_operation(
            at_migrate::undo::UndoOperationRequest {
                connection: database.connection(),
                store_root: &paths.user_store,
                backup_root: &scenario.backup_root,
                authorized_roots: &scenario.roots,
                actor: at_db::ops::OperationActor::Migration,
                force: false,
            },
            &id,
        )?;
        assert_eq!(
            undo,
            at_migrate::undo::UndoOutcome::NotUndoable,
            "undo refuses rolled-back operation {id}"
        );
    }
    Ok(())
}
