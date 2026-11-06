//! App-facing runtime configuration: resolved once at startup and exposed read-only.
//!
//! The file format and precedence rules live in [`at_store::config`] so the CLI and the app
//! read exactly the same values. This module only binds them to the support directory and adds
//! the Tauri payload (`config_report`) and the diagnostics fragment.
//!
//! Flags are read once: the typed [`FeatureFlags`] value in [`AppConfig`] never changes, which is
//! why [`at_store::config::ConfigReport::restart_note`] tells the UI that a restart is required.

use std::sync::Arc;

use at_store::config::{
    CONFIG_FILE_NAME, ConfigInput, ConfigReport, FeatureFlags, RuntimeConfig, SAFETY_FLAGS,
};
use at_store::paths::AppPaths;
use tauri::State;

use super::state::AppState;

/// The process-wide runtime configuration.
pub struct AppConfig {
    inner: RuntimeConfig,
    degraded: bool,
}

impl AppConfig {
    /// Loads `<support>/agentuna.toml` with the process environment and the given CLI overrides.
    ///
    /// A file that cannot be created is not fatal: the compiled defaults are used and the
    /// problem is logged, because a read-only or full disk must not prevent the app from
    /// starting.
    #[must_use]
    pub fn load(paths: &AppPaths, cli_flags: Vec<String>) -> Self {
        let path = paths.support_dir().join(CONFIG_FILE_NAME);
        match RuntimeConfig::load(&path, ConfigInput::production(cli_flags)) {
            Ok(inner) => Self {
                inner,
                degraded: false,
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    "runtime configuration could not be created; using the compiled defaults"
                );
                Self {
                    inner: RuntimeConfig::defaults(path),
                    degraded: true,
                }
            }
        }
    }

    /// The read-only typed flags.
    #[must_use]
    pub fn flags(&self) -> &FeatureFlags {
        self.inner.flags()
    }

    /// The serializable effective configuration.
    #[must_use]
    pub fn report(&self) -> &ConfigReport {
        self.inner.report()
    }

    /// Whether loading had to fall back to the compiled defaults.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// The object `diagnostics.json` embeds under `feature_flags`.
    #[must_use]
    pub fn diagnostics_section(&self) -> serde_json::Value {
        self.inner.diagnostics_section()
    }

    /// Every safety flag this build refuses, for the settings screen's read-only list.
    #[must_use]
    pub fn safety_flags(&self) -> &'static [&'static str] {
        &SAFETY_FLAGS
    }
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppConfig")
            .field("degraded", &self.degraded)
            .field("report", &self.inner.report())
            .finish()
    }
}

/// The shared, read-only configuration used by every command.
pub type SharedConfig = Arc<AppConfig>;

/// Tauri command: the effective flags with value, default and source.
///
/// The payload is exactly [`RuntimeConfig::diagnostics_section`], which is also what
/// `agentuna config --json` prints and `diagnostics.json` embeds, so a support report and the
/// settings screen cannot disagree.
#[tauri::command]
#[must_use]
pub fn config_report(state: State<'_, AppState>) -> serde_json::Value {
    match state.config() {
        Some(config) => config.diagnostics_section(),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests;
