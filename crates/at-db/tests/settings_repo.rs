//! TF-023: typed settings over the `settings` table.

use at_db::Database;
use at_db::ops::OperationActor;
use at_db::settings::{
    BackgroundHealth, DEFAULT_BACKUP_RETENTION_DAYS, HealthInterval, MIN_BACKUP_RETENTION_DAYS,
    SETTINGS_SCHEMA_VERSION, Settings, SettingsDocument, SettingsError, SettingsKey,
    UNDO_WINDOW_DAYS, export, import, load, set_auto_update_skills, set_background_health,
    set_backup_retention_days, set_default_link_mode, set_global_store_path, set_health_interval,
    set_last_project_id, set_last_scope, set_link_mode, set_marketplace_enabled,
    set_native_banner_hidden, set_value,
};
use at_model::{AgentId, ItemKind, LinkMode, Scope};
use rusqlite::Connection;

const UI: OperationActor = OperationActor::Ui;

fn database() -> Database {
    Database::open_in_memory().expect("in-memory database opens")
}

fn stored_rows(connection: &Connection) -> Vec<(String, String)> {
    let mut statement = connection
        .prepare("SELECT key, value_json FROM settings ORDER BY key ASC")
        .expect("select prepares");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("select runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows are text")
}

fn settings_row_count(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .expect("count runs")
}

fn audit_rows(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT summary FROM operations WHERE kind = 'settings' ORDER BY started_at ASC")
        .expect("select prepares");
    statement
        .query_map([], |row| row.get(0))
        .expect("select runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows are text")
}

#[test]
fn a_fresh_database_reads_complete_defaults_without_writing() {
    let database = database();
    let connection = database.connection();
    let settings = load(connection).expect("fresh database loads");

    assert_eq!(settings, Settings::default());
    assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
    assert_eq!(settings.health_interval, HealthInterval::FiveMinutes);
    assert_eq!(settings.health_interval.minutes(), 5);
    assert_eq!(settings.background_health, BackgroundHealth::Local);
    assert_eq!(
        settings.backup_retention_days,
        DEFAULT_BACKUP_RETENTION_DAYS
    );
    assert_eq!(settings.default_link_mode, LinkMode::Link);
    assert!(!settings.marketplace_enabled);
    assert!(!settings.auto_update_skills);
    assert_eq!(settings.last_project_id, None);
    assert_eq!(settings.last_scope("prj_one"), None);
    assert_eq!(settings.global_store_path(), None);
    assert_eq!(
        settings_row_count(connection),
        0,
        "reading must not write defaults"
    );
}

