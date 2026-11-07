//! TF-037 and TF-039: `AppPaths` follows each platform's documented layout, with injected
//! home and root variables so nothing depends on the host's real environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use at_store::{PathError, PathKey, PlatformRootError, PortablePlatform};

/// A lookup over the given pairs only, so unset variables stay unset.
fn env(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<OsString> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

fn resolve_for(
    platform: PortablePlatform,
    environment: &PathEnvironment,
    pairs: &[(&str, &str)],
) -> AppPaths {
    AppPaths::resolve_for(platform, environment, env(pairs)).expect("paths resolve")
}

#[test]
fn linux_uses_the_xdg_directories_for_config_data_and_cache() {
    let paths = resolve_for(
        PortablePlatform::Linux,
        &PathEnvironment::default().with_home("/home/a"),
        &[
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_DATA_HOME", "/xdg/data"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
        ],
    );

    let config = Path::new("/xdg/config/agentuna");
    assert_eq!(paths.support_dir(), config);
    assert_eq!(paths.db_path(), config.join("agentuna.db"));
    assert_eq!(paths.log_dir(), config.join("logs"));
    assert_eq!(paths.backup_dir(), config.join("backups"));
    assert_eq!(paths.local_state_dir(), Path::new("/xdg/data/agentuna"));
    assert_eq!(paths.cache_dir(), Path::new("/xdg/cache/agentuna"));
    // CONTRACT decision 3: the User store is `~/.agentuna` on every platform, not an XDG root.
    assert_eq!(paths.user_store(), Path::new("/home/a/.agentuna"));
    assert_eq!(paths.home_dir(), Some(Path::new("/home/a")));
}

#[test]
fn linux_falls_back_to_the_home_dot_directories() {
    let paths = resolve_for(
        PortablePlatform::Linux,
        &PathEnvironment::default().with_home("/home/a"),
        &[],
    );

    assert_eq!(paths.support_dir(), Path::new("/home/a/.config/agentuna"));
    assert_eq!(
        paths.local_state_dir(),
        Path::new("/home/a/.local/share/agentuna")
    );
    assert_eq!(paths.cache_dir(), Path::new("/home/a/.cache/agentuna"));
    assert_eq!(paths.db_path(), paths.support_dir().join("agentuna.db"));
}

#[test]
fn linux_ignores_relative_and_empty_xdg_values() {
    let paths = resolve_for(
        PortablePlatform::Linux,
        &PathEnvironment::default().with_home("/home/a"),
        &[
            ("XDG_CONFIG_HOME", "relative/config"),
            ("XDG_DATA_HOME", ""),
            ("XDG_CACHE_HOME", "relative/cache"),
        ],
    );

    assert_eq!(paths.support_dir(), Path::new("/home/a/.config/agentuna"));
    assert_eq!(
        paths.local_state_dir(),
        Path::new("/home/a/.local/share/agentuna")
    );
    assert_eq!(paths.cache_dir(), Path::new("/home/a/.cache/agentuna"));
}

#[test]
fn windows_uses_appdata_and_localappdata() {
    let paths = resolve_for(
        PortablePlatform::Windows,
        &PathEnvironment::default().with_home(r"C:\Users\a"),
        &[
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\a\AppData\Local"),
        ],
    );

    assert_eq!(
        paths.support_dir(),
        Path::new(r"C:\Users\a\AppData\Roaming\AgenTuna")
    );
    assert_eq!(
        paths.db_path(),
        Path::new(r"C:\Users\a\AppData\Roaming\AgenTuna\agentuna.db")
    );
    assert_eq!(
        paths.log_dir(),
        Path::new(r"C:\Users\a\AppData\Roaming\AgenTuna\logs")
    );
    assert_eq!(
        paths.backup_dir(),
        Path::new(r"C:\Users\a\AppData\Roaming\AgenTuna\backups")
    );
    assert_eq!(
        paths.local_state_dir(),
        Path::new(r"C:\Users\a\AppData\Local\AgenTuna")
    );
    assert_eq!(
        paths.cache_dir(),
        Path::new(r"C:\Users\a\AppData\Local\AgenTuna\cache")
    );
    assert_eq!(paths.home_dir(), Some(Path::new(r"C:\Users\a")));
}

#[test]
fn windows_falls_back_to_userprofile_when_no_home_is_injected() {
    let paths = resolve_for(
        PortablePlatform::Windows,
        &PathEnvironment::default().without_home(),
        &[
            ("USERPROFILE", r"C:\Users\a"),
            ("APPDATA", r"C:\Users\a\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\a\AppData\Local"),
        ],
    );

    assert_eq!(paths.home_dir(), Some(Path::new(r"C:\Users\a")));
    assert_eq!(
        paths.user_store(),
        PathBuf::from(r"C:\Users\a\.agentuna").as_path()
    );
}

#[test]
fn windows_reports_a_missing_appdata_as_a_typed_error() {
    let error = AppPaths::resolve_for(
        PortablePlatform::Windows,
        &PathEnvironment::default().with_home(r"C:\Users\a"),
        env(&[("LOCALAPPDATA", r"C:\Users\a\AppData\Local")]),
    )
    .expect_err("APPDATA is required");
    assert!(matches!(
        error,
        PathError::PlatformRoot(PlatformRootError::MissingVariable {
            variable: "APPDATA"
        })
    ));
    assert!(!error.to_string().is_empty());
}

#[test]
fn a_missing_home_is_still_home_unavailable_on_every_platform() {
    for platform in [
        PortablePlatform::MacOs,
        PortablePlatform::Linux,
        PortablePlatform::Windows,
    ] {
        let error = AppPaths::resolve_for(
            platform,
            &PathEnvironment::default().without_home(),
            env(&[]),
        )
        .expect_err("no home");
        assert!(
            matches!(error, PathError::HomeUnavailable),
            "{platform:?} must report HomeUnavailable, got {error:?}"
        );
    }
}

#[test]
fn agentuna_home_overrides_the_platform_layout() {
    let overrides = PathOverrides::from_lookup(|name| match name {
        "AGENTUNA_HOME" => Some(OsString::from("/sandbox/support")),
        _ => None,
    });
    let paths = resolve_for(
        PortablePlatform::Linux,
        &PathEnvironment::default()
            .with_home("/home/a")
            .with_overrides(overrides),
        &[("XDG_CONFIG_HOME", "/xdg/config")],
    );

    assert_eq!(paths.support_dir(), Path::new("/sandbox/support"));
    assert_eq!(paths.local_state_dir(), Path::new("/sandbox/support"));
    assert_eq!(paths.cache_dir(), Path::new("/sandbox/support/cache"));
    assert_eq!(paths.user_store(), Path::new("/home/a/.agentuna"));
}

#[test]
fn switching_projects_keeps_the_resolved_platform_roots() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let project = sandbox.path().join("repo");
    std::fs::create_dir(&project).expect("project dir");
    let paths = resolve_for(
        PortablePlatform::Linux,
        &PathEnvironment::default().with_home("/home/a"),
        &[
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_DATA_HOME", "/xdg/data"),
        ],
    );

    let with_project = paths.for_project(&project).expect("project paths");
    assert_eq!(
        with_project.project_store(),
        Some(project.join(".agentuna").as_path())
    );
    assert_eq!(
        with_project.support_dir(),
        Path::new("/xdg/config/agentuna")
    );
    assert_eq!(
        with_project.local_state_dir(),
        Path::new("/xdg/data/agentuna")
    );
    assert_eq!(with_project.user_store(), paths.user_store());
}

