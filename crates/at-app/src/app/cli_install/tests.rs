//! Behaviour tests for the packaged CLI installer.

use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    bundled: PathBuf,
    home: PathBuf,
    primary: PathBuf,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().to_path_buf();
    let bundle_bin = root.join("bundle");
    std::fs::create_dir_all(&bundle_bin).expect("bundle dir");
    let bundled = write_executable(&bundle_bin.join(CLI_NAME), env!("CARGO_PKG_VERSION"));
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let primary = root.join("usr-local-bin");
    std::fs::create_dir_all(&primary).expect("primary");
    Fixture {
        _temp: temp,
        root,
        bundled,
        home,
        primary,
    }
}

fn write_executable(path: &Path, version: &str) -> PathBuf {
    at_fs::testkit::write_executable(path, format!("#!/bin/sh\necho \"agentuna {version}\"\n"))
        .expect("write script");
    path.to_path_buf()
}

fn installer(fixture: &Fixture) -> CliInstaller {
    CliInstaller::new(&fixture.bundled, &fixture.home)
        .with_primary_dir(&fixture.primary)
        .with_path_var(Some(OsString::from("/usr/bin:/bin")))
}

#[test]
fn install_creates_a_symlink_to_the_bundled_binary_and_reports_it() {
    let fixture = fixture();
    let installer = installer(&fixture);
    let link = fixture.primary.join(CLI_NAME);

    let report = installer
        .install(CliInstallLocation::Primary)
        .expect("install");

    assert!(report.created);
    assert_eq!(report.link_path, link.display().to_string());
    assert_eq!(
        report.target,
        std::fs::canonicalize(&fixture.bundled)
            .expect("canonicalize")
            .display()
            .to_string()
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).expect("read link"),
        fixture.bundled
    );

    let expected_version = format!("agentuna {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report.reported_version.as_deref(),
        Some(expected_version.as_str())
    );
    assert!(report.matches_app_version);

    let status = installer.status();
    let installed = status.installed.expect("reported as installed");
    assert_eq!(installed.link_path, link.display().to_string());

    // A shell whose PATH contains the install directory resolves the command.
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("command -v agentuna")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fixture.primary.display()),
        )
        .output()
        .expect("run shell");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        link.display().to_string()
    );
}

#[test]
fn repeated_install_is_idempotent() {
    let fixture = fixture();
    let installer = installer(&fixture);
    installer
        .install(CliInstallLocation::Primary)
        .expect("first install");
    let second = installer
        .install(CliInstallLocation::Primary)
        .expect("second install");
    assert!(!second.created);
    assert!(second.matches_app_version);
}

#[test]
fn install_never_overwrites_a_foreign_file() {
    let fixture = fixture();
    let installer = installer(&fixture);
    let link = fixture.primary.join(CLI_NAME);
    std::fs::write(&link, "someone else's tool").expect("write foreign file");

    let error = installer
        .install(CliInstallLocation::Primary)
        .expect_err("must refuse");
    assert!(matches!(&error, CliInstallError::ForeignFile { path } if path == &link));
    assert_eq!(
        std::fs::read_to_string(&link).expect("read foreign file"),
        "someone else's tool"
    );
}

#[test]
fn install_never_replaces_a_foreign_symlink() {
    let fixture = fixture();
    let installer = installer(&fixture);
    let link = fixture.primary.join(CLI_NAME);
    let other = fixture.root.join("other-tool");
    write_executable(&other, "9.9.9");
    symlink(&other, &link).expect("foreign symlink");

    let error = installer
        .install(CliInstallLocation::Primary)
        .expect_err("must refuse");
    assert!(matches!(
        &error,
        CliInstallError::ForeignSymlink { path, target }
            if path == &link && target == &other
    ));
    assert_eq!(std::fs::read_link(&link).expect("read link"), other);
}

