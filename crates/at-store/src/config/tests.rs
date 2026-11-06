//! Behaviour tests for the runtime configuration: precedence, warnings and the report shape.

use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

fn config_path() -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(CONFIG_FILE_NAME);
    (temp, path)
}

fn write_config(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).expect("write config");
}

fn environment(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn cli(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn first_run_creates_an_annotated_file_and_reports_compiled_defaults() {
    let (_temp, path) = config_path();
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");

    let text = std::fs::read_to_string(&path).expect("created file");
    assert!(text.contains("# AgenTuna runtime configuration."));
    assert!(text.contains("[flags]"));
    assert!(text.contains("[advanced]"));
    assert!(text.contains("marketplace = false"));
    assert!(text.contains("instructions = true"));
    assert!(!text.contains("allow_overwrite ="));

    assert!(!config.flags().marketplace());
    assert!(config.flags().instructions());
    assert!(config.flags().presets());
    let report = config.report();
    assert!(report.requires_restart);
    assert!(report.restart_note.contains("restart"));
    assert_eq!(report.flags.len(), FLAGS.len());
    let marketplace = &report.flags["marketplace"];
    assert!(!marketplace.value);
    assert!(!marketplace.default);
    assert_eq!(marketplace.source, FlagSource::Default);
}

#[test]
fn deleting_the_file_restores_the_defaults_on_the_next_start() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplace = true\n");

    let enabled = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(enabled.flags().marketplace());
    assert_eq!(
        enabled.report().flags["marketplace"].source,
        FlagSource::File
    );

    std::fs::remove_file(&path).expect("remove");
    let restored = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(!restored.flags().marketplace());
    assert_eq!(
        restored.report().flags["marketplace"].source,
        FlagSource::Default
    );
    assert!(path.exists(), "the next start recreates the file");
}

#[test]
fn precedence_one_compiled_default_applies_without_a_file_value() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\npresets = true\n");
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(!config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        FlagSource::Default
    );
}

#[test]
fn precedence_two_the_file_overrides_the_default() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplace = true\n");
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        FlagSource::File
    );
}

#[test]
fn precedence_three_the_environment_overrides_the_file() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplace = false\n");
    let input = ConfigInput::default()
        .with_environment(environment(&[("AGENTUNA_FLAG_MARKETPLACE", "true")]));
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        FlagSource::Environment
    );
}

#[test]
fn precedence_four_the_command_line_overrides_the_environment_and_file() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplace = false\n");
    let input = ConfigInput {
        cli_flags: cli(&["agentuna:marketplace=true"]),
        ..ConfigInput::default()
            .with_environment(environment(&[("AGENTUNA_FLAG_MARKETPLACE", "false")]))
    };
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(config.flags().marketplace());
    assert_eq!(
        config.report().flags["marketplace"].source,
        FlagSource::CommandLine
    );
}

#[test]
fn an_unknown_key_warns_once_with_a_suggestion_and_startup_continues() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplce = true\n");
    let input = ConfigInput::default()
        .with_environment(environment(&[("AGENTUNA_FLAG_MARKETPLCE", "true")]));
    let config = RuntimeConfig::load(&path, input).expect("load");

    let warnings = &config.report().warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].key, "marketplce");
    assert_eq!(warnings[0].suggestion.as_deref(), Some("marketplace"));
    assert!(!config.flags().marketplace());
}

#[test]
fn safety_flags_are_rejected_even_when_present() {
    let (_temp, path) = config_path();
    write_config(
        &path,
        "[flags]\nallow_overwrite = true\nskip_backup = true\n\
         disable_concurrency_check = true\ndisable_refuse_overwrite = true\n",
    );
    let input = ConfigInput {
        cli_flags: cli(&["agentuna:allow_overwrite=true"]),
        ..ConfigInput::default()
    };
    let config = RuntimeConfig::load(&path, input).expect("load");

    let warnings = &config.report().warnings;
    assert_eq!(warnings.len(), SAFETY_FLAGS.len(), "{warnings:?}");
    for warning in warnings {
        assert!(
            warning.message.contains(SAFETY_REJECTION),
            "unexpected message: {}",
            warning.message
        );
    }
    for key in SAFETY_FLAGS {
        assert!(
            !config.report().flags.contains_key(key),
            "safety key {key} must not be a flag"
        );
    }
    assert!(!config.flags().marketplace());
}

#[test]
fn a_safety_flag_in_the_environment_is_rejected_too() {
    let (_temp, path) = config_path();
    let input = ConfigInput::default()
        .with_environment(environment(&[("AGENTUNA_FLAG_SKIP_BACKUP", "true")]));
    let config = RuntimeConfig::load(&path, input).expect("load");
    let warnings = &config.report().warnings;
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].key, "skip_backup");
    assert!(warnings[0].message.contains(SAFETY_REJECTION));
}

#[test]
fn invalid_toml_warns_and_keeps_the_defaults() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags\nmarketplace = true\n");
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(!config.flags().marketplace());
    assert_eq!(config.report().warnings.len(), 1);
}

