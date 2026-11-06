//! TF-042: move-aside into the support area, cross-device verification and refusal remedies.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_fs::remedies::{MoveFs, SystemMoveFs, move_aside, move_aside_with};
use at_fs::{
    ADOPT_FLOW_ID, FsError, InverseMove, MoveAction, MoveAsideRequest, MoveDraft, MoveJournal,
    MoveOperationId, MoveRecord, MoveStrategy, RefusalRemedies, RefusalRemedy, encode_path,
};
use tempfile::TempDir;
use time::macros::datetime;

struct Fixture {
    _tmp: TempDir,
    support: PathBuf,
    unmanaged: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let support = tmp.path().join("support");
    let unmanaged = tmp.path().join("home/.claude/skills/review");
    fs::create_dir_all(&support).unwrap();
    fs::create_dir_all(unmanaged.join("scripts")).unwrap();
    fs::write(unmanaged.join("SKILL.md"), "name: review\n").unwrap();
    fs::write(unmanaged.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    Fixture {
        _tmp: tmp,
        support,
        unmanaged,
    }
}

#[derive(Debug, Default)]
struct MemoryJournal {
    rows: RefCell<BTreeMap<String, MoveRecord>>,
    next: Cell<u32>,
    completed: RefCell<Vec<(String, MoveStrategy)>>,
    failed: RefCell<Vec<String>>,
    undone: RefCell<Vec<String>>,
}

impl MoveJournal for MemoryJournal {
    fn begin(&self, draft: MoveDraft) -> Result<MoveOperationId, FsError> {
        let next = self.next.get() + 1;
        self.next.set(next);
        let id = MoveOperationId::new(format!("op-{next}"));
        let record = MoveRecord {
            operation_id: id.clone(),
            action: draft.action,
            original: draft.original,
            moved_to: draft.moved_to,
            strategy: MoveStrategy::Rename,
            inverse: draft.inverse,
        };
        self.rows
            .borrow_mut()
            .insert(id.as_str().to_owned(), record);
        Ok(id)
    }

    fn complete(
        &self,
        operation_id: &MoveOperationId,
        strategy: MoveStrategy,
    ) -> Result<(), FsError> {
        if let Some(record) = self.rows.borrow_mut().get_mut(operation_id.as_str()) {
            record.strategy = strategy;
        }
        self.completed
            .borrow_mut()
            .push((operation_id.as_str().to_owned(), strategy));
        Ok(())
    }

    fn fail(&self, operation_id: &MoveOperationId, reason: &str) -> Result<(), FsError> {
        self.failed
            .borrow_mut()
            .push(format!("{}: {reason}", operation_id.as_str()));
        Ok(())
    }

    fn lookup(&self, operation_id: &MoveOperationId) -> Result<Option<MoveRecord>, FsError> {
        Ok(self.rows.borrow().get(operation_id.as_str()).cloned())
    }

    fn mark_undone(&self, operation_id: &MoveOperationId) -> Result<(), FsError> {
        self.undone
            .borrow_mut()
            .push(operation_id.as_str().to_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct FailingJournal;

impl MoveJournal for FailingJournal {
    fn begin(&self, _draft: MoveDraft) -> Result<MoveOperationId, FsError> {
        Err(FsError::Io {
            path: PathBuf::from("operations"),
            source: io::Error::other("database unavailable"),
        })
    }

    fn complete(
        &self,
        _operation_id: &MoveOperationId,
        _strategy: MoveStrategy,
    ) -> Result<(), FsError> {
        Ok(())
    }

    fn fail(&self, _operation_id: &MoveOperationId, _reason: &str) -> Result<(), FsError> {
        Ok(())
    }

    fn lookup(&self, _operation_id: &MoveOperationId) -> Result<Option<MoveRecord>, FsError> {
        Ok(None)
    }

    fn mark_undone(&self, _operation_id: &MoveOperationId) -> Result<(), FsError> {
        Ok(())
    }
}

/// The real filesystem, but every rename claims to cross devices.
#[derive(Debug, Clone, Copy)]
struct CrossDeviceFs;

impl MoveFs for CrossDeviceFs {
    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    }

    fn copy_tree(&self, from: &Path, to: &Path) -> io::Result<()> {
        SystemMoveFs.copy_tree(from, to)
    }

    fn remove_tree(&self, path: &Path) -> io::Result<()> {
        SystemMoveFs.remove_tree(path)
    }
}

/// Writes a partial destination and then fails, like an interrupted copy.
#[derive(Debug, Clone, Copy)]
struct InterruptedCopyFs;

impl MoveFs for InterruptedCopyFs {
    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    }

    fn copy_tree(&self, _from: &Path, to: &Path) -> io::Result<()> {
        fs::create_dir_all(to)?;
        fs::write(to.join("partial"), "half")?;
        Err(io::Error::other("interrupted"))
    }

    fn remove_tree(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_tree_moves_into_the_timestamped_moved_aside_area_and_undo_restores_it() {
    let f = fixture();
    let now = datetime!(2026-11-05 17:04:09 -5);
    let journal = MemoryJournal::default();
    let outcome = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, now),
        &journal,
    )
    .unwrap();

    let stamp_dir = f.support.join(".moved-aside/20261105T220409Z");
    assert_eq!(
        outcome.moved_to,
        stamp_dir.join(encode_path(&f.unmanaged).unwrap())
    );
    assert!(fs::symlink_metadata(&f.unmanaged).is_err());
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("scripts/run.sh")).unwrap(),
        "#!/bin/sh\n"
    );