#[test]
fn unwritable_primary_offers_the_fallback_with_the_exact_export_line() {
    let fixture = fixture();
    std::fs::set_permissions(&fixture.primary, std::fs::Permissions::from_mode(0o500))
        .expect("chmod primary");

    // A process that ignores permission bits (for example root) cannot exercise this.
    if std::fs::write(fixture.primary.join(".probe"), b"x").is_ok() {
        let _ = std::fs::remove_file(fixture.primary.join(".probe"));
        std::fs::set_permissions(&fixture.primary, std::fs::Permissions::from_mode(0o700))
            .expect("restore");
        eprintln!("permissions do not restrict this process; skipping");
        return;
    }

    let installer = installer(&fixture);
    let status = installer.status();
    assert!(!status.primary.writable);
    assert!(!status.fallback.on_path);
    assert_eq!(
        status.fallback.path_export.as_deref(),
        Some("export PATH=\"$HOME/.local/bin:$PATH\"")
    );
    assert!(status.fallback.writable);
    assert_eq!(
        status.fallback.directory,
        fixture.home.join(".local/bin").display().to_string()
    );

    let report = installer
        .install(CliInstallLocation::Fallback)
        .expect("fallback install");
    let link = fixture.home.join(".local/bin").join(CLI_NAME);
    assert!(report.created);
    assert_eq!(report.link_path, link.display().to_string());
    assert!(link.is_symlink());

    std::fs::set_permissions(&fixture.primary, std::fs::Permissions::from_mode(0o700))
        .expect("restore");
}

#[test]
fn path_export_is_omitted_when_the_directory_is_already_on_path() {
    let fixture = fixture();
    let installer = CliInstaller::new(&fixture.bundled, &fixture.home)
        .with_primary_dir(&fixture.primary)
        .with_path_var(Some(OsString::from(format!(
            "{}:/usr/bin:/bin",
            fixture.primary.display()
        ))));
    let status = installer.status();
    assert!(status.primary.on_path);
    assert_eq!(status.primary.path_export, None);
    assert_eq!(
        status.fallback.path_export.as_deref(),
        Some("export PATH=\"$HOME/.local/bin:$PATH\"")
    );
}

#[test]
fn uninstall_removes_only_a_link_that_points_at_this_bundle() {
    let fixture = fixture();
    let installer = installer(&fixture);
    installer
        .install(CliInstallLocation::Primary)
        .expect("install");
    let link = fixture.primary.join(CLI_NAME);

    let report = installer.uninstall(Some(CliInstallLocation::Primary));
    assert!(report.removed);
    assert_eq!(report.warning, None);
    assert!(!link.exists());
}

#[test]
fn uninstall_leaves_foreign_symlinks_and_files_alone() {
    let fixture = fixture();
    let installer = installer(&fixture);
    let link = fixture.primary.join(CLI_NAME);
    let other = fixture.root.join("other-tool");
    write_executable(&other, "9.9.9");
    symlink(&other, &link).expect("foreign symlink");

    let report = installer.uninstall(Some(CliInstallLocation::Primary));
    assert!(!report.removed);
    let warning = report.warning.expect("warning");
    assert!(warning.contains(&link.display().to_string()), "{warning}");
    assert!(link.is_symlink());

    std::fs::remove_file(&link).expect("remove symlink");
    std::fs::write(&link, "foreign").expect("write file");
    let report = installer.uninstall(Some(CliInstallLocation::Primary));
    assert!(!report.removed);
    assert!(report.warning.expect("warning").contains("not a symlink"));
    assert_eq!(std::fs::read_to_string(&link).expect("read"), "foreign");
}

#[test]
fn missing_bundled_binary_names_the_offending_path() {
    let fixture = fixture();
    let missing = fixture.root.join("missing").join(CLI_NAME);
    let installer = CliInstaller::new(&missing, &fixture.home)
        .with_primary_dir(&fixture.primary)
        .with_path_var(None);

    let error = installer
        .install(CliInstallLocation::Primary)
        .expect_err("must fail");
    assert!(matches!(
        &error,
        CliInstallError::BundledBinaryMissing { path } if path == &missing
    ));
}
