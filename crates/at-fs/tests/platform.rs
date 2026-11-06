//! TF-038: platform-neutral link ordering, junction storage rules and link removal.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;

use at_fs::{
    FsError, HostPlatform, LinkMethod, LinkRemovalRequest, LinkRequest, LinkShape, LinkTargetKind,
    Linker, RemovalOperation, StoredLinkMode, SystemLinker, classify_link_shape,
    ensure_mode_supported, link_attempt_order, link_with_fallback, removal_operation, remove_link,
};
use tempfile::TempDir;

const PRIVILEGE_NOT_HELD: i32 = 1314;

/// A linker that records calls and can simulate a platform refusing symlinks and junctions.
#[derive(Debug, Default)]
struct FakeLinker {
    calls: RefCell<Vec<&'static str>>,
    symlink_raw: Option<i32>,
    symlink_kind: Option<io::ErrorKind>,
    junction_ok: bool,
    copy_ok: bool,
}

impl Linker for FakeLinker {
    fn symlink_dir(&self, _link_target: &Path, _link_path: &Path) -> io::Result<()> {
        self.symlink()
    }

    fn symlink_file(&self, _link_target: &Path, _link_path: &Path) -> io::Result<()> {
        self.symlink()
    }

    fn junction_dir(&self, _link_target: &Path, _link_path: &Path) -> io::Result<()> {
        self.calls.borrow_mut().push("junction");
        if self.junction_ok {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no junction support",
            ))
        }
    }

    fn copy_dir(&self, _source: &Path, _destination: &Path) -> io::Result<()> {
        self.calls.borrow_mut().push("copy");
        if self.copy_ok {
            Ok(())
        } else {
            Err(io::Error::other("no copy support"))
        }
    }
}

impl FakeLinker {
    fn symlink(&self) -> io::Result<()> {
        self.calls.borrow_mut().push("symlink");
        if let Some(code) = self.symlink_raw {
            return Err(io::Error::from_raw_os_error(code));
        }
        if let Some(kind) = self.symlink_kind {
            return Err(io::Error::new(kind, "refused"));
        }
        Ok(())
    }
}

fn request<'a>(source: &'a Path, link: &'a Path, kind: LinkTargetKind) -> LinkRequest<'a> {
    LinkRequest {
        source,
        link,
        kind,
        allow_junction: true,
        allow_copy: true,
    }
}

#[test]
fn windows_directory_order_is_symlink_then_junction_then_copy() {
    assert_eq!(
        link_attempt_order(HostPlatform::Windows, LinkTargetKind::Directory, true, true),
        vec![LinkMethod::Symlink, LinkMethod::Junction, LinkMethod::Copy]
    );
    assert_eq!(
        link_attempt_order(
            HostPlatform::Windows,
            LinkTargetKind::Directory,
            true,
            false
        ),
        vec![LinkMethod::Symlink, LinkMethod::Junction]
    );
    assert_eq!(
        link_attempt_order(HostPlatform::Windows, LinkTargetKind::File, true, true),
        vec![LinkMethod::Symlink, LinkMethod::Copy],
        "a file never becomes a junction"
    );
    assert_eq!(
        link_attempt_order(HostPlatform::MacOs, LinkTargetKind::Directory, true, true),
        vec![LinkMethod::Symlink, LinkMethod::Copy],
        "junctions are Windows-only"
    );
}

#[test]
fn missing_symlink_privilege_falls_back_to_a_junction() {
    let linker = FakeLinker {
        symlink_raw: Some(PRIVILEGE_NOT_HELD),
        junction_ok: true,
        ..FakeLinker::default()
    };
    let made = link_with_fallback(
        &linker,
        HostPlatform::Windows,
        &request(
            Path::new("/store/x"),
            Path::new("/agent/x"),
            LinkTargetKind::Directory,
        ),
    )
    .unwrap();
    assert_eq!(made.method, LinkMethod::Junction);
    assert_eq!(made.attempts.len(), 1);
    assert_eq!(made.attempts[0].method, LinkMethod::Symlink);
    assert_eq!(*linker.calls.borrow(), ["symlink", "junction"]);
}

#[test]
fn junction_refusal_falls_back_to_a_copy() {
    let linker = FakeLinker {
        symlink_raw: Some(PRIVILEGE_NOT_HELD),
        junction_ok: false,
        copy_ok: true,
        ..FakeLinker::default()
    };
    let made = link_with_fallback(
        &linker,
        HostPlatform::Windows,
        &request(
            Path::new("/store/x"),
            Path::new("/agent/x"),
            LinkTargetKind::Directory,
        ),
    )
    .unwrap();
    assert_eq!(made.method, LinkMethod::Copy);
    assert_eq!(*linker.calls.borrow(), ["symlink", "junction", "copy"]);
}

#[test]
fn a_non_privilege_error_is_not_hidden_by_a_fallback() {
    let linker = FakeLinker {
        symlink_kind: Some(io::ErrorKind::AlreadyExists),
        junction_ok: true,
        copy_ok: true,
        ..FakeLinker::default()
    };
    let error = link_with_fallback(
        &linker,
        HostPlatform::Windows,
        &request(
            Path::new("/store/x"),
            Path::new("/agent/x"),
            LinkTargetKind::Directory,
        ),
    )
    .unwrap_err();
    assert!(matches!(error, FsError::AlreadyExists { .. }));
    assert_eq!(*linker.calls.borrow(), ["symlink"]);
}

