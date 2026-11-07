//! TF-042: the durable move-aside journal — operations rows, inverse mapping and undo.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use at_db::Database;
use at_fs::remedies::{SystemMoveFs, move_aside};
use at_fs::{FsError, InverseMove, MoveDraft, MoveJournal, MoveOperationId, MoveStrategy};
use at_store::move_journal::{MOVE_JOURNAL_DIR, MoveJournalStore};
use tempfile::TempDir;
use time::macros::datetime;

struct Fixture {
    _tmp: TempDir,
    support: PathBuf,
    unmanaged: PathBuf,
    database: Arc<Mutex<Database>>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().expect("temp dir");
        let support = tmp.path().join("support");
        let unmanaged = tmp.path().join("home/.claude/skills/review");
        fs::create_dir_all(&support).expect("support dir");
        fs::create_dir_all(unmanaged.join("scripts")).expect("skill dir");
        fs::write(unmanaged.join("SKILL.md"), "name: review\n").expect("skill doc");
        fs::write(unmanaged.join("scripts/run.sh"), "#!/bin/sh\n").expect("script");
        let database = Database::open_file(tmp.path().join("agentuna.db")).expect("database");
        Self {
            _tmp: tmp,
            support,
            unmanaged,
            database: Arc::new(Mutex::new(database)),
        }
    }

    fn journal(&self) -> MoveJournalStore {
        MoveJournalStore::in_support(Arc::clone(&self.database), &self.support)
    }

    fn operation_row(&self, id: &MoveOperationId) -> (String, String, Option<String>) {
        let database = self.database.lock().expect("database");
        database
            .connection()
            .query_row(
                "SELECT kind, status, summary FROM operations WHERE id = ?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("operation row")
    }
}

fn now() -> time::OffsetDateTime {
    datetime!(2026-11-06 10:15:00 UTC)
}

#[test]
fn a_tree_moves_into_the_support_area_writes_an_operation_row_and_undoes() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let outcome = move_aside(
        &at_fs::MoveAsideRequest::new(&fixture.support, &fixture.unmanaged, now()),
        &journal,
    )
    .expect("move aside");

    assert!(!fixture.unmanaged.exists(), "the original path is gone");
    assert!(
        outcome
            .moved_to
            .starts_with(fixture.support.join(".moved-aside")),
        "the tree moved under the support area: {}",
        outcome.moved_to.display()
    );
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("SKILL.md")).expect("skill doc"),
        "name: review\n"
    );
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("scripts/run.sh")).expect("script"),
        "#!/bin/sh\n"
    );

    let (kind, status, summary) = fixture.operation_row(&outcome.operation_id);
    assert_eq!(kind, "move");
    assert_eq!(status, "succeeded");
    assert_eq!(
        summary.as_deref(),
        Some(format!("Moved {} aside", fixture.unmanaged.display()).as_str())
    );
    let record = fixture
        .support
        .join(MOVE_JOURNAL_DIR)
        .join(format!("{}.json", outcome.operation_id.as_str()));
    assert!(record.exists(), "the inverse mapping is journaled");

    let undone =
        at_fs::undo_move(&outcome.operation_id, &journal, &SystemMoveFs).expect("undo the move");
    assert_eq!(undone.original, fixture.unmanaged);
    assert!(fixture.unmanaged.join("SKILL.md").exists());
    let (_, status, _) = fixture.operation_row(&outcome.operation_id);
    assert_eq!(status, "undone");

    let second = at_fs::undo_move(&outcome.operation_id, &journal, &SystemMoveFs)
        .expect_err("an undone move is not undoable twice");
    assert!(matches!(second, FsError::NotFound { .. }), "{second:?}");
}

#[test]
fn a_journal_failure_before_the_move_leaves_the_source_untouched() {
    let fixture = Fixture::new();
    let blocking = fixture.support.join("blocked");
    fs::write(&blocking, b"not a directory").expect("blocking file");
    let journal = MoveJournalStore::new(Arc::clone(&fixture.database), &blocking);

    let error = move_aside(
        &at_fs::MoveAsideRequest::new(&fixture.support, &fixture.unmanaged, now()),
        &journal,
    )
    .expect_err("the journal cannot write its record");
    // Creating the journal directory goes through `at_fs::ensure_dir`, so a regular file in place
    // of that directory is reported as an unusable path naming the cause, not a bare errno.
    let FsError::InvalidPath { path, reason } = &error else {
        panic!("expected a typed path error, got {error:?}");
    };
    assert_eq!(
        path, &blocking,
        "the error names the blocked journal directory"
    );
    assert!(reason.contains("not a directory"), "{reason}");
    assert!(
        fixture.unmanaged.join("SKILL.md").exists(),
        "nothing moved when the journal refused"
    );
    assert!(!fixture.support.join(".moved-aside").exists());
}

