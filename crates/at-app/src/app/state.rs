//! Managed application state shared by Tauri commands.

use std::sync::{Arc, Mutex, MutexGuard};

use at_db::Database;

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
    database: Option<Arc<Mutex<Database>>>,
    logging: Option<at_store::logging::Logging>,
    recovery: Option<Arc<super::recovery::RecoveryService>>,
    recovery_outcome: Option<super::recovery::RecoveryOutcome>,
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
            database: None,
            logging: None,
            recovery: None,
            recovery_outcome: None,
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
    /// Attaches the shared SQLite database service.
    #[must_use]
    pub fn with_database(mut self, database: Database) -> Self {
        self.database = Some(Arc::new(Mutex::new(database)));
        self
    }

    /// Locks the shared database for a typed command operation.
    ///
    /// # Errors
    /// Returns an error when the database was not configured or its mutex was poisoned.
    pub fn database(&self) -> Result<MutexGuard<'_, Database>, &'static str> {
        let database = self.database.as_ref().ok_or("database unavailable")?;
        database.lock().map_err(|_| "database lock poisoned")
    }
    /// Clones the shared database handle for a background job.
    ///
    /// # Errors
    /// Returns an error when the database was not configured.
    pub fn database_handle(&self) -> Result<Arc<Mutex<Database>>, &'static str> {
        self.database.clone().ok_or("database unavailable")
    }

    /// Attaches the single-instance state that owns the record and activation listener.
    #[must_use]
    pub fn with_instance(mut self, instance: Arc<InstanceState>) -> Self {
        self.instance = Some(instance);
        self
    }
    /// Attaches the startup recovery service and its first-run result.
    #[must_use]
    pub fn with_recovery(
        mut self,
        service: Arc<super::recovery::RecoveryService>,
        outcome: super::recovery::RecoveryOutcome,
    ) -> Self {
        self.recovery = Some(service);
        self.recovery_outcome = Some(outcome);
        self
    }

    /// Startup recovery state, when the service was configured.
    #[must_use]
    pub fn recovery_outcome(&self) -> Option<&super::recovery::RecoveryOutcome> {
        self.recovery_outcome.as_ref()
    }

    /// The recovery service used by Restore and Discard commands.
    #[must_use]
    pub fn recovery(&self) -> Option<&super::recovery::RecoveryService> {
        self.recovery.as_deref()
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
