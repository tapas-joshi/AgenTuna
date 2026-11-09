//! Acceptance tests for `AppPaths`: every root, override order and directory modes.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use at_store::PortablePlatform;
use at_store::default_global_store;
use at_store::paths::{AppPaths, GlobalStoreSource, PathEnvironment, PathError, PathOverrides};

fn injected(home: &str) -> PathEnvironment {
    PathEnvironment::default().with_home(home)
}

fn resolve(environment: &PathEnvironment) -> AppPaths {
    AppPaths::resolve(environment).expect("paths resolve")
}

fn modes() -> impl Fn(&Path) -> u32 {
    |path: &Path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            0
        }
    }
}

#[test]
#[cfg(target_os = "macos")]
fn macos_snapshot_of_every_root() {
    let paths = resolve(&injected("/Users/tester"));
    let support = Path::new("/Users/tester/Library/Application Support/AgenTuna");
    assert_eq!(paths.home_dir(), Some(Path::new("/Users/tester")));
    assert_eq!(paths.support_dir(), support);
    assert_eq!(paths.db_path(), support.join("agentuna.db"));
    assert_eq!(paths.log_dir(), support.join("logs"));
    assert_eq!(paths.backup_dir(), support.join("backups"));
    assert_eq!(paths.cache_dir(), support.join("cache"));
    assert_eq!(paths.user_store(), Path::new("/Users/tester/.agentuna"));
    assert_eq!(paths.project_store(), None);
    assert_eq!(
        paths.global_store(),
        Path::new("/Library/Application Support/AgenTuna/global")
    );
    assert_eq!(paths.global_store_source(), GlobalStoreSource::Default);
}

#[test]
fn global_store_default_is_platform_specific_and_machine_wide() {
    for (platform, expected) in [
        (
            PortablePlatform::MacOs,
            "/Library/Application Support/AgenTuna/global",
        ),
        (PortablePlatform::Linux, "/var/lib/agentuna/global"),
        (PortablePlatform::Windows, r"C:\ProgramData\AgenTuna\global"),
    ] {
        assert_eq!(
            default_global_store(platform),
            expected,
            "the Global store default for {platform:?}"
        );
    }
    let platform = PortablePlatform::current();
    let expected = Path::new(default_global_store(platform));
    assert!(
        expected.is_absolute(),
        "the Global default must be absolute: {expected:?}"
    );
    assert!(
        !expected.starts_with("/Users/tester"),
        "the Global default must not live under the user's home: {expected:?}"
    );
    let paths = resolve(&injected("/Users/tester"));
    assert_eq!(paths.global_store(), expected);
}

#[test]
fn agentuna_home_relocates_support_files_and_wins_over_project_dirs() {
    let overrides = PathOverrides::from_lookup(|name| match name {
        "AGENTUNA_HOME" => Some(OsString::from("/sandbox/support")),
        _ => None,
    });
    let paths = resolve(&injected("/Users/tester").with_overrides(overrides));
    let support = Path::new("/sandbox/support");
    assert_eq!(paths.support_dir(), support);
    assert_eq!(paths.db_path(), support.join("agentuna.db"));
    assert_eq!(paths.log_dir(), support.join("logs"));
    assert_eq!(paths.backup_dir(), support.join("backups"));
    assert_eq!(paths.cache_dir(), support.join("cache"));
    // The user store follows home, not AGENTUNA_HOME.
    assert_eq!(paths.user_store(), Path::new("/Users/tester/.agentuna"));
}

#[test]
fn user_store_override_wins_over_home() {
    let overrides = PathOverrides::from_lookup(|name| match name {
        "AGENTUNA_USER_STORE" => Some(OsString::from("/sandbox/user-store")),
        _ => None,
    });
    let paths = resolve(&injected("/Users/tester").with_overrides(overrides));
    assert_eq!(paths.user_store(), Path::new("/sandbox/user-store"));
}

