//! Integration tests for authorized absolute ChangeSet destinations.
use std::fs;
use std::path::{Path, PathBuf};

use at_fs::{ApplyContext, ApplyRoots, ChangeSetApplyError, OperationRecorder, apply_with_roots};
use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind, Scope};
use tempfile::TempDir;

struct Recorder {
    fail_file_completion: bool,
}

impl OperationRecorder for Recorder {
    fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
        Ok("apply-roots-test".to_owned())
    }

    fn file_completed(
        &mut self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&Path>,
    ) -> Result<(), String> {
        if self.fail_file_completion {
            Err("injected history failure".to_owned())
        } else {
            Ok(())
        }
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

fn write_set(path: PathBuf) -> ChangeSet {
    ChangeSet {
        id: "apply-roots".to_owned(),
        preview_id: "preview".to_owned(),
        operation_kind: OperationKind::parse("edit_rule").expect("known operation"),
        scope: Some(Scope::Project),
        project_id: None,
        summary: "authorized absolute write".to_owned(),
        ops: vec![FileOp::Write {
            path,
            bytes: b"approved".to_vec(),
            expected: ExpectedState::Absent,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: 0,
    }
}

fn apply(
    set: &ChangeSet,
    store: &Path,
    roots: &ApplyRoots,
    fail: bool,
) -> Result<(), ChangeSetApplyError> {
    let mut recorder = Recorder {
        fail_file_completion: fail,
    };
    let backup = store.join("backups");
    let mut context = ApplyContext {
        store_root: store,
        backup_folder: &backup,
        recorder: &mut recorder,
    };
    apply_with_roots(set, &mut context, roots).map(|_| ())
}

#[test]
fn absolute_destination_under_authorized_root_is_applied() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(root.join(".config")).expect("target parent");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    let target = root.join(".config/rules.md");
    apply(&write_set(target.clone()), &store, &roots, false).expect("apply");
    assert_eq!(fs::read(target).expect("deployed file"), b"approved");
}
#[test]
fn most_specific_authorized_root_is_selected_when_roots_overlap() {
    let tmp = TempDir::new().expect("tempdir");
    let broad_root = tmp.path().to_path_buf();
    let narrow_root = broad_root.join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(&narrow_root).expect("narrow root");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([broad_root.clone(), narrow_root.clone()]).expect("valid roots");
    let target = narrow_root.join("rule.md");

    apply(&write_set(target.clone()), &store, &roots, false).expect("apply");
    assert_eq!(fs::read(target).expect("deployed file"), b"approved");
}
#[test]
fn nested_missing_directories_are_created_and_rolled_back_with_the_write() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    let target = root.join(".config/agent/rules.md");

    apply(&write_set(target.clone()), &store, &roots, false).expect("apply");
    assert_eq!(fs::read(&target).expect("written file"), b"approved");

    fs::remove_file(&target).expect("remove target before failure case");
    fs::remove_dir_all(root.join(".config")).expect("remove created parents");
    let result = apply(&write_set(target.clone()), &store, &roots, true);
    assert!(matches!(result, Err(ChangeSetApplyError::Apply { .. })));
    assert!(!target.exists());
    assert!(!root.join(".config").exists());
}

#[cfg(unix)]
#[test]
fn missing_tail_below_symlink_to_outside_is_refused() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let outside = tmp.path().join("outside");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&outside).expect("outside");
    fs::create_dir_all(&store).expect("store");
    symlink(&outside, root.join("escape")).expect("symlink");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    assert!(matches!(
        apply(
            &write_set(root.join("escape/missing/nested/file")),
            &store,
            &roots,
            false
        ),
        Err(ChangeSetApplyError::OutsideAuthorizedRoots(_))
    ));
    assert!(!outside.join("missing").exists());
}

#[test]
fn parent_component_in_missing_tail_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    assert!(matches!(
        apply(
            &write_set(root.join("missing/../file")),
            &store,
            &roots,
            false
        ),
        Err(ChangeSetApplyError::OutsideAuthorizedRoots(_))
    ));
}

#[test]
fn parent_component_escape_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(tmp.path()).expect("parent");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    let escaped = root.join("../outside/file");
    assert!(matches!(
        apply(&write_set(escaped), &store, &roots, false),
        Err(ChangeSetApplyError::OutsideAuthorizedRoots(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_parent_that_resolves_outside_is_refused() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let outside = tmp.path().join("outside");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&outside).expect("outside");
    fs::create_dir_all(&store).expect("store");
    symlink(&outside, root.join("escape")).expect("symlink");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    assert!(matches!(
        apply(&write_set(root.join("escape/file")), &store, &roots, false),
        Err(ChangeSetApplyError::OutsideAuthorizedRoots(_))
    ));
    assert!(!outside.join("file").exists());
}

#[test]
fn sibling_prefix_is_not_authorized() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().join("home/u");
    let sibling = tmp.path().join("home/u2");
    let store = tmp.path().join("store");
    fs::create_dir_all(&home).expect("authorized root");
    fs::create_dir_all(&sibling).expect("sibling");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([home]).expect("valid root");
    assert!(matches!(
        apply(&write_set(sibling.join("file")), &store, &roots, false),
        Err(ChangeSetApplyError::OutsideAuthorizedRoots(_))
    ));
}

#[test]
fn filesystem_root_is_rejected_as_an_authorized_root() {
    assert!(matches!(
        ApplyRoots::new([PathBuf::from("/")]),
        Err(ChangeSetApplyError::InvalidAuthorizedRoot(_))
    ));
}

#[test]
fn absolute_destination_is_journaled_and_rolled_back_on_history_failure() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("home");
    let store = tmp.path().join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&store).expect("store");
    let roots = ApplyRoots::new([root.clone()]).expect("valid root");
    let target = root.join("rule.md");
    let result = apply(&write_set(target.clone()), &store, &roots, true);
    assert!(matches!(result, Err(ChangeSetApplyError::Apply { .. })));
    assert!(!target.exists(), "failed absolute write must roll back");
    let journal_dir = store.join(".state/journal");
    assert!(
        !journal_dir.exists()
            || journal_dir
                .read_dir()
                .expect("journal directory")
                .next()
                .is_none(),
        "successful rollback must remove its absolute-path journal"
    );
}
