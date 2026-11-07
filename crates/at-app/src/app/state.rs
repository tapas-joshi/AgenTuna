//! Managed application state shared by Tauri commands.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use at_adapters::adapter::AgentAdapterRegistry;
use at_db::Database;
use at_fs::RecentWrites;

use super::bridge::JobBridge;
use super::cli_install::CliInstaller;
use super::config::AppConfig;
use super::instance::InstanceState;

/// Failure to load cached scope snapshots for an application query.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeCacheError {
    /// Resolved application paths were not attached to the state.
    #[error("application paths are unavailable")]
    PathsUnavailable,
    /// Resolving the selected project paths failed.
    #[error(transparent)]
    Paths(#[from] at_store::paths::PathError),
    /// A store-level scope load failed.
    #[error(transparent)]
    Loader(#[from] at_engine::loader::LoadError),
    /// The in-process scope cache mutex was poisoned.
    #[error("scope snapshot cache is unavailable")]
    CachePoisoned,
}

/// Process-wide services shared by every Tauri command.
pub struct AppState {
    bridge: Arc<JobBridge>,
    cli: Option<CliInstaller>,
    config: Option<Arc<AppConfig>>,
    instance: Option<Arc<InstanceState>>,
    database: Option<Arc<Mutex<Database>>>,
    recent_writes: Arc<RecentWrites>,
    adapters: AgentAdapterRegistry,
    logging: Option<at_store::logging::Logging>,
    recovery: Option<Arc<super::recovery::RecoveryService>>,
    recovery_outcome: Option<super::recovery::RecoveryOutcome>,
    paths: Option<at_store::paths::AppPaths>,
    scope_cache: Mutex<Option<(Option<PathBuf>, at_engine::loader::LoadedScopes)>>,
    launch_approvals: super::mcp_launch::McpLaunchApprovals,
    startup_warnings: Vec<super::startup::StartupWarningView>,
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
            recent_writes: Arc::new(RecentWrites::new()),
            adapters: AgentAdapterRegistry::register(),
            logging: None,
            recovery: None,
            recovery_outcome: None,
            paths: None,
            scope_cache: Mutex::new(None),
            launch_approvals: super::mcp_launch::McpLaunchApprovals::new(),
            startup_warnings: Vec::new(),
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

    /// Attaches an already-shared database handle, used when another service (the job bridge's
    /// operations log) writes to the same connection.
    #[must_use]
    pub fn with_database_handle(mut self, database: Arc<Mutex<Database>>) -> Self {
        self.database = Some(database);
        self
    }

    /// Attaches the non-blocking warnings recorded while the database was opened.
    #[must_use]
    pub fn with_startup_warnings(
        mut self,
        warnings: Vec<super::startup::StartupWarningView>,
    ) -> Self {
        self.startup_warnings = warnings;
        self
    }

    /// Warnings recorded at startup: a quarantine, a failed rebuild or an unreadable index.
    #[must_use]
    pub fn startup_warnings(&self) -> &[super::startup::StartupWarningView] {
        &self.startup_warnings
    }

    /// Attaches the resolved, injectable filesystem roots used by read-only engine queries.
    #[must_use]
    pub fn with_paths(mut self, paths: at_store::paths::AppPaths) -> Self {
        self.paths = Some(paths);
        self
    }
    /// The resolved application paths, absent in tests that do not configure stores.
    #[must_use]
    pub fn paths(&self) -> Option<&at_store::paths::AppPaths> {
        self.paths.as_ref()
    }

    /// Loads or reuses scope snapshots for the selected project.
    ///
    /// # Errors
    /// Returns a typed error if paths are unavailable, path resolution fails, the cache is
    /// poisoned, or a store-level load fails.
    pub fn load_scope_snapshots(
        &self,
        active_project: Option<&Path>,
    ) -> Result<at_engine::loader::LoadedScopes, ScopeCacheError> {
        let base = self
            .paths
            .as_ref()
            .ok_or(ScopeCacheError::PathsUnavailable)?;
        let paths = match active_project {
            Some(root) => base.for_project(root).map_err(ScopeCacheError::Paths)?,
            None => base.clone(),
        };
        let key = active_project.map(Path::to_path_buf);
        let mut cache = self
            .scope_cache
            .lock()
            .map_err(|_| ScopeCacheError::CachePoisoned)?;
        if let Some((cached_key, scopes)) = cache.as_ref()
            && cached_key == &key
        {
            return Ok(scopes.clone());
        }
        let scopes = at_engine::loader::load_scope_snapshots(&paths, active_project)
            .map_err(ScopeCacheError::Loader)?;
        *cache = Some((key, scopes.clone()));
        Ok(scopes)
    }

    /// Invalidates cached scope snapshots after a store refresh or write.
    pub fn invalidate_scope_cache(&self) {
        if let Ok(mut cache) = self.scope_cache.lock() {
            *cache = None;
        }
    }

    /// Shared registry used to suppress watcher events caused by AgenTuna's own writes.
    #[must_use]
    pub fn recent_writes(&self) -> Arc<RecentWrites> {
        Arc::clone(&self.recent_writes)
    }

    /// The process-wide agent adapter registry.
    ///
    /// A custom agent is inserted here when it is created, so the Effective agent filter
    /// observes it without an application restart (TA-029).
    #[must_use]
    pub fn adapters(&self) -> &AgentAdapterRegistry {
        &self.adapters
    }

    /// Records successful writes from an applied ChangeSet for filesystem-event suppression.
    pub fn record_writes(&self, set: &at_model::ChangeSet, store_root: &Path) {
        for operation in &set.ops {
            let Some(path) = operation.destination() else {
                continue;
            };
            let destination = if path.is_absolute() {
                path.to_path_buf()
            } else {
                store_root.join(path)
            };
            let hash = match operation {
                at_model::FileOp::Write { bytes, .. } => at_fs::sha256_hex(bytes),
                at_model::FileOp::Delete { .. } | at_model::FileOp::Mkdir { .. } => continue,
                _ => {
                    let Ok(bytes) = std::fs::read(&destination) else {
                        continue;
                    };
                    at_fs::sha256_hex(&bytes)
                }
            };
            self.recent_writes.record(destination, hash);
        }
        self.invalidate_scope_cache();
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

    /// The process-wide registry of one-shot MCP launch approvals.
    #[must_use]
    pub fn launch_approvals(&self) -> &super::mcp_launch::McpLaunchApprovals {
        &self.launch_approvals
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use at_fs::{WatchConfig, WatchEvent, WatchRoot, WatchRootKind, WatchService};
    use at_model::{ChangeSet, ExpectedState, FileOp, OperationKind};

    use super::AppState;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};

    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    #[test]
    fn app_writes_are_suppressed_but_external_writes_are_reported() {
        let temp = tempfile::tempdir().expect("temp");
        // Watcher events use canonical paths (macOS temp dirs live under /private/var).
        let root = std::fs::canonicalize(temp.path())
            .expect("canonical temp")
            .join("agent");
        std::fs::create_dir_all(&root).expect("root");
        let state = AppState::new(
            Arc::new(JobBridge::new(
                Arc::new(NullSink),
                Arc::new(TracingOperationsLog),
            )),
            None,
        );
        let file = root.join("settings.json");
        std::fs::write(&file, b"before").expect("seed");
        let service = WatchService::new(
            WatchConfig::new([WatchRoot::dir(&root, WatchRootKind::SkillsDir)])
                .with_debounce(Duration::from_millis(50))
                .with_registry(state.recent_writes()),
        );
        std::thread::sleep(Duration::from_millis(2_500));
        service.start().expect("start");

        let app_bytes = b"app write".to_vec();
        std::fs::write(&file, &app_bytes).expect("app write");
        let change_set = ChangeSet {
            id: "test".to_owned(),
            preview_id: "test-preview".to_owned(),
            operation_kind: OperationKind::parse("edit_skill").expect("operation"),
            scope: None,
            project_id: None,
            summary: "test".to_owned(),
            ops: vec![FileOp::Write {
                path: PathBuf::from("settings.json"),
                bytes: app_bytes,
                expected: ExpectedState::absent(),
                follow_symlink: false,
                backup: true,
            }],
            created_at_unix_seconds: 0,
        };
        state.record_writes(&change_set, &root);

        let deadline = Instant::now() + Duration::from_secs(3);
        while state.recent_writes().suppressed_count() == 0 && Instant::now() < deadline {
            let _ = service.next_event(Duration::from_millis(50));
        }
        assert_eq!(state.recent_writes().suppressed_count(), 1);

        std::fs::write(&file, b"external write").expect("external write");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut external_batch = false;
        while Instant::now() < deadline {
            if matches!(
                service.next_event(Duration::from_millis(100)),
                Some(WatchEvent::Batch(batch)) if batch.contains(&file)
            ) {
                external_batch = true;
                break;
            }
        }
        assert!(external_batch, "external write should reach the watcher");
        service.stop().expect("stop");
    }

    #[test]
    fn scope_snapshot_cache_uses_injected_home_and_project_paths() {
        let home = at_fs::testkit::TestHome::new().expect("temporary home");
        let project = home.project("repo").expect("temporary project");
        std::fs::create_dir_all(project.root().join(".git")).expect("git directory");
        let paths = at_store::paths::AppPaths::resolve(
            &at_store::paths::PathEnvironment::default()
                .with_home(home.home())
                .with_project_root(project.root()),
        )
        .expect("injected app paths");
        let state = AppState::new(
            Arc::new(JobBridge::new(
                Arc::new(NullSink),
                Arc::new(TracingOperationsLog),
            )),
            None,
        )
        .with_paths(paths);
        let loaded = state
            .load_scope_snapshots(Some(project.root()))
            .expect("load injected stores");
        assert_eq!(loaded.layers.len(), 4);
        assert!(
            loaded
                .layers
                .iter()
                .all(|layer| layer.definitions.is_empty())
        );
        state.invalidate_scope_cache();
        let refreshed = state
            .load_scope_snapshots(Some(project.root()))
            .expect("reload injected stores");
        assert_eq!(refreshed.layers.len(), 4);
    }
}