#[test]
fn health_interval_and_background_health_accept_only_documented_values() {
    let mut database = database();
    let connection = database.connection_mut();

    for rejected in ["7", "30", "-5", "\"5\""] {
        let error = set_value(
            connection,
            &SettingsKey::HealthIntervalMinutes,
            rejected,
            UI,
        )
        .expect_err("invalid interval is rejected");
        assert!(
            matches!(error, SettingsError::InvalidValue { .. }),
            "unexpected error: {error}"
        );
    }
    for rejected in ["\"sometimes\"", "\"true\"", "1"] {
        assert!(matches!(
            set_value(connection, &SettingsKey::BackgroundHealth, rejected, UI),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    set_health_interval(connection, HealthInterval::Off, UI).expect("off persists");
    set_health_interval(connection, HealthInterval::SixtyMinutes, UI).expect("60 persists");
    set_background_health(connection, BackgroundHealth::All, UI).expect("all persists");

    let settings = load(connection).expect("settings load");
    assert_eq!(settings.health_interval, HealthInterval::SixtyMinutes);
    assert_eq!(settings.background_health, BackgroundHealth::All);
    assert_eq!(settings_row_count(connection), 2);
}

#[test]
fn retention_below_the_minimum_is_clamped_with_a_warning() {
    let mut database = database();
    let connection = database.connection_mut();

    let stored = set_backup_retention_days(connection, 10, UI).expect("clamped write");
    assert_eq!(stored, MIN_BACKUP_RETENTION_DAYS);
    assert_eq!(load(connection).expect("load").backup_retention_days, 30);

    let stored = set_backup_retention_days(connection, 45, UI).expect("valid write");
    assert_eq!(stored, 45);
    assert_eq!(load(connection).expect("load").backup_retention_days, 45);
}

#[test]
fn suffixed_keys_accept_only_known_agents_kinds_and_scopes() {
    let mut database = database();
    let connection = database.connection_mut();

    for malformed in ["link_mode.not_an_agent", "native_banner_hidden.widget"] {
        assert!(
            matches!(
                set_value(
                    connection,
                    &SettingsKey::LinkMode(AgentId::Codex),
                    "\"copy\"",
                    UI
                ),
                Ok(())
            ),
            "a valid key writes"
        );
        let error = SettingsKey::parse(malformed).expect_err("malformed key is rejected");
        assert!(matches!(
            error,
            at_db::settings::SettingsKeyError::Malformed { .. }
        ));
    }

    // A malformed key never reaches the writer: `parse` is the gate.
    assert!(SettingsKey::parse("link_mode.codex").is_ok());
    assert!(SettingsKey::parse("native_banner_hidden.mcp_server").is_ok());
    assert!(SettingsKey::parse("last_scope.prj_01H").is_ok());
    assert!(SettingsKey::parse("future_preference").is_err());

    set_link_mode(connection, &AgentId::Codex, LinkMode::Copy, UI).expect("per-agent mode");
    set_native_banner_hidden(connection, ItemKind::Skill, true, UI).expect("banner");
    set_last_scope(connection, "prj_01H", Some(Scope::Project), UI).expect("last scope");

    let body = load(connection).expect("load");
    assert_eq!(body.link_mode(&AgentId::Codex), LinkMode::Copy);
    assert_eq!(body.link_mode(&AgentId::Omp), LinkMode::Link);
    assert!(body.native_banner_hidden(ItemKind::Skill));
    assert!(!body.native_banner_hidden(ItemKind::Rule));
    assert_eq!(body.last_scope("prj_01H"), Some(Scope::Project));
    assert_eq!(body.last_scope("prj_missing"), None);

    assert!(matches!(
        set_value(
            connection,
            &SettingsKey::LastScope("prj_01H".to_owned()),
            "\"workspace\"",
            UI
        ),
        Err(SettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        set_last_scope(connection, "", Some(Scope::Project), UI),
        Err(SettingsError::InvalidKey { .. })
    ));
}

#[test]
fn un_hiding_a_banner_clears_its_row() {
    let mut database = database();
    let connection = database.connection_mut();

    set_native_banner_hidden(connection, ItemKind::McpServer, true, UI).expect("hide");
    assert_eq!(settings_row_count(connection), 1);
    set_native_banner_hidden(connection, ItemKind::McpServer, false, UI).expect("un-hide");
    assert_eq!(settings_row_count(connection), 0);
    assert!(
        !load(connection)
            .expect("load")
            .native_banner_hidden(ItemKind::McpServer)
    );
}

#[test]
fn global_store_path_is_absolute_or_absent() {
    let mut database = database();
    let connection = database.connection_mut();

    set_global_store_path(
        connection,
        Some(std::path::Path::new("/Library/AgenTuna")),
        UI,
    )
    .expect("absolute path");
    assert_eq!(
        load(connection)
            .expect("load")
            .global_store_path()
            .and_then(|path| path.to_str()),
        Some("/Library/AgenTuna")
    );

    set_global_store_path(connection, Some(std::path::Path::new("relative/store")), UI)
        .expect_err("relative path is rejected");
    set_global_store_path(connection, None, UI).expect("clear");
    assert_eq!(load(connection).expect("load").global_store_path(), None);
}

#[test]
fn undo_window_days_is_a_constant_with_no_row_or_override() {
    assert_eq!(UNDO_WINDOW_DAYS, 30);
    let database = database();
    let connection = database.connection();

    // Even a row that tries to override retention is not a setting.
    connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('undo_window_days', '90')",
            [],
        )
        .expect("row inserted");

    let settings = load(connection).expect("load");
    assert_eq!(UNDO_WINDOW_DAYS, 30);
    assert!(settings.unknown.contains_key("undo_window_days"));
    assert_eq!(settings.backup_retention_days, 30);
}

#[test]
fn unknown_rows_are_preserved_and_exported() {
    let database = database();
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO settings (key, value_json) VALUES ('future_preference', '{\"a\":  1}')",
            [],
        )
        .expect("row inserted");

    let settings = load(connection).expect("load");
    assert_eq!(
        settings
            .unknown
            .get("future_preference")
            .map(String::as_str),
        Some("{\"a\":  1}")
    );

    let document = export(connection).expect("export");
    let raw = document
        .values
        .get("future_preference")
        .expect("unknown key exported");
    assert_eq!(raw.get(), "{\"a\":  1}");
}

#[test]
fn export_then_import_round_trips_byte_identically() {
    let mut source = database();
    {
        let connection = source.connection_mut();
        set_health_interval(connection, HealthInterval::FifteenMinutes, UI).expect("interval");
        set_background_health(connection, BackgroundHealth::All, UI).expect("health");
        set_backup_retention_days(connection, 60, UI).expect("retention");
        set_default_link_mode(connection, LinkMode::Copy, UI).expect("default mode");
        set_link_mode(connection, &AgentId::ClaudeCode, LinkMode::Link, UI).expect("mode");
        set_native_banner_hidden(connection, ItemKind::Rule, true, UI).expect("banner");
        set_marketplace_enabled(connection, true, UI).expect("marketplace");
        set_auto_update_skills(connection, true, UI).expect("updates");
        set_last_scope(connection, "prj_01H", Some(Scope::Local), UI).expect("scope");
        set_last_project_id(connection, Some("prj_01H"), UI).expect("project");
        set_global_store_path(
            connection,
            Some(std::path::Path::new("/Library/AgenTuna/global")),
            UI,
        )
        .expect("store path");
        connection
            .execute(
                "INSERT INTO settings (key, value_json) VALUES ('future_preference', '{\"nested\": [1, 2]}')",
                [],
            )
            .expect("unknown row");
    }

    let document = export(source.connection()).expect("export");
    let text = serde_json::to_string(&document).expect("document serialises");
    let parsed: SettingsDocument = serde_json::from_str(&text).expect("document parses");

    let mut target = database();
    let report = import(target.connection_mut(), &parsed, OperationActor::Cli).expect("import");
    assert_eq!(report.preserved, 1);

    let original = stored_rows(source.connection());
    assert_eq!(stored_rows(target.connection()), original);

    let reexported = serde_json::to_string(&export(target.connection()).expect("re-export"))
        .expect("re-export serialises");
    assert_eq!(reexported, text);
    assert_eq!(
        load(target.connection()).expect("load"),
        load(source.connection()).expect("load")
    );
}

#[test]
fn an_import_with_one_invalid_value_writes_nothing() {
    let document: SettingsDocument = serde_json::from_str(
        r#"{
            "schema_version": 1,
            "values": {
                "background_health": "all",
                "health_interval_minutes": 7
            }
        }"#,
    )
    .expect("document parses");

    let mut target = database();
    let error = import(target.connection_mut(), &document, UI).expect_err("invalid value");
    match error {
        SettingsError::InvalidValue { key, .. } => assert_eq!(key, "health_interval_minutes"),
        other => panic!("expected InvalidValue, got {other}"),
    }
    assert_eq!(
        settings_row_count(target.connection()),
        0,
        "a rejected import writes nothing"
    );

    let newer: SettingsDocument =
        serde_json::from_str(r#"{"schema_version": 99, "values": {}}"#).expect("document parses");
    assert!(matches!(
        import(target.connection_mut(), &newer, UI),
        Err(SettingsError::UnsupportedSchemaVersion { found: 99, .. })
    ));
    assert_eq!(settings_row_count(target.connection()), 0);
}

#[test]
fn audited_keys_record_one_operations_row_and_navigation_does_not() {
    let mut database = database();
    let connection = database.connection_mut();

    set_marketplace_enabled(connection, true, UI).expect("enable marketplace");
    let rows = audit_rows(connection);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].contains("network.marketplace_enabled") && rows[0].contains("false -> true"),
        "summary carries the old and new value: {}",
        rows[0]
    );

    // Writing the same value again is not a change.
    set_marketplace_enabled(connection, true, UI).expect("same value");
    assert_eq!(audit_rows(connection).len(), 1);

    set_last_project_id(connection, Some("prj_01H"), UI).expect("navigation write");
    set_last_scope(connection, "prj_01H", Some(Scope::User), UI).expect("navigation write");
    set_native_banner_hidden(connection, ItemKind::Skill, true, UI).expect("banner write");
    assert_eq!(
        audit_rows(connection).len(),
        1,
        "navigation and banner keys are not audited"
    );
}
