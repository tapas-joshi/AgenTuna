//! Acceptance tests for skill packages: round-trip, path safety, limits, guard, crash safety.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use at_fs::{Remedy, TargetKind};
use at_model::Scope;
use at_store::layout::{Slug, StoreLayout, init_store};
use at_store::skill_package::{
    SKILL_PACKAGE_MAX_BYTES, SKILL_PACKAGE_MAX_FILES, SkillPackage, SkillPackageError,
};
use tempfile::TempDir;

fn initialized() -> (TempDir, StoreLayout) {
    let dir = TempDir::new().expect("temp dir");
    let layout = StoreLayout::new(Scope::User, dir.path().join("store"));
    init_store(Scope::User, layout.root()).expect("init");
    (dir, layout)
}

fn demo_package() -> SkillPackage {
    let mut package = SkillPackage::new();
    package
        .insert(
            "SKILL.md",
            b"---\nname: demo\ndescription: test\n---\nbody\n".to_vec(),
            0o644,
        )
        .expect("SKILL.md");
    package
        .insert("scripts/x.sh", b"#!/bin/sh\necho hi\n".to_vec(), 0o755)
        .expect("script");
    package
        .insert("references/a.md", b"# Ref\n".to_vec(), 0o644)
        .expect("ref");
    package
}

#[test]
fn round_trip_preserves_bytes_and_permissions() {
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let package = demo_package();

    let outcome = package.write(&layout, &slug, None, None).expect("write");
    assert!(!outcome.replaced);

    let target = layout.skill_dir(&slug);
    assert!(target.join("SKILL.md").is_file());
    assert!(target.join("scripts/x.sh").is_file());
    assert!(target.join("references/a.md").is_file());

    // Bytes preserved.
    assert_eq!(
        fs::read(target.join("SKILL.md")).expect("read"),
        b"---\nname: demo\ndescription: test\n---\nbody\n"
    );
    assert_eq!(
        fs::read(target.join("scripts/x.sh")).expect("read"),
        b"#!/bin/sh\necho hi\n"
    );
    assert_eq!(
        fs::read(target.join("references/a.md")).expect("read"),
        b"# Ref\n"
    );

    // Permissions preserved.
    let script_mode = fs::metadata(target.join("scripts/x.sh"))
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(script_mode, 0o755, "script keeps executable bit");
    let skill_mode = fs::metadata(target.join("SKILL.md"))
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(skill_mode, 0o644, "SKILL.md keeps 0644");

    // Read back through the package API.
    let loaded = SkillPackage::read(&target).expect("read package");
    assert_eq!(loaded.package.len(), 3);
    assert_eq!(
        loaded.package.get("scripts/x.sh").expect("script").mode & 0o777,
        0o755
    );
}

#[test]
fn dotdot_and_absolute_paths_rejected() {
    let mut package = SkillPackage::new();
    package
        .insert("SKILL.md", b"x".to_vec(), 0o644)
        .expect("SKILL.md");
    // `..` and absolute paths return InvalidPackage.
    let err = package.insert("../escape", vec![], 0o644).expect_err("..");
    assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
    let err = package.insert("/abs/path", vec![], 0o644).expect_err("abs");
    assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
    let err = package
        .insert("a/../b", vec![], 0o644)
        .expect_err("nested ..");
    assert!(matches!(err, SkillPackageError::InvalidPackage { .. }));
    // Other invalid paths return InvalidPath.
    assert!(package.insert("a//b", vec![], 0o644).is_err());
    assert!(package.insert("a\\b", vec![], 0o644).is_err());
    assert!(package.insert("", vec![], 0o644).is_err());
}

#[test]
fn symlink_pointing_outside_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let skill = dir.path().join("skill");
    fs::create_dir_all(&skill).expect("skill dir");
    fs::write(skill.join("SKILL.md"), b"---\nname: x\n---\n").expect("SKILL.md");
    // Symlink to outside the package.
    std::os::unix::fs::symlink("/etc/passwd", skill.join("link")).expect("symlink");

    let error = SkillPackage::read(&skill).expect_err("symlink rejected");
    assert!(matches!(error, SkillPackageError::InvalidPackage { .. }));
}

#[test]
fn symlink_to_directory_outside_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let skill = dir.path().join("skill");
    fs::create_dir_all(&skill).expect("skill dir");
    fs::write(skill.join("SKILL.md"), b"---\nname: x\n---\n").expect("SKILL.md");
    // Symlink to a directory outside.
    std::os::unix::fs::symlink("/tmp", skill.join("dirlink")).expect("symlink");

    let error = SkillPackage::read(&skill).expect_err("symlink rejected");
    assert!(matches!(error, SkillPackageError::InvalidPackage { .. }));
}

#[test]
fn too_many_files_rejected() {
    let mut package = SkillPackage::new();
    package
        .insert("SKILL.md", b"x".to_vec(), 0o644)
        .expect("SKILL.md");
    for i in 0..SKILL_PACKAGE_MAX_FILES {
        package
            .insert(format!("f{i}"), vec![], 0o644)
            .expect("file");
    }
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let error = package
        .write(&layout, &slug, None, None)
        .expect_err("too many files");
    assert!(matches!(error, SkillPackageError::TooManyFiles { .. }));
    assert!(
        error
            .to_string()
            .contains(&SKILL_PACKAGE_MAX_FILES.to_string()),
        "error names the limit: {error}"
    );
}