#[test]
fn empty_values_are_not_overrides() {
    let overrides = PathOverrides::from_lookup(|_| Some(OsString::new()));
    assert_eq!(overrides, PathOverrides::default());
    let paths = resolve(&injected("/Users/tester").with_overrides(overrides));
    let default_paths = resolve(&injected("/Users/tester"));
    assert_eq!(paths.support_dir(), default_paths.support_dir());
}

#[test]
fn global_store_resolution_is_environment_then_settings_then_default() {
    let environment = PathOverrides::from_lookup(|name| match name {
        "AGENTUNA_GLOBAL_STORE" => Some(OsString::from("/sandbox/global")),
        _ => None,
    });
    let mut paths = resolve(&injected("/Users/tester").with_overrides(environment));
    assert_eq!(paths.global_store(), Path::new("/sandbox/global"));
    assert_eq!(paths.global_store_source(), GlobalStoreSource::Environment);
    // The environment wins over settings, and the pass reports no change.
    assert!(!paths.apply_settings_global_store(Some(Path::new("/settings/global"))));
    assert_eq!(paths.global_store(), Path::new("/sandbox/global"));
    assert_eq!(paths.global_store_source(), GlobalStoreSource::Environment);

    let mut paths = resolve(&injected("/Users/tester"));
    let support = paths.support_dir().to_path_buf();
    assert!(paths.apply_settings_global_store(Some(Path::new("custom-global"))));
    assert_eq!(paths.global_store(), support.join("custom-global"));
    assert_eq!(paths.global_store_source(), GlobalStoreSource::Settings);
    // Repeating the same value is a no-op.
    assert!(!paths.apply_settings_global_store(Some(Path::new("custom-global"))));

    assert!(paths.apply_settings_global_store(Some(Path::new("/abs/global"))));
    assert_eq!(paths.global_store(), Path::new("/abs/global"));

    // Clearing the setting falls back to the documented default.
    assert!(paths.apply_settings_global_store(None));
    assert_eq!(
        paths.global_store(),
        Path::new(default_global_store(PortablePlatform::current()))
    );
    assert_eq!(paths.global_store_source(), GlobalStoreSource::Default);
    assert!(!paths.apply_settings_global_store(None));
}

#[test]
fn project_store_requires_an_existing_project_directory() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let project = sandbox.path().join("repo");
    fs::create_dir(&project).expect("project dir");

    let paths = resolve(&injected("/Users/tester").with_project_root(&project));
    let expected = project.join(".agentuna");
    assert_eq!(paths.project_store(), Some(expected.as_path()));

    let missing = sandbox.path().join("missing");
    let error = AppPaths::resolve(&injected("/Users/tester").with_project_root(&missing))
        .expect_err("missing project root");
    assert!(matches!(error, PathError::ProjectRootMissing { .. }));

    let file = sandbox.path().join("file.txt");
    fs::write(&file, b"not a directory").expect("file");
    let error = AppPaths::resolve(&injected("/Users/tester").with_project_root(&file))
        .expect_err("file project root");
    assert!(matches!(error, PathError::ProjectRootNotDirectory { .. }));
}

#[test]
fn project_store_override_wins_over_the_project_default() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let project = sandbox.path().join("repo");
    fs::create_dir(&project).expect("project dir");
    let overrides = PathOverrides {
        project_store: Some(PathBuf::from("/sandbox/project-store")),
        ..PathOverrides::default()
    };
    let paths = resolve(
        &injected("/Users/tester")
            .with_project_root(&project)
            .with_overrides(overrides),
    );
    assert_eq!(
        paths.project_store(),
        Some(Path::new("/sandbox/project-store"))
    );
}

#[test]
fn a_project_store_override_needs_a_project_root() {
    let overrides = PathOverrides {
        project_store: Some(PathBuf::from("/sandbox/project-store")),
        ..PathOverrides::default()
    };
    let error = AppPaths::resolve(&injected("/Users/tester").with_overrides(overrides))
        .expect_err("override without a project root");
    assert!(matches!(error, PathError::ProjectRootRequired { .. }));
}

