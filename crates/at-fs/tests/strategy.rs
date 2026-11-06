//! TS-027: skill link strategy, cross-volume policy and typed error translation.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_fs::{
    COPY_MANIFEST_FILE, FsError, LinkMode, SkillDeployRequest, SkillStrategy, SkillWarning,
    SystemVolumes, VolumeLookup, deploy_skill, deploy_skill_with, normalize_error, normalize_io,
    skill_mode,
};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    item: PathBuf,
    agent: PathBuf,
    target: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("home/.agentuna/skills");
    let item = store.join("review");
    let agent = tmp.path().join("home/.claude/skills");
    fs::create_dir_all(&item).unwrap();
    fs::create_dir_all(&agent).unwrap();
    fs::write(item.join("SKILL.md"), "name: review\n").unwrap();
    let target = agent.join("review");
    Fixture {
        _tmp: tmp,
        item,
        agent,
        target,
    }
}

#[derive(Debug, Clone, Copy)]
struct FakeVolumes {
    same: bool,
}

impl VolumeLookup for FakeVolumes {
    fn same_volume(&self, _source: &Path, _destination: &Path) -> bool {
        self.same
    }
}

fn link_request(f: &Fixture) -> SkillDeployRequest {
    SkillDeployRequest::new(&f.item, &f.target, SkillStrategy::new(LinkMode::Link, None))
}

#[test]
fn macos_creates_a_relative_symlink_that_reads_through() {
    let f = fixture();
    let outcome = deploy_skill(&link_request(&f)).unwrap();
    assert_eq!(outcome.mode, LinkMode::Link);
    assert!(outcome.warnings.is_empty());

    let meta = fs::symlink_metadata(&f.target).unwrap();
    assert!(meta.file_type().is_symlink());
    let raw = fs::read_link(&f.target).unwrap();
    assert!(!raw.is_absolute(), "same-volume links must be relative");
    assert_eq!(
        fs::canonicalize(&f.target).unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );
    assert_eq!(
        fs::read_to_string(f.target.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
}

#[test]
fn an_owned_link_is_replaced_without_leaving_staging_behind() {
    let f = fixture();
    deploy_skill(&link_request(&f)).unwrap();
    deploy_skill(&link_request(&f)).unwrap();
    assert_eq!(
        fs::canonicalize(&f.target).unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );
    let leftovers: Vec<String> = fs::read_dir(&f.agent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "review")
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
}

#[test]
fn a_foreign_link_is_refused_and_the_error_names_its_target() {
    let f = fixture();
    let foreign = f._tmp.path().join("elsewhere/review");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "someone else\n").unwrap();
    std::os::unix::fs::symlink(&foreign, &f.target).unwrap();

    let error = deploy_skill(&link_request(&f)).unwrap_err();
    let FsError::ForeignLink { path, target } = error else {
        panic!("expected ForeignLink");
    };
    assert_eq!(path, f.target);
    assert_eq!(target, fs::canonicalize(&foreign).unwrap());
    assert_eq!(
        fs::read_to_string(foreign.join("SKILL.md")).unwrap(),
        "someone else\n"
    );
}

#[test]
fn a_real_folder_is_refused_with_its_path() {
    let f = fixture();
    fs::create_dir_all(&f.target).unwrap();
    fs::write(f.target.join("notes.md"), "mine\n").unwrap();
    let error = deploy_skill(&link_request(&f)).unwrap_err();
    let FsError::RefusedOverwrite { path, kind, .. } = error else {
        panic!("expected RefusedOverwrite");
    };
    assert_eq!(path, f.target);
    assert_eq!(kind, at_fs::TargetKind::Dir);
    assert_eq!(
        fs::read_to_string(f.target.join("notes.md")).unwrap(),
        "mine\n"
    );
}

#[test]
fn a_read_only_parent_returns_permission_denied_and_leaves_nothing() {
    let f = fixture();
    let lock = f.agent.join("locked");
    fs::create_dir_all(&lock).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o500)).unwrap();
    let target = lock.join("review");

    let result = deploy_skill(&SkillDeployRequest::new(
        &f.item,
        &target,
        SkillStrategy::new(LinkMode::Link, None),
    ));
    let entries = fs::read_dir(&lock)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o700)).unwrap();

    match result {
        // A privileged test user can write into a 0500 directory; only a normal user sees the
        // refusal this test is about.
        Ok(_) => {}
        Err(error) => {
            let FsError::PermissionDenied { path } = error else {
                panic!("expected PermissionDenied, got {error:?}");
            };
            assert_eq!(path, lock);
            assert!(entries.is_empty(), "partial files left: {entries:?}");
        }
    }
}

