//! Managed application state shared by Tauri commands.

use std::sync::Arc;

use super::bridge::JobBridge;
use super::cli_install::CliInstaller;

/// Process-wide services shared by every Tauri command.
pub struct AppState {
    bridge: Arc<JobBridge>,
    cli: Option<CliInstaller>,
}

impl AppState {
    /// Creates state from the async job bridge and an optional packaged-CLI installer.
    #[must_use]
    pub fn new(bridge: Arc<JobBridge>, cli: Option<CliInstaller>) -> Self {
        Self { bridge, cli }
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
}