#[test]
fn missing_home_is_a_typed_error_not_a_panic() {
    let error = AppPaths::resolve(&PathEnvironment::default().without_home())
        .expect_err("no home, no overrides");
    assert!(matches!(error, PathError::HomeUnavailable));
    assert!(!error.to_string().is_empty());

    // Explicit overrides make a home-less process workable.
    let overrides = PathOverrides {
        home: Some(PathBuf::from("/sandbox/support")),
        user_store: Some(PathBuf::from("/sandbox/user")),
        ..PathOverrides::default()
    };
    let paths = resolve(
        &PathEnvironment::default()
            .without_home()
            .with_overrides(overrides),
    );
    assert_eq!(paths.home_dir(), None);
    assert_eq!(paths.support_dir(), Path::new("/sandbox/support"));
    assert_eq!(paths.user_store(), Path::new("/sandbox/user"));

    // A support override alone is not enough: the user store still needs a home.
    let overrides = PathOverrides {
        home: Some(PathBuf::from("/sandbox/support")),
        ..PathOverrides::default()
    };
    let error = AppPaths::resolve(
        &PathEnvironment::default()
            .without_home()
            .with_overrides(overrides),
    )
    .expect_err("user store needs a home");
    assert!(matches!(error, PathError::HomeUnavailable));
}

#[test]
fn create_dirs_makes_every_directory_with_the_documented_modes() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let project = sandbox.path().join("repo");
    fs::create_dir(&project).expect("project dir");
    let paths = resolve(
        &injected(sandbox.path().to_str().expect("utf-8 temp path")).with_project_root(&project),
    );

    paths.create_dirs().expect("create directories");

    let mode = modes();
    for directory in [
        paths.support_dir(),
        paths.log_dir(),
        paths.backup_dir(),
        paths.cache_dir(),
        paths.user_store(),
    ] {
        assert!(directory.is_dir(), "{} missing", directory.display());
        if cfg!(unix) {
            assert_eq!(mode(directory), 0o700, "{} mode", directory.display());
        }
    }
    let project_store = paths.project_store().expect("project store");
    assert!(project_store.is_dir());
    if cfg!(unix) {
        assert_eq!(mode(project_store), 0o755);
    }
    // Creating twice is fine.
    paths.create_dirs().expect("idempotent create");
}

/// A caller-supplied home governs every root, whatever the process environment says.
///
/// Linux derives the support, state and cache trees from `XDG_*` (`%APPDATA%` and
/// `%LOCALAPPDATA%` on Windows). Reading those from the process while a home is injected put a
/// sandbox's database, logs and backups outside the sandbox; GitHub's Linux runner hit exactly
/// that when a sibling test's environment scope pointed the process at another sandbox mid-run.
#[test]
fn an_injected_home_outranks_process_platform_roots() {
    let outside = "/outside/the-sandbox";
    let _environment = env_lock::lock_env(
        [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
        ]
        .map(|name| (name, Some(outside))),
    );
    assert_eq!(
        std::env::var("XDG_CONFIG_HOME").as_deref(),
        Ok(outside),
        "the guard points the process away from the sandbox"
    );

    let sandbox = tempfile::tempdir().expect("temp dir");
    let home = sandbox.path().to_str().expect("utf-8 temp path");
    let paths = resolve(&injected(home));
    for root in [
        paths.support_dir(),
        paths.local_state_dir(),
        paths.cache_dir(),
    ] {
        assert!(
            root.starts_with(home),
            "{} must stay under the injected home {home}",
            root.display()
        );
    }

    // An explicit lookup still honours the variables: that is how a foreign layout is asserted.
    let linux = AppPaths::resolve_for(PortablePlatform::Linux, &injected(home), |name| {
        (name == "XDG_CONFIG_HOME").then(|| OsString::from(outside))
    })
    .expect("linux layout");
    assert_eq!(linux.support_dir(), Path::new(outside).join("agentuna"));
}
