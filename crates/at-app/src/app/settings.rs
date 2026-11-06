//! Typed preferences for the Tauri shell.
//!
//! The canonical keys, defaults and validation live in [`at_db::settings`]; this
//! module re-exports them so command modules and the webview contract depend on
//! the shell's own surface rather than on the database crate's module layout.
//! Nothing here adds behaviour: the Preferences screen (TU-019) calls these
//! functions through commands, and every write goes through `at-db`.

pub use at_db::settings::{
    BackgroundHealth, DEFAULT_BACKUP_RETENTION_DAYS, HealthInterval, ImportReport,
    MIN_BACKUP_RETENTION_DAYS, SETTINGS_SCHEMA_VERSION, Settings, SettingsDocument, SettingsError,
    SettingsKey, SettingsKeyError, UNDO_WINDOW_DAYS, export, import, load, set_auto_update_skills,
    set_background_health, set_backup_retention_days, set_default_link_mode, set_global_store_path,
    set_health_interval, set_last_project_id, set_last_scope, set_link_mode,
    set_marketplace_enabled, set_native_banner_hidden, set_value,
};
