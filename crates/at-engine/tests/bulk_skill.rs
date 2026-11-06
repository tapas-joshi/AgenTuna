//! TS-015: one reviewed batch applies atomically, and a mid-batch fault restores the pre-state.
//!
//! The fault is injected at the second ChangeSet operation, which is exactly the "permission
//! failure on the second link" case: the first write is already on disk when the second fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use at_engine::bulk_skill::{
    BulkApplyError, BulkIntent, BulkItemPlan, BulkKind, BulkProgressStage, apply_bulk, plan_bulk,
};
use at_fs::failpoint::{FailInjector, Fault, FaultPoint, with_injector};
use at_model::{AgentId, ChangeSet, ExpectedState, FileOp, ItemId, Scope};

/// Fails the n-th `ChangeSetOperation` crossing with `EACCES`; every other crossing proceeds.
struct FailNthOperation {
    target: usize,
    seen: AtomicUsize,
}

impl FailInjector for FailNthOperation {
    fn fault(&self, point: FaultPoint, _path: &Path) -> Option<Fault> {
        if point != FaultPoint::ChangeSetOperation {
            return None;
        }
        let crossing = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        (crossing == self.target).then_some(Fault::Fail(libc_eacces()))
    }
}

const fn libc_eacces() -> i32 {
    13
}

fn write(path: PathBuf) -> FileOp {
    FileOp::Write {
        path,
        bytes: b"deployed".to_vec(),
        expected: ExpectedState::absent(),
        follow_symlink: false,
        backup: true,
    }
}

fn intent(items: Vec<ItemId>) -> BulkIntent {
    BulkIntent {
        kind: BulkKind::Enable,
        items,
        targets: vec![AgentId::Codex, AgentId::ClaudeCode],
        scope: Scope::User,
        project_id: None,
    }
}

/// Sorted relative-path/content digest of a directory tree.
///
/// `.state/` is at-fs bookkeeping (journal lock and staging), not deployed content, so it is
/// excluded exactly like SQLite `-shm`/`-wal` sidecars in snapshot comparisons.
fn tree_hash(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, current: &Path, into: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".state") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, into);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                into.insert(relative, at_fs::sha256_hex(&bytes));
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

fn batch(root: &Path, database: &at_db::Database) -> (BulkPlanFixture, at_fs::ApplyRoots) {
    let items = [ItemId::new(), ItemId::new(), ItemId::new()];
    let plans = items
        .iter()
        .enumerate()
        .map(|(index, item)| BulkItemPlan {
            item_id: *item,
            ops: vec![
                write(root.join(format!("{index}/codex"))),
                write(root.join(format!("{index}/claude"))),
            ],
            refusal: None,
        })
        .collect();
    let plan = plan_bulk(&intent(items.to_vec()), plans).expect("batch plan");
    let _ = database;
    (
        BulkPlanFixture { plan },
        at_fs::ApplyRoots::new([root.to_path_buf()]).expect("authorized root"),
    )
}

struct BulkPlanFixture {
    plan: at_engine::bulk_skill::BulkPlan,
}

#[test]
fn second_link_permission_failure_rolls_back_the_first_and_restores_the_tree_hash() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().canonicalize().expect("canonical root");
    std::fs::write(root.join("sentinel.txt"), b"keep me").expect("sentinel");
    let backup_folder = root.join("backups");
    std::fs::create_dir_all(&backup_folder).expect("backup directory");
    let database = at_db::Database::open_in_memory().expect("database");
    let before = tree_hash(&root);

    let (fixture, roots) = batch(&root, &database);
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: &root,
        backup_folder: &backup_folder,
        recorder: &mut recorder,
    };
    let injector = Arc::new(FailNthOperation {
        target: 2,
        seen: AtomicUsize::new(0),
    });
    let mut events = Vec::new();
    let outcome = with_injector(injector, || {
        apply_bulk(&fixture.plan, &mut context, &roots, |event| {
            events.push(event)
        })
    });
    assert!(
        matches!(outcome, Err(BulkApplyError::Apply(_))),
        "the injected permission failure surfaces as an apply error"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.stage == BulkProgressStage::ItemStarted)
            .count(),
        fixture.plan.items.len(),
        "every item reports started"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.stage == BulkProgressStage::ItemFailed)
            .count(),
        fixture.plan.items.len(),
        "every item reports failed after rollback"
    );
    assert_eq!(
        tree_hash(&root),
        before,
        "the pre-state tree hash is restored, so the first link was rolled back"
    );
}

#[test]
fn a_clean_batch_applies_six_operations_in_one_changeset() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().canonicalize().expect("canonical root");
    let backup_folder = root.join("backups");
    std::fs::create_dir_all(&backup_folder).expect("backup directory");
    let database = at_db::Database::open_in_memory().expect("database");
    let (fixture, roots) = batch(&root, &database);
    assert_eq!(fixture.plan.change_set.ops.len(), 6);
    let mut recorder = at_fs::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::ApplyContext {
        store_root: &root,
        backup_folder: &backup_folder,
        recorder: &mut recorder,
    };
    let mut events = Vec::new();
    apply_bulk(&fixture.plan, &mut context, &roots, |event| {
        events.push(event)
    })
    .expect("clean batch applies");
    assert_eq!(events.len(), 6, "started and done for every item");
    assert_eq!(
        database
            .connection()
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row
                .get::<_, i64>(0))
            .expect("operation count"),
        1,
        "one operations row for the batch"
    );
    assert_eq!(
        database
            .connection()
            .query_row("SELECT COUNT(*) FROM operation_files", [], |row| row
                .get::<_, i64>(0))
            .expect("file count"),
        6,
        "six operation_files rows"
    );
}

#[test]
fn zero_valid_items_is_nothing_to_do_and_writes_nothing() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().canonicalize().expect("canonical root");
    let before = tree_hash(&root);
    let item = ItemId::new();
    let error = plan_bulk(
        &intent(vec![item]),
        vec![BulkItemPlan {
            item_id: item,
            ops: Vec::new(),
            refusal: None,
        }],
    )
    .expect_err("no operations is nothing to do");
    assert!(matches!(
        error,
        at_engine::bulk_skill::BulkPlanError::NothingToDo
    ));
    assert_eq!(tree_hash(&root), before);
}

/// The batch's ChangeSet carries the reviewed operation kind and scope.
#[test]
fn batch_changeset_is_a_deploy_in_the_requested_scope() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().canonicalize().expect("canonical root");
    let database = at_db::Database::open_in_memory().expect("database");
    let (fixture, _) = batch(&root, &database);
    let ChangeSet {
        operation_kind,
        scope,
        ops,
        ..
    } = fixture.plan.change_set;
    assert_eq!(operation_kind.as_str(), "deploy");
    assert_eq!(scope, Some(Scope::User));
    assert_eq!(ops.len(), 6);
}
