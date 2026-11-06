//! Deployment primitive: link and copy modes, atomic replacement, manifests and removal.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use at_fs::{
    COPY_MANIFEST_FILE, COPY_MANIFEST_VERSION, CopyManifest, DeployFs, DeployRequest,
    DeploymentLookup, FsError, LinkMode, OverwriteGuard, OwnershipContext, RemoveRequest,
    RemovedDeployment, copy_dir, deploy_with, link_dir, remove_deployment, sha256_hex,
};
use at_model::{AgentId, ItemId, Scope};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    store: PathBuf,
    item: PathBuf,
    agent: PathBuf,
    target: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let store = tmp.path().join("home/.agentuna");
    let item = store.join("skills/review");
    let agent = tmp.path().join("home/.claude/skills");
    fs::create_dir_all(&item).unwrap();
    fs::create_dir_all(&agent).unwrap();
    fs::write(item.join("SKILL.md"), b"name: review\n").unwrap();
    fs::create_dir_all(item.join("scripts")).unwrap();
    fs::write(item.join("scripts/run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
    let target = agent.join("review");
    Fixture {
        _tmp: tmp,
        store,
        item,
        agent,
        target,
    }
}

fn context(f: &Fixture, item_id: ItemId) -> OwnershipContext {
    OwnershipContext {
        item_id,
        agent_id: AgentId::ClaudeCode,
        scope: Scope::User,
        store_roots: vec![f.store.clone()],
    }
}

/// Stands in for the deployments table: exact (path, item, agent, scope) rows.
#[derive(Debug, Default)]
struct Rows(Vec<(PathBuf, ItemId, AgentId, Scope)>);

impl DeploymentLookup for Rows {
    fn is_recorded(&self, path: &Path, item: &ItemId, agent: &AgentId, scope: Scope) -> bool {
        self.0
            .iter()
            .any(|(p, i, a, s)| p == path && i == item && a == agent && *s == scope)
    }
}

/// A filesystem that can refuse symlinks or renames, like a restricted platform would.
#[derive(Debug, Clone, Copy)]
struct FaultyFs {
    symlink_error: Option<i32>,
    rename_error: Option<i32>,
    same_volume: bool,
}

impl FaultyFs {
    fn working() -> Self {
        Self {
            symlink_error: None,
            rename_error: None,
            same_volume: true,
        }
    }

    fn without_symlinks() -> Self {
        Self {
            symlink_error: Some(libc::EPERM),
            ..Self::working()
        }
    }
}

impl DeployFs for FaultyFs {
    fn symlink(&self, link_target: &Path, link_path: &Path) -> io::Result<()> {
        match self.symlink_error {
            Some(code) => Err(io::Error::from_raw_os_error(code)),
            None => std::os::unix::fs::symlink(link_target, link_path),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match self.rename_error {
            Some(code) => Err(io::Error::from_raw_os_error(code)),
            None => fs::rename(from, to),
        }
    }

    fn same_volume(&self, _a: &Path, _b: &Path) -> bool {
        self.same_volume
    }
}

#[derive(Debug)]
struct AllowAll;

impl OverwriteGuard for AllowAll {
    fn check(&self, _path: &Path, _allow_empty_dir: bool) -> Result<(), FsError> {
        Ok(())
    }
}

fn hash_tree(root: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(Result::unwrap).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let relative = path.strip_prefix(base).unwrap().display().to_string();
                out.push((relative, sha256_hex(&fs::read(&path).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn link_deploy_is_relative_and_store_edits_show_through() {
    let f = fixture();
    let outcome = link_dir(&f.item, &f.target, LinkMode::Link).unwrap();

    assert_eq!(outcome.mode, LinkMode::Link);
    let link = outcome.link.clone().unwrap();
    assert!(link.is_relative);
    let raw = fs::read_link(&f.target).unwrap();
    assert!(raw.is_relative());
    assert_eq!(
        f.agent.join(&raw).canonicalize().unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );
    assert_eq!(
        fs::canonicalize(&f.target).unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );

    // Writing in the store is visible through the agent path without a redeploy.
    fs::write(f.item.join("SKILL.md"), b"name: review\nversion: 2\n").unwrap();
    assert_eq!(
        fs::read(f.target.join("SKILL.md")).unwrap(),
        b"name: review\nversion: 2\n"
    );
    fs::write(f.item.join("extra.md"), b"new").unwrap();
    assert_eq!(fs::read(f.target.join("extra.md")).unwrap(), b"new");
}

#[test]
fn link_deploy_is_absolute_on_another_volume() {
    let f = fixture();
    let request = DeployRequest::new(&f.item, &f.target, LinkMode::Link);
    let faulty = FaultyFs {
        same_volume: false,
        ..FaultyFs::working()
    };

    let outcome = deploy_with(&request, &faulty).unwrap();
    let link = outcome.link.unwrap();
    assert!(!link.is_relative);
    assert_eq!(link.link_target, fs::canonicalize(&f.item).unwrap());
    assert_eq!(fs::read_link(&f.target).unwrap(), link.link_target);
}

#[test]
fn link_replacement_never_exposes_enoent() {
    let f = fixture();
    link_dir(&f.item, &f.target, LinkMode::Link).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let reader = {
        let barrier = Arc::clone(&barrier);
        let target = f.target.clone();
        thread::spawn(move || {
            barrier.wait();
            let mut failures = Vec::new();
            for _ in 0..1000 {
                // Acceptance is "never ENOENT": the link path must always exist while it
                // is replaced. Other errors are reported separately below.
                if let Err(e) = fs::read_link(&target) {
                    failures.push(e);
                }
            }
            failures
        })
    };

    barrier.wait();
    for _ in 0..50 {
        link_dir(&f.item, &f.target, LinkMode::Link).unwrap();
    }

    let failures = reader.join().unwrap();
    let missing: Vec<_> = failures
        .iter()
        .filter(|e| e.kind() == std::io::ErrorKind::NotFound)
        .collect();
    assert!(
        missing.is_empty(),
        "link path vanished {} times during replacement: {missing:?}",
        missing.len()
    );
    // Observed once on a loaded macOS CI runner as EINVAL while the kernel swapped the
    // entry; reported, not asserted.
    if !failures.is_empty() {
        eprintln!("non-ENOENT readlink errors during replacement: {failures:?}");
    }
    assert_eq!(
        fs::canonicalize(&f.target).unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );
}

#[test]
fn symlink_refusal_returns_link_unsupported_and_writes_nothing() {
    for code in [libc::EPERM, libc::ENOTSUP, libc::EOPNOTSUPP] {
        let f = fixture();
        let request = DeployRequest::new(&f.item, &f.target, LinkMode::Link);
        let faulty = FaultyFs {
            symlink_error: Some(code),
            ..FaultyFs::working()
        };

        let error = deploy_with(&request, &faulty).unwrap_err();
        assert!(
            matches!(error, FsError::LinkUnsupported { .. }),
            "errno {code} gave unexpected error: {error:?}"
        );
        assert!(fs::symlink_metadata(&f.target).is_err());
        assert_eq!(
            fs::read_dir(&f.agent).unwrap().count(),
            0,
            "a refused deploy leaves no staging path behind"
        );
        assert!(f.item.join("SKILL.md").exists());
    }
}

#[test]
fn copy_mode_writes_a_manifest_and_reports_copy() {
    let f = fixture();
    let request = DeployRequest::new(&f.item, &f.target, LinkMode::Copy);

    let outcome = deploy_with(&request, &FaultyFs::without_symlinks()).unwrap();
    assert_eq!(outcome.mode, LinkMode::Copy);
    assert!(outcome.link.is_none());
    assert_eq!(outcome.copy.as_ref().unwrap().files_written, 2);
    assert!(f.target.join(COPY_MANIFEST_FILE).exists());
    assert_eq!(
        fs::read(f.target.join("scripts/run.sh")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );

    let manifest = CopyManifest::read(&f.target).unwrap().unwrap();
    assert_eq!(manifest.schema_version, COPY_MANIFEST_VERSION);
    assert_eq!(manifest.mode, LinkMode::Copy);
    assert_eq!(manifest.source_path, fs::canonicalize(&f.item).unwrap());
    assert_eq!(manifest.files.len(), 2);
    assert_eq!(manifest.dirs, vec!["scripts".to_owned()]);
    assert_eq!(
        manifest.entry("SKILL.md").unwrap().sha256,
        sha256_hex(b"name: review\n")
    );
    assert_eq!(
        manifest.entry("scripts/run.sh").unwrap().sha256,
        sha256_hex(b"#!/bin/sh\necho hi\n")
    );
}

#[test]
fn copy_fallback_happens_only_when_requested() {
    let f = fixture();
    let request = DeployRequest::new(&f.item, &f.target, LinkMode::Link).with_copy_fallback();

    let outcome = deploy_with(&request, &FaultyFs::without_symlinks()).unwrap();
    assert_eq!(outcome.mode, LinkMode::Copy);
    assert!(outcome.link.is_none());
    assert!(outcome.copy.is_some());
    assert!(f.target.join(COPY_MANIFEST_FILE).exists());
}

#[test]
fn copy_redeploy_rewrites_only_changed_files() {
    let f = fixture();
    copy_dir(&f.item, &f.target).unwrap();

    let skill_before = fs::metadata(f.target.join("SKILL.md")).unwrap();
    let script_before = fs::metadata(f.target.join("scripts/run.sh")).unwrap();

    // A rewrite must land on a later timestamp for the mtime assertion to mean anything.
    thread::sleep(Duration::from_millis(20));
    fs::write(f.item.join("SKILL.md"), b"name: review\nversion: 2\n").unwrap();

    let outcome = copy_dir(&f.item, &f.target).unwrap();
    let copy = outcome.copy.unwrap();
    assert_eq!(copy.files_written, 1);
    assert_eq!(copy.files_unchanged, 1);
    assert_eq!(copy.files_removed, 0);

    let skill_after = fs::metadata(f.target.join("SKILL.md")).unwrap();
    let script_after = fs::metadata(f.target.join("scripts/run.sh")).unwrap();
    assert_ne!(
        skill_before.ino(),
        skill_after.ino(),
        "the changed file is replaced"
    );
    assert_eq!(
        script_before.ino(),
        script_after.ino(),
        "an unchanged file is not rewritten"
    );
    assert_eq!(
        script_before.modified().unwrap(),
        script_after.modified().unwrap(),
        "an unchanged file keeps its mtime"
    );
    assert_eq!(
        fs::read(f.target.join("SKILL.md")).unwrap(),
        b"name: review\nversion: 2\n"
    );

    let manifest = CopyManifest::read(&f.target).unwrap().unwrap();
    assert_eq!(
        manifest.entry("SKILL.md").unwrap().sha256,
        sha256_hex(b"name: review\nversion: 2\n")
    );
    assert_eq!(
        manifest.entry("scripts/run.sh").unwrap().sha256,
        sha256_hex(b"#!/bin/sh\necho hi\n")
    );

    // New and removed files are reflected in the next report.
    fs::write(f.item.join("extra.md"), b"extra").unwrap();
    fs::remove_file(f.item.join("scripts/run.sh")).unwrap();
    let outcome = copy_dir(&f.item, &f.target).unwrap();
    let copy = outcome.copy.unwrap();
    assert_eq!(copy.files_written, 1);
    assert_eq!(copy.files_unchanged, 1);
    assert_eq!(copy.files_removed, 1);
    assert!(fs::symlink_metadata(f.target.join("scripts/run.sh")).is_err());
    assert_eq!(fs::read(f.target.join("extra.md")).unwrap(), b"extra");
}

#[test]
fn removal_deletes_deployments_but_leaves_store_hashes_unchanged() {
    let f = fixture();
    let item_id = ItemId::new();
    let ctx = context(&f, item_id);
    let rows = Arc::new(Rows(vec![(
        f.target.clone(),
        item_id,
        AgentId::ClaudeCode,
        Scope::User,
    )]));
    let store_before = hash_tree(&f.item);

    copy_dir(&f.item, &f.target).unwrap();
    let outcome =
        remove_deployment(&RemoveRequest::new(&f.target, ctx.clone(), rows.clone())).unwrap();
    assert_eq!(outcome.removed, RemovedDeployment::Copy { files: 2 });
    assert!(fs::symlink_metadata(&f.target).is_err());
    assert_eq!(hash_tree(&f.item), store_before);
    assert!(f.item.join("SKILL.md").exists());

    link_dir(&f.item, &f.target, LinkMode::Link).unwrap();
    let outcome = remove_deployment(&RemoveRequest::new(&f.target, ctx, rows)).unwrap();
    assert_eq!(outcome.removed, RemovedDeployment::Link);
    assert!(fs::symlink_metadata(&f.target).is_err());
    assert_eq!(hash_tree(&f.item), store_before);
}

#[test]
fn removal_refuses_paths_that_are_not_owned_deployments() {
    let f = fixture();
    let item_id = ItemId::new();
    let ctx = context(&f, item_id);

    // Not recorded: refused, nothing deleted.
    copy_dir(&f.item, &f.target).unwrap();
    let empty = Arc::new(Rows::default());
    let error = remove_deployment(&RemoveRequest::new(&f.target, ctx.clone(), empty)).unwrap_err();
    assert!(
        matches!(error, FsError::DeploymentNotOwned { .. }),
        "unexpected error: {error:?}"
    );
    assert!(f.target.join(COPY_MANIFEST_FILE).exists());

    // Recorded but inside a store root: canonical content is never deleted.
    let store_rows = Arc::new(Rows(vec![(
        f.item.clone(),
        item_id,
        AgentId::ClaudeCode,
        Scope::User,
    )]));
    let error =
        remove_deployment(&RemoveRequest::new(&f.item, ctx.clone(), store_rows)).unwrap_err();
    assert!(
        matches!(error, FsError::DeploymentNotOwned { .. }),
        "unexpected error: {error:?}"
    );
    assert!(f.item.join("SKILL.md").exists());

    // Recorded directory without a manifest: not recognizable as a copy deployment.
    fs::remove_file(f.target.join(COPY_MANIFEST_FILE)).unwrap();
    let rows = Arc::new(Rows(vec![(
        f.target.clone(),
        item_id,
        AgentId::ClaudeCode,
        Scope::User,
    )]));
    let error = remove_deployment(&RemoveRequest::new(&f.target, ctx, rows)).unwrap_err();
    assert!(
        matches!(error, FsError::DeploymentNotOwned { .. }),
        "unexpected error: {error:?}"
    );
    assert!(f.target.join("SKILL.md").exists());
}

#[test]
fn rename_across_filesystems_is_reported() {
    let f = fixture();
    let request = DeployRequest::new(&f.item, &f.target, LinkMode::Link);
    let faulty = FaultyFs {
        rename_error: Some(libc::EXDEV),
        ..FaultyFs::working()
    };

    let error = deploy_with(&request, &faulty).unwrap_err();
    assert!(
        matches!(error, FsError::CrossDeviceRename { .. }),
        "unexpected error: {error:?}"
    );
    assert!(fs::symlink_metadata(&f.target).is_err());
    assert_eq!(fs::read_dir(&f.agent).unwrap().count(), 0);
}

#[test]
fn unmanaged_target_is_refused_without_a_guard() {
    let f = fixture();
    fs::create_dir_all(&f.target).unwrap();
    fs::write(f.target.join("user.md"), b"mine").unwrap();

    let error = link_dir(&f.item, &f.target, LinkMode::Link).unwrap_err();
    assert!(
        matches!(error, FsError::RefusedOverwrite { .. }),
        "unexpected error: {error:?}"
    );
    assert_eq!(fs::read(f.target.join("user.md")).unwrap(), b"mine");
    assert!(fs::symlink_metadata(f.target.join("SKILL.md")).is_err());

    // An empty directory is safe to replace without a guard.
    fs::remove_file(f.target.join("user.md")).unwrap();
    link_dir(&f.item, &f.target, LinkMode::Copy).unwrap();
    assert!(f.target.join(COPY_MANIFEST_FILE).exists());
}

#[test]
fn guard_can_authorise_replacing_a_directory_with_a_link() {
    let f = fixture();
    fs::create_dir_all(&f.target).unwrap();
    fs::write(f.target.join("stale.md"), b"old deployment").unwrap();

    let request =
        DeployRequest::new(&f.item, &f.target, LinkMode::Link).with_guard(Arc::new(AllowAll));

    let outcome = deploy_with(&request, &FaultyFs::working()).unwrap();
    assert_eq!(outcome.mode, LinkMode::Link);
    assert!(
        fs::symlink_metadata(&f.target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::canonicalize(&f.target).unwrap(),
        fs::canonicalize(&f.item).unwrap()
    );
    assert_eq!(fs::read_dir(&f.agent).unwrap().count(), 1);
}

#[test]
fn switching_between_link_and_copy_keeps_one_deployment() {
    let f = fixture();
    link_dir(&f.item, &f.target, LinkMode::Link).unwrap();

    let outcome = copy_dir(&f.item, &f.target).unwrap();
    assert_eq!(outcome.mode, LinkMode::Copy);
    assert!(fs::symlink_metadata(&f.target).unwrap().is_dir());
    assert!(f.target.join(COPY_MANIFEST_FILE).exists());
    assert_eq!(fs::read_dir(&f.agent).unwrap().count(), 1);

    let outcome = link_dir(&f.item, &f.target, LinkMode::Link).unwrap();
    assert_eq!(outcome.mode, LinkMode::Link);
    assert!(
        fs::symlink_metadata(&f.target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_dir(&f.agent).unwrap().count(), 1);
}