#[test]
fn a_pending_record_is_not_undoable_until_the_move_completes() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let draft = MoveDraft {
        action: at_fs::MoveAction::Moved,
        original: fixture.unmanaged.clone(),
        moved_to: fixture.support.join("elsewhere"),
        inverse: InverseMove {
            from: fixture.support.join("elsewhere"),
            to: fixture.unmanaged.clone(),
        },
    };
    let id = journal.begin(draft).expect("pending row");
    let (kind, status, _) = fixture.operation_row(&id);
    assert_eq!(kind, "move");
    assert_eq!(status, "running");
    assert!(
        journal.lookup(&id).expect("lookup").is_none(),
        "a running move has no inverse yet"
    );

    journal
        .complete(&id, MoveStrategy::Rename)
        .expect("complete the move");
    let record = journal.lookup(&id).expect("lookup").expect("record");
    assert_eq!(record.original, fixture.unmanaged);
    assert_eq!(record.strategy, MoveStrategy::Rename);
    assert_eq!(record.inverse.to, fixture.unmanaged);
}

#[test]
fn a_failed_move_is_recorded_with_its_reason_and_stays_undoable_window_closed() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let draft = MoveDraft {
        action: at_fs::MoveAction::Moved,
        original: fixture.unmanaged.clone(),
        moved_to: fixture.support.join("elsewhere"),
        inverse: InverseMove {
            from: fixture.support.join("elsewhere"),
            to: fixture.unmanaged.clone(),
        },
    };
    let id = journal.begin(draft).expect("pending row");
    journal
        .fail(&id, "permission denied")
        .expect("fail the row");

    let database = fixture.database.lock().expect("database");
    let (status, error_kind): (String, Option<String>) = database
        .connection()
        .query_row(
            "SELECT status, error_kind FROM operations WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("operation row");
    assert_eq!(status, "failed");
    assert_eq!(error_kind.as_deref(), Some("permission denied"));
    drop(database);
    assert!(journal.lookup(&id).expect("lookup").is_none());
    assert!(
        fixture.unmanaged.join("SKILL.md").exists(),
        "a failed move changes nothing"
    );
}

#[test]
fn a_reopened_journal_reads_the_same_inverse_mapping() {
    let fixture = Fixture::new();
    let outcome = {
        let journal = fixture.journal();
        move_aside(
            &at_fs::MoveAsideRequest::new(&fixture.support, &fixture.unmanaged, now()),
            &journal,
        )
        .expect("move aside")
    };

    // A new process: a fresh handle on the same index and the same journal directory.
    let database = Database::open_file(fixture._tmp.path().join("agentuna.db")).expect("reopen");
    let reopened = MoveJournalStore::in_support(Arc::new(Mutex::new(database)), &fixture.support);
    let record = reopened
        .lookup(&outcome.operation_id)
        .expect("lookup")
        .expect("record survives a restart");
    assert_eq!(record.original, fixture.unmanaged);
    assert_eq!(record.moved_to, outcome.moved_to);
    assert_eq!(record.strategy, MoveStrategy::Rename);
    assert_eq!(
        record.inverse,
        InverseMove {
            from: outcome.moved_to.clone(),
            to: fixture.unmanaged.clone(),
        }
    );
    at_fs::undo_move(&outcome.operation_id, &reopened, &SystemMoveFs).expect("undo after restart");
    assert!(fixture.unmanaged.join("SKILL.md").exists());
}

#[test]
fn journal_records_never_land_inside_a_canonical_store() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    assert_eq!(
        journal.directory(),
        fixture.support.join(MOVE_JOURNAL_DIR).as_path()
    );
    assert!(
        !journal
            .directory()
            .starts_with(fixture.unmanaged.parent().unwrap_or(Path::new("/")))
    );
    assert!(
        !journal
            .directory()
            .starts_with(fixture.support.join(".moved-aside"))
    );
}