    let record = journal.lookup(&outcome.operation_id).unwrap().unwrap();
    assert_eq!(record.action, MoveAction::Moved);
    assert_eq!(record.original, outcome.original);
    assert_eq!(record.moved_to, outcome.moved_to);
    assert_eq!(record.strategy, MoveStrategy::Rename);
    assert_eq!(
        record.inverse,
        InverseMove {
            from: outcome.moved_to.clone(),
            to: outcome.original.clone(),
        }
    );
    assert_eq!(
        journal.completed.borrow().as_slice(),
        &[(
            outcome.operation_id.as_str().to_owned(),
            MoveStrategy::Rename
        )]
    );

    let undone = at_fs::undo_move(&outcome.operation_id, &journal, &SystemMoveFs).unwrap();
    assert_eq!(undone.original, f.unmanaged);
    assert_eq!(undone.moved_back_from, outcome.moved_to);
    assert!(fs::symlink_metadata(&outcome.moved_to).is_err());
    assert_eq!(
        fs::read_to_string(f.unmanaged.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    assert_eq!(
        journal.undone.borrow().as_slice(),
        &[outcome.operation_id.as_str().to_owned()]
    );
}

#[test]
fn a_second_move_in_the_same_second_gets_a_numeric_suffix() {
    let f = fixture();
    let now = datetime!(2026-11-05 17:04:09 -5);
    let journal = MemoryJournal::default();
    let first = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, now),
        &journal,
    )
    .unwrap();

    fs::create_dir_all(&f.unmanaged).unwrap();
    fs::write(f.unmanaged.join("SKILL.md"), "second\n").unwrap();
    let second = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, now),
        &journal,
    )
    .unwrap();

    assert_ne!(first.moved_to, second.moved_to);
    assert_eq!(
        fs::read_to_string(first.moved_to.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    assert_eq!(
        fs::read_to_string(second.moved_to.join("SKILL.md")).unwrap(),
        "second\n"
    );
}

#[test]
fn a_cross_device_move_copies_verifies_and_then_deletes_the_source() {
    let f = fixture();
    let journal = MemoryJournal::default();
    let outcome = move_aside_with(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, datetime!(2026-11-05 17:04:09 UTC)),
        &journal,
        &CrossDeviceFs,
    )
    .unwrap();

    assert!(fs::symlink_metadata(&f.unmanaged).is_err());
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    let record = journal.lookup(&outcome.operation_id).unwrap().unwrap();
    assert_eq!(record.strategy, MoveStrategy::CopyVerified);
}