#[test]
fn file_links_never_try_a_junction() {
    let linker = FakeLinker {
        symlink_raw: Some(PRIVILEGE_NOT_HELD),
        copy_ok: true,
        ..FakeLinker::default()
    };
    let made = link_with_fallback(
        &linker,
        HostPlatform::Windows,
        &request(
            Path::new("/store/x"),
            Path::new("/agent/x"),
            LinkTargetKind::File,
        ),
    )
    .unwrap();
    assert_eq!(made.method, LinkMethod::Copy);
    assert_eq!(*linker.calls.borrow(), ["symlink", "copy"]);
}

#[test]
fn junction_mode_round_trips_and_is_guarded_by_platform() {
    assert_eq!(
        StoredLinkMode::from_db("junction"),
        Some(StoredLinkMode::Junction)
    );
    assert_eq!(StoredLinkMode::Junction.as_db(), "junction");
    assert_eq!(StoredLinkMode::Symlink.as_db(), "symlink");
    assert_eq!(StoredLinkMode::from_db("native"), None);
    assert_eq!(StoredLinkMode::from_db("none"), None);

    let path = Path::new("/agent/skills/x");
    assert!(matches!(
        ensure_mode_supported(StoredLinkMode::Junction, HostPlatform::MacOs, path),
        Err(FsError::UnsupportedPlatform { .. })
    ));
    assert!(matches!(
        ensure_mode_supported(StoredLinkMode::Junction, HostPlatform::Linux, path),
        Err(FsError::UnsupportedPlatform { .. })
    ));
    assert!(ensure_mode_supported(StoredLinkMode::Junction, HostPlatform::Windows, path).is_ok());
    assert!(ensure_mode_supported(StoredLinkMode::Symlink, HostPlatform::MacOs, path).is_ok());
    assert!(ensure_mode_supported(StoredLinkMode::Copy, HostPlatform::Linux, path).is_ok());
}

#[test]
fn removal_uses_remove_directory_for_junctions_and_remove_file_for_links() {
    let path = Path::new("/agent/skills/x");
    assert_eq!(
        removal_operation(LinkShape::Junction, HostPlatform::Windows, path).unwrap(),
        RemovalOperation::RemoveDirectory
    );
    assert_eq!(
        removal_operation(LinkShape::SymlinkDir, HostPlatform::MacOs, path).unwrap(),
        RemovalOperation::RemoveFile
    );
    assert_eq!(
        removal_operation(LinkShape::SymlinkFile, HostPlatform::MacOs, path).unwrap(),
        RemovalOperation::RemoveFile
    );
    assert!(matches!(
        removal_operation(LinkShape::Junction, HostPlatform::MacOs, path),
        Err(FsError::UnsupportedPlatform { .. })
    ));
    assert!(matches!(
        removal_operation(LinkShape::Missing, HostPlatform::MacOs, path),
        Err(FsError::NotFound { .. })
    ));
}

#[test]
fn removing_a_symlink_keeps_the_store_content() {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("store/skills/review");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("SKILL.md"), "name: review\n").unwrap();
    let link = tmp.path().join("agent/review");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&store, &link).unwrap();

    let shape = classify_link_shape(&link, HostPlatform::MacOs).unwrap();
    assert_eq!(shape, LinkShape::SymlinkDir);
    let removal = remove_link(
        &LinkRemovalRequest {
            path: link.clone(),
            store_roots: vec![tmp.path().join("store")],
            shape,
        },
        HostPlatform::MacOs,
    )
    .unwrap();
    assert_eq!(removal.operation, RemovalOperation::RemoveFile);
    assert!(fs::symlink_metadata(&link).is_err());
    assert_eq!(
        fs::read_to_string(store.join("SKILL.md")).unwrap(),
        "name: review\n"
    );
}

#[test]
fn a_link_pointing_outside_the_store_is_not_removed() {
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside/precious");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.md"), "keep\n").unwrap();
    let link = tmp.path().join("agent/precious");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let shape = classify_link_shape(&link, HostPlatform::MacOs).unwrap();
    let error = remove_link(
        &LinkRemovalRequest {
            path: link.clone(),
            store_roots: vec![tmp.path().join("store")],
            shape,
        },
        HostPlatform::MacOs,
    )
    .unwrap_err();
    assert!(matches!(error, FsError::DeploymentNotOwned { .. }));
    assert!(fs::symlink_metadata(&link).is_ok());
    assert_eq!(
        fs::read_to_string(outside.join("keep.md")).unwrap(),
        "keep\n"
    );
}

#[test]
fn the_unix_linker_refuses_junctions_without_touching_disk() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("target");
    fs::create_dir_all(&target).unwrap();
    let link = tmp.path().join("link");
    let error = SystemLinker.junction_dir(&target, &link).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(fs::symlink_metadata(&link).is_err());
}

#[test]
fn removing_a_copy_directory_is_refused_by_remove_link() {
    let tmp = TempDir::new().unwrap();
    let copy = tmp.path().join("agent/copy");
    fs::create_dir_all(&copy).unwrap();
    let shape = classify_link_shape(&copy, HostPlatform::MacOs).unwrap();
    assert_eq!(shape, LinkShape::RealDir);
    let error = remove_link(
        &LinkRemovalRequest {
            path: copy.clone(),
            store_roots: vec![tmp.path().join("store")],
            shape,
        },
        HostPlatform::MacOs,
    )
    .unwrap_err();
    assert!(matches!(error, FsError::DeploymentNotOwned { .. }));
    assert!(copy.exists());
}

#[test]
fn host_platform_matches_the_build_target() {
    let expected = if cfg!(target_os = "macos") {
        HostPlatform::MacOs
    } else if cfg!(target_os = "linux") {
        HostPlatform::Linux
    } else if cfg!(windows) {
        HostPlatform::Windows
    } else {
        HostPlatform::Other
    };
    assert_eq!(at_fs::host_platform(), expected);
}