#[test]
fn too_large_rejected() {
    let mut package = SkillPackage::new();
    package
        .insert(
            "SKILL.md",
            vec![0u8; SKILL_PACKAGE_MAX_BYTES as usize + 1],
            0o644,
        )
        .expect("SKILL.md");
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let error = package
        .write(&layout, &slug, None, None)
        .expect_err("too large");
    assert!(matches!(
        error,
        SkillPackageError::FileTooLarge { .. } | SkillPackageError::TooLarge { .. }
    ));
    assert!(
        error
            .to_string()
            .contains(&SKILL_PACKAGE_MAX_BYTES.to_string())
            || error.to_string().contains("limit"),
        "error names the limit: {error}"
    );
}

#[test]
fn overwrite_owned_backs_up_first() {
    let (dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let package = demo_package();

    // First write: create.
    let first = package
        .write(&layout, &slug, None, None)
        .expect("first write");
    assert!(!first.replaced);

    // Second write with backup dir.
    let backup_dir = dir.path().join("backups/op1");
    fs::create_dir_all(&backup_dir).expect("backup dir");
    let second = package
        .write(&layout, &slug, Some(&backup_dir), None)
        .expect("second write");
    assert!(second.replaced, "existing folder was replaced");
    assert!(
        second.backup_path.is_some(),
        "backup was taken for the overwrite"
    );
    let backup = second.backup_path.expect("backup path");
    assert!(backup.join("SKILL.md").is_file(), "backup holds SKILL.md");
    assert!(
        backup.join("scripts/x.sh").is_file(),
        "backup holds scripts/x.sh"
    );
}

#[test]
fn overwrite_foreign_folder_fails_with_absolute_path() {
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let target = layout.skill_dir(&slug);
    fs::create_dir_all(&target).expect("target dir");
    fs::write(target.join("SKILL.md"), b"foreign\n").expect("foreign file");
    fs::write(target.join("other.txt"), b"foreign data\n").expect("foreign file");

    let package = demo_package();

    #[derive(Debug)]
    struct Refuse;
    impl at_fs::OverwriteGuard for Refuse {
        fn check(&self, path: &Path, _allow_empty_dir: bool) -> Result<(), at_fs::FsError> {
            Err(at_fs::FsError::RefusedOverwrite {
                path: path.to_path_buf(),
                kind: TargetKind::Dir,
                existing_hash: None,
                remedies: vec![Remedy::Adopt, Remedy::MoveAside, Remedy::Cancel],
            })
        }
    }

    let error = package
        .write(&layout, &slug, None, Some(&Refuse))
        .expect_err("foreign folder refused");
    assert!(matches!(error, SkillPackageError::RefusedOverwrite { .. }));
    // The error carries the absolute path.
    let message = error.to_string();
    assert!(
        message.contains(&target.display().to_string()),
        "error names absolute path: {message}"
    );
    // Foreign files untouched.
    assert_eq!(
        fs::read_to_string(target.join("SKILL.md")).expect("read"),
        "foreign\n"
    );
    assert_eq!(
        fs::read_to_string(target.join("other.txt")).expect("read"),
        "foreign data\n"
    );
}

#[test]
fn crash_leaves_no_partial_folder() {
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let skills = layout.skills_dir();
    fs::create_dir_all(&skills).expect("skills dir");

    // Simulate crash residue: staging dir exists, target does not.
    let staging = skills.join(".demo.agentuna-tmp-99999-0");
    fs::create_dir_all(staging.join("scripts")).expect("staging");
    fs::write(staging.join("SKILL.md"), b"partial\n").expect("partial");
    assert!(staging.exists());
    assert!(!layout.skill_dir(&slug).exists());

    // A real write succeeds and produces a complete folder.
    let package = demo_package();
    let outcome = package.write(&layout, &slug, None, None).expect("write");
    assert!(outcome.path.is_dir());
    assert!(outcome.path.join("SKILL.md").is_file());
    assert!(outcome.path.join("scripts/x.sh").is_file());
    assert!(outcome.path.join("references/a.md").is_file());

    // The simulated crash residue remains (expected), but the writer's own
    // staging dir is gone and no .old aside remains.
    assert!(staging.exists(), "crash residue remains");
    for entry in fs::read_dir(&skills).expect("skills") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        if name == staging.file_name().expect("name").to_string_lossy() {
            continue;
        }
        assert!(!name.contains(".agentuna-tmp-"), "staging left: {name}");
        assert!(!name.contains(".agentuna-old-"), "aside left: {name}");
    }
}

#[test]
fn missing_skill_md_rejected() {
    let package = SkillPackage::new();
    let (_dir, layout) = initialized();
    let slug = Slug::parse("demo").expect("slug");
    let error = package
        .write(&layout, &slug, None, None)
        .expect_err("missing SKILL.md");
    assert!(matches!(error, SkillPackageError::MissingSkillDoc { .. }));
}

#[test]
fn read_missing_skill_md_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let skill = dir.path().join("skill");
    fs::create_dir_all(&skill).expect("skill dir");
    fs::write(skill.join("other.txt"), b"no skill doc\n").expect("file");

    let error = SkillPackage::read(&skill).expect_err("missing SKILL.md");
    assert!(matches!(error, SkillPackageError::MissingSkillDoc { .. }));
}
