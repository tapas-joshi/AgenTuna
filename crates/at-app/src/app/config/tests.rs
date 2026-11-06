//! Behaviour tests for the app-facing runtime configuration.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use tempfile::TempDir;

use super::*;

fn paths_for(temp: &Path) -> AppPaths {
    let environment = PathEnvironment::default()
        .with_home(temp.join("home"))
        .with_overrides(PathOverrides {
            home: Some(temp.join("support")),
            ..PathOverrides::default()
        });
    AppPaths::resolve(&environment).expect("paths")
}

#[test]
fn load_reads_the_support_directory_file_and_reports_every_flag() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let config = AppConfig::load(&paths, Vec::new());

    let expected = paths.support_dir().join(CONFIG_FILE_NAME);
    assert_eq!(config.report().config_path, expected.display().to_string());
    assert!(expected.exists(), "first load creates the file");
    assert!(!config.is_degraded());
    assert!(!config.flags().marketplace());
    assert!(config.flags().presets());

    let section = config.diagnostics_section();
    assert_eq!(section["flags"]["marketplace"]["source"], "default");
    assert_eq!(section["flags"]["presets"]["value"], true);
    assert_eq!(config.safety_flags().len(), SAFETY_FLAGS.len());
}

#[test]
fn command_line_overrides_reach_the_typed_flags() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    std::fs::create_dir_all(paths.support_dir()).expect("support dir");
    std::fs::write(
        paths.support_dir().join(CONFIG_FILE_NAME),
        "[flags]\nmarketplace = false\n",
    )
    .expect("write config");

    let config = AppConfig::load(&paths, vec!["agentuna:marketplace=true".to_owned()]);
    assert!(config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        at_store::config::FlagSource::CommandLine
    );
}

#[test]
fn an_unwritable_support_directory_falls_back_to_the_compiled_defaults() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    std::fs::create_dir_all(paths.support_dir()).expect("support dir");
    let original = std::fs::metadata(paths.support_dir())
        .expect("metadata")
        .permissions();
    std::fs::set_permissions(paths.support_dir(), std::fs::Permissions::from_mode(0o555))
        .expect("make read-only");

    let config = AppConfig::load(&paths, Vec::new());
    assert!(config.is_degraded());
    assert!(!config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        at_store::config::FlagSource::Default
    );

    std::fs::set_permissions(paths.support_dir(), original).expect("restore permissions");
}

#[test]
fn the_report_serializes_to_the_diagnostics_object() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let config = AppConfig::load(&paths, Vec::new());
    assert_eq!(
        config.diagnostics_section(),
        serde_json::to_value(config.report()).expect("serialize")
    );
}

#[test]
fn support_directory_is_created_on_first_load() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    assert!(!paths.support_dir().exists());
    let _config = AppConfig::load(&paths, Vec::new());
    assert!(paths.support_dir().exists());
    assert!(PathBuf::from(&_config.report().config_path).exists());
}