#[test]
fn invalid_values_warn_and_are_ignored() {
    let (_temp, path) = config_path();
    write_config(&path, "[flags]\nmarketplace = \"loud\"\n");
    let input = ConfigInput::default()
        .with_environment(environment(&[("AGENTUNA_FLAG_REMOTE_BACKUP", "maybe")]));
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(!config.flags().marketplace());
    assert!(!config.flags().remote_backup());
    let keys: Vec<&str> = config
        .report()
        .warnings
        .iter()
        .map(|warning| warning.key.as_str())
        .collect();
    assert!(keys.contains(&"marketplace"));
    assert!(keys.contains(&"remote_backup"));
}

#[test]
fn advanced_values_come_from_the_file_and_are_reported_with_their_source() {
    let (_temp, path) = config_path();
    write_config(
        &path,
        "[advanced]\nlog_level = \"debug\"\nmax_concurrent_jobs = 2\n\
         mcp_connect_timeout_seconds = 45\n",
    );
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert_eq!(config.advanced().log_level, LogLevel::Debug);
    assert_eq!(config.advanced().max_concurrent_jobs, 2);
    assert_eq!(config.advanced().mcp_connect_timeout_seconds, 45);

    let report = config.report();
    assert_eq!(report.advanced["log_level"].source, FlagSource::File);
    assert_eq!(report.advanced["log_level"].value, "debug");
    assert_eq!(report.advanced["log_level"].default, "info");
    assert_eq!(report.advanced["max_concurrent_jobs"].value, 2);
    assert_eq!(report.advanced["max_concurrent_jobs"].default, 4);
}

#[test]
fn out_of_range_advanced_values_warn_and_keep_the_default() {
    let (_temp, path) = config_path();
    write_config(
        &path,
        "[advanced]\nlog_level = \"loud\"\nmax_concurrent_jobs = 0\n\
         mcp_connect_timeout_seconds = 900\n",
    );
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert_eq!(config.advanced().log_level, LogLevel::Info);
    assert_eq!(config.advanced().max_concurrent_jobs, 4);
    assert_eq!(config.advanced().mcp_connect_timeout_seconds, 30);
    assert_eq!(config.report().warnings.len(), 3);
}

#[test]
fn the_report_lists_every_flag_with_value_default_and_source() {
    let (_temp, path) = config_path();
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    let report = config.report();
    assert_eq!(report.flags.len(), FLAGS.len());
    for definition in FLAGS {
        let entry = report
            .flags
            .get(definition.key)
            .unwrap_or_else(|| panic!("missing {}", definition.key));
        assert_eq!(entry.default, definition.default);
        assert!(!entry.description.is_empty());
    }

    let section = config.diagnostics_section();
    assert_eq!(section["requires_restart"], true);
    assert_eq!(section["flags"]["marketplace"]["value"], false);
    assert_eq!(section["flags"]["marketplace"]["default"], false);
    assert_eq!(section["flags"]["marketplace"]["source"], "default");
    assert_eq!(section["advanced"]["log_level"]["default"], "info");
    assert_eq!(
        section,
        serde_json::to_value(report).expect("serialize report")
    );
}

#[test]
fn read_only_input_never_creates_the_file() {
    let (_temp, path) = config_path();
    let input = ConfigInput {
        create: false,
        ..ConfigInput::default()
    };
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(!path.exists());
    assert!(!config.flags().marketplace());
}

#[test]
fn a_missing_parent_directory_is_created_on_first_run() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp
        .path()
        .join("nested")
        .join("support")
        .join(CONFIG_FILE_NAME);
    let config = RuntimeConfig::load(&path, ConfigInput::default()).expect("load");
    assert!(path.exists());
    assert!(!config.flags().auto_update());
}

#[test]
fn defaults_without_a_file_report_every_flag_as_default() {
    let path = PathBuf::from("/nonexistent/agentuna.toml");
    let config = RuntimeConfig::defaults(&path);
    assert_eq!(config.report().flags.len(), FLAGS.len());
    for entry in config.report().flags.values() {
        assert_eq!(entry.source, FlagSource::Default);
        assert_eq!(entry.value, entry.default);
    }
    assert!(config.report().warnings.is_empty());
}

#[test]
fn vendor_prefixes_other_than_agentuna_are_rejected_without_failing_startup() {
    let (_temp, path) = config_path();
    let input = ConfigInput {
        cli_flags: cli(&["other:marketplace=true"]),
        ..ConfigInput::default()
    };
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(!config.flags().marketplace());
    assert_eq!(config.report().warnings.len(), 1);
}

#[test]
fn boolean_spellings_and_bare_keys_are_accepted() {
    let (_temp, path) = config_path();
    let input = ConfigInput {
        cli_flags: cli(&[
            "marketplace=yes",
            "agentuna:remote_backup=on",
            "auto_update=1",
        ]),
        ..ConfigInput::default()
    };
    let config = RuntimeConfig::load(&path, input).expect("load");
    assert!(config.flags().marketplace());
    assert!(config.flags().remote_backup());
    assert!(config.flags().auto_update());
    assert!(config.report().warnings.is_empty());
}