#[test]
fn a_cross_volume_link_without_fallback_is_refused() {
    let f = fixture();
    let request = link_request(&f);
    let error = deploy_skill_with(&request, &FakeVolumes { same: false }).unwrap_err();
    let FsError::CrossVolume { from, to } = error else {
        panic!("expected CrossVolume");
    };
    assert_eq!(from, fs::canonicalize(&f.item).unwrap());
    assert_eq!(to, f.target);
    assert!(fs::symlink_metadata(&f.target).is_err());
}

#[test]
fn a_cross_volume_link_with_fallback_becomes_a_copy_with_a_warning() {
    let f = fixture();
    let request = SkillDeployRequest::new(
        &f.item,
        &f.target,
        SkillStrategy::new(LinkMode::Link, None).with_copy_fallback(),
    );
    let outcome = deploy_skill_with(&request, &FakeVolumes { same: false }).unwrap();
    assert_eq!(outcome.mode, LinkMode::Copy);
    assert_eq!(
        outcome.warnings,
        vec![SkillWarning::CopyFallbackCrossVolume]
    );
    let meta = fs::symlink_metadata(&f.target).unwrap();
    assert!(meta.is_dir());
    assert!(!meta.file_type().is_symlink());
    assert_eq!(
        fs::read_to_string(f.target.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
    assert!(f.target.join(COPY_MANIFEST_FILE).is_file());
}

#[test]
fn error_normalization_maps_errno_codes_to_typed_errors() {
    let path = Path::new("/agent/skills/review");

    let denied = normalize_io(path, io::Error::from_raw_os_error(libc::EACCES));
    assert!(matches!(denied, FsError::PermissionDenied { .. }));
    assert!(denied.remediation().is_some());

    let not_permitted = normalize_io(path, io::Error::from_raw_os_error(libc::EPERM));
    assert!(matches!(not_permitted, FsError::PermissionDenied { .. }));

    let full = normalize_io(path, io::Error::from_raw_os_error(libc::ENOSPC));
    assert!(matches!(full, FsError::NoSpaceLeft { .. }));
    assert!(full.remediation().is_some());

    let quota = normalize_io(path, io::Error::from_raw_os_error(libc::EDQUOT));
    assert!(matches!(quota, FsError::QuotaExceeded { .. }));
    assert!(quota.remediation().is_some());

    let generic = io::Error::other("boom");
    assert!(matches!(normalize_io(path, generic), FsError::Io { .. }));
}

#[test]
fn normalize_error_rewrites_an_already_mapped_io_variant() {
    let error = FsError::Io {
        path: PathBuf::from("/agent/x"),
        source: io::Error::from_raw_os_error(libc::ENOSPC),
    };
    assert!(matches!(
        normalize_error(error),
        FsError::NoSpaceLeft { .. }
    ));
}

#[test]
fn the_per_agent_setting_wins_over_the_default() {
    assert_eq!(
        skill_mode(LinkMode::Link, Some(LinkMode::Copy)),
        LinkMode::Copy
    );
    assert_eq!(skill_mode(LinkMode::Copy, None), LinkMode::Copy);
    let strategy = SkillStrategy::new(LinkMode::Copy, Some(LinkMode::Link));
    assert_eq!(strategy.mode, LinkMode::Link);
    assert!(!strategy.copy_fallback, "fallback is opt-in");
    assert!(strategy.with_copy_fallback().copy_fallback);
}

#[test]
fn the_system_volume_check_is_true_for_one_temporary_tree() {
    let f = fixture();
    assert!(SystemVolumes.same_volume(&f.item, &f.agent));
}
