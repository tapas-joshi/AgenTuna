//! Managed application state shared by Tauri commands.

use std::sync::Arc;

use super::bridge::JobBridge;
use super::cli_install::CliInstaller;
use super::config::AppConfig;
use super::instance::InstanceState;

/// Process-wide services shared by every Tauri command.
pub struct AppState {
    bridge: Arc<JobBridge>,
    cli: Option<CliInstaller>,
    config: Option<Arc<AppConfig>>,
    instance: Option<Arc<InstanceState>>,
    logging: Option<at_store::logging::Logging>,
}

impl AppState {
    /// Creates state from the async job bridge and an optional packaged-CLI installer.
    #[must_use]
    pub fn new(bridge: Arc<JobBridge>, cli: Option<CliInstaller>) -> Self {
        Self {
            bridge,
            cli,
            config: None,
            instance: None,
            logging: None,
        }
    }

    /// Attaches the runtime configuration loaded at startup.
    #[must_use]
    pub fn with_config(mut self, config: Arc<AppConfig>) -> Self {
        self.config = Some(config);
        self
    }

    /// Attaches the process logging handle (reload + redactor slot).
    #[must_use]
    pub fn with_logging(mut self, logging: at_store::logging::Logging) -> Self {
        self.logging = Some(logging);
        self
    }

    /// Attaches the single-instance state that owns the record and activation listener.
    #[must_use]
    pub fn with_instance(mut self, instance: Arc<InstanceState>) -> Self {
        self.instance = Some(instance);
        self
    }

    /// The async job bridge used by long-running commands.
    #[must_use]
    pub fn bridge(&self) -> &JobBridge {
        &self.bridge
    }

    /// The packaged-CLI installer; absent when the bundle layout cannot be resolved.
    #[must_use]
    pub fn cli(&self) -> Option<&CliInstaller> {
        self.cli.as_ref()
    }

    /// The runtime configuration, absent only in tests that build state directly.
    #[must_use]
    pub fn config(&self) -> Option<&AppConfig> {
        self.config.as_deref()
    }

    /// The process logging handle, absent only in tests that build state directly.
    #[must_use]
    pub fn logging(&self) -> Option<&at_store::logging::Logging> {
        self.logging.as_ref()
    }

    /// The single-instance state, absent only in tests that build state directly.
    #[must_use]
    pub fn instance(&self) -> Option<&InstanceState> {
        self.instance.as_deref()
    }
}