#[test]
fn a_path_key_round_trips_through_database_storage() {
    let database = at_db::Database::open_in_memory().expect("in-memory database");
    let native = PathBuf::from(r"C:\Users\a\.agentuna\skills\x");
    let key = PathKey::from_path(&native, PortablePlatform::Windows).expect("windows key");
    assert_eq!(key.as_str(), "C:/Users/a/.agentuna/skills/x");

    database
        .connection()
        .execute(
            "INSERT INTO projects (id, name, path) VALUES ('prj_round_trip', 'demo', ?1)",
            [key.as_str()],
        )
        .expect("insert project row");
    let stored: String = database
        .connection()
        .query_row(
            "SELECT path FROM projects WHERE id = 'prj_round_trip'",
            [],
            |row| row.get(0),
        )
        .expect("read project row");

    assert!(
        !stored.contains('\\'),
        "stored text must use forward slashes: {stored}"
    );
    let read_back = PathKey::from_text(&stored, PortablePlatform::Windows).expect("read back");
    assert_eq!(read_back, key);
    assert_eq!(read_back.to_native(PortablePlatform::Windows), native);
    // A macOS or Linux host converts the same stored text to its own separator rules.
    assert_eq!(
        read_back.to_native(PortablePlatform::Linux),
        PathBuf::from("C:/Users/a/.agentuna/skills/x")
    );
}
