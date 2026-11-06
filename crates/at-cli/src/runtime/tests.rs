//! Behaviour tests for `agentuna config` and the CLI lock helpers.

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

fn config_file(paths: &AppPaths) -> PathBuf {
    paths.support_dir().join(CONFIG_FILE_NAME)
}

#[test]
fn the_json_payload_lists_every_flag_with_value_default_and_source() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let config = load_config(&paths, Vec::new(), true).expect("load");
    let output = config_output(&config);

    for key in [
        "marketplace",
        "remote_backup",
        "auto_update",
        "instructions",
        "presets",
    ] {
        let entry = &output.data["flags"][key];
        assert!(entry.is_object(), "missing flag {key}: {}", output.data);
        assert!(entry.get("value").is_some(), "{key} has no value");
        assert!(entry.get("default").is_some(), "{key} has no default");
        assert!(entry.get("source").is_some(), "{key} has no source");
    }
    assert_eq!(output.data["flags"]["marketplace"]["source"], "default");
    assert_eq!(output.data["requires_restart"], true);
    assert!(output.human.contains("restart required: yes"));
    assert!(output.human.contains("marketplace"));
    assert!(output.human.contains("log_level"));
}

#[test]
fn command_line_flag_overrides_show_the_command_line_source() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let config =
        load_config(&paths, vec!["agentuna:marketplace=true".to_owned()], true).expect("load");
    let output = config_output(&config);
    assert_eq!(output.data["flags"]["marketplace"]["value"], true);
    assert_eq!(
        output.data["flags"]["marketplace"]["source"],
        "command_line"
    );
}

#[test]
fn read_only_config_reads_do_not_create_the_file() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let config = load_config(&paths, Vec::new(), false).expect("load");
    assert!(!config_file(&paths).exists());
    assert!(!config.flags().marketplace());
}

#[test]
fn configuration_warnings_reach_the_envelope() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    std::fs::create_dir_all(paths.support_dir()).expect("support dir");
    std::fs::write(config_file(&paths), "[flags]\nmarketplce = true\n").expect("write config");

    let config = load_config(&paths, Vec::new(), true).expect("load");
    let output = config_output(&config);
    assert_eq!(output.warnings.len(), 1);
    assert!(
        output.warnings[0]
            .message
            .contains("did you mean `marketplace`?")
    );
    assert!(output.human.contains("marketplace"));
}

#[test]
fn read_only_access_rejects_writes_with_an_actionable_error() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let access = CliAccess::read_only(&paths).expect("read-only access");
    let error = access.ensure_writable().expect_err("write rejected");
    assert_eq!(error.code(), ErrorCode::Locked);
    assert!(error.message().contains("read-only"));
    assert!(error.hint().contains("--read-only"));
    assert!(!paths.support_dir().join("instance.lock").exists());
}

#[test]
fn a_read_while_a_write_transaction_holds_the_lock_reports_the_holder() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let writer = CliAccess::write(&paths).expect("writer");
    let record = writer.record().expect("record").clone();

    let error = CliAccess::read(&paths).expect_err("shared lock blocked");
    assert_eq!(error.code(), ErrorCode::Locked);
    assert!(error.message().contains(&record.pid.to_string()), "{error}");
    assert!(error.message().contains(&record.version));
    let details = error.details().expect("holder details");
    assert_eq!(details["holder"]["pid"], record.pid);

    writer.release();
    let reader = CliAccess::read(&paths).expect("lock released");
    reader.release();
}

#[test]
fn a_read_only_lock_error_keeps_the_reason_and_a_real_hint() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let access = CliAccess::read_only(&paths).expect("read-only");
    let error = lock_error_to_cli(LockError::ReadOnly);
    assert_eq!(error.code(), ErrorCode::Locked);
    assert!(error.message().contains("read-only"));
    assert!(!error.hint().is_empty());
    access.release();
}

#[test]
fn io_failures_map_to_conflict_with_the_path() {
    let error = lock_error_to_cli(LockError::Io {
        path: PathBuf::from("/tmp/agentuna/instance.lock"),
        message: "permission denied".to_owned(),
    });
    assert_eq!(error.code(), ErrorCode::Conflict);
    assert!(error.message().contains("instance.lock"));
    assert!(!error.hint().is_empty());
}

#[test]
fn read_only_index_never_migrates_and_rejects_writes() {
    let temp = TempDir::new().expect("temp dir");
    let paths = paths_for(temp.path());
    let index = open_index_read_only(&paths).expect_err("a missing index is a typed error");
    assert_eq!(index.code(), ErrorCode::NotFound);
    assert!(!paths.db_path().exists());

    std::fs::create_dir_all(paths.support_dir()).expect("support dir");
    let created = at_db::Database::open_file(paths.db_path()).expect("create the index");
    let version = created.schema_version().expect("schema version");
    drop(created);
    let read_only = open_index_read_only(&paths).expect("open read-only");
    assert_eq!(read_only.schema_version().expect("version"), version);
    let error = read_only
        .execute("CREATE TABLE probe (id INTEGER)", [])
        .expect_err("write rejected");
    assert!(matches!(error, at_db::ReadOnlyError::ReadOnly), "{error:?}");
}

#[test]
fn the_registered_group_exposes_config_and_its_help_parses() {
    let command = ConfigGroup.attach(clap::Command::new("agentuna"));
    let config = command
        .find_subcommand("config")
        .expect("config subcommand");
    let matches = config
        .clone()
        .try_get_matches_from(["config", "--flag", "agentuna:presets=false"])
        .expect("parse config");
    let flags: Vec<&String> = matches
        .get_many::<String>("flag")
        .expect("flag values")
        .collect();
    assert_eq!(flags, vec!["agentuna:presets=false"]);

    let help = config.clone().try_get_matches_from(["config", "--help"]);
    assert!(help.is_err_and(|error| error.exit_code() == 0));
}