#[test]
fn an_interrupted_copy_leaves_the_source_untouched_and_no_partial_destination() {
    let f = fixture();
    let journal = MemoryJournal::default();
    let error = move_aside_with(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, datetime!(2026-11-05 17:04:09 UTC)),
        &journal,
        &InterruptedCopyFs,
    )
    .unwrap_err();
    assert!(matches!(error, FsError::Io { .. }));
    assert_eq!(
        fs::read_to_string(f.unmanaged.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    assert_eq!(
        fs::read_to_string(f.unmanaged.join("scripts/run.sh")).unwrap(),
        "#!/bin/sh\n"
    );
    assert_eq!(journal.failed.borrow().len(), 1);
    let stamp_dir = f.support.join(".moved-aside/20261105T220409Z");
    let leftovers: Vec<_> = fs::read_dir(&stamp_dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[test]
fn a_read_only_parent_returns_permission_denied_and_keeps_the_bytes() {
    let f = fixture();
    let parent = f.unmanaged.parent().unwrap().to_path_buf();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
    let journal = MemoryJournal::default();
    let result = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, datetime!(2026-11-05 17:04:09 UTC)),
        &journal,
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

    match result {
        Ok(_) => {}
        Err(error) => {
            assert!(
                matches!(error, FsError::PermissionDenied { .. }),
                "{error:?}"
            );
            assert_eq!(
                fs::read_to_string(f.unmanaged.join("SKILL.md")).unwrap(),
                "name: review\n"
            );
            assert!(journal.completed.borrow().is_empty());
            assert_eq!(journal.failed.borrow().len(), 1);
        }
    }
}

#[test]
fn undo_refuses_to_clobber_a_new_file_at_the_original_path() {
    let f = fixture();
    let journal = MemoryJournal::default();
    let outcome = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, datetime!(2026-11-05 17:04:09 UTC)),
        &journal,
    )
    .unwrap();
    fs::create_dir_all(&f.unmanaged).unwrap();
    fs::write(f.unmanaged.join("SKILL.md"), "newer\n").unwrap();

    let error = at_fs::undo_move(&outcome.operation_id, &journal, &SystemMoveFs).unwrap_err();
    assert!(matches!(error, FsError::AlreadyExists { .. }));
    assert_eq!(
        fs::read_to_string(f.unmanaged.join("SKILL.md")).unwrap(),
        "newer\n"
    );
    assert_eq!(
        fs::read_to_string(outcome.moved_to.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
}

#[test]
fn a_journal_failure_before_the_move_changes_nothing() {
    let f = fixture();
    let error = move_aside(
        &MoveAsideRequest::new(&f.support, &f.unmanaged, datetime!(2026-11-05 17:04:09 UTC)),
        &FailingJournal,
    )
    .unwrap_err();
    assert!(matches!(error, FsError::Io { .. }));
    assert_eq!(
        fs::read_to_string(f.unmanaged.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
}

#[test]
fn a_later_move_below_an_already_moved_path_does_not_collide() {
    let tmp = TempDir::new().unwrap();
    let support = tmp.path().join("support");
    fs::create_dir_all(&support).unwrap();
    let parent = tmp.path().join("home");
    fs::create_dir_all(&parent).unwrap();
    let moved = parent.join("x");
    fs::write(&moved, "first").unwrap();
    let journal = MemoryJournal::default();
    let now = datetime!(2026-11-05 17:04:09 UTC);

    let first = move_aside(&MoveAsideRequest::new(&support, &moved, now), &journal).unwrap();
    fs::create_dir_all(&moved).unwrap();
    fs::write(moved.join("y"), "second").unwrap();
    let second = move_aside(
        &MoveAsideRequest::new(&support, moved.join("y"), now),
        &journal,
    )
    .unwrap();

    assert_ne!(first.moved_to, second.moved_to);
    assert_eq!(fs::read_to_string(first.moved_to).unwrap(), "first");
    assert_eq!(fs::read_to_string(second.moved_to).unwrap(), "second");
}

#[test]
fn refusal_remedies_expose_adopt_via_the_tc011_flow() {
    let remedies = RefusalRemedies::standard();
    assert_eq!(remedies.remedies().len(), 3);
    assert_eq!(
        remedies.remedies()[0],
        RefusalRemedy::AdoptVia {
            flow_id: ADOPT_FLOW_ID
        }
    );
    assert_eq!(remedies.remedies()[1], RefusalRemedy::MoveAside);
    assert_eq!(remedies.remedies()[2], RefusalRemedy::Cancel);
    assert_eq!(remedies.adopt_flow_id(), Some("TC-011"));
    assert_eq!(remedies, RefusalRemedies::default());
    assert_eq!(
        remedies.model_remedies(),
        vec![
            at_model::Remedy::Adopt,
            at_model::Remedy::MoveAside,
            at_model::Remedy::Cancel
        ]
    );
}

#[test]
fn at_fs_has_no_adopt_implementation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                assert!(
                    !text.contains("adopt_item"),
                    "{} mentions adopt_item; adopt belongs to TC-011",
                    path.display()
                );
            }
        }
    }
}
