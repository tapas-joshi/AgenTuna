//! Global scope for the Tauri shell: resolved path, availability, elevation and locks.
//!
//! [`GlobalStoreService`] is the single seam between `AppPaths`, the filesystem gate in
//! [`at_fs::privilege`] and the lock index. It holds no policy of its own:
//!
//! * the path and its source come from `at_store::AppPaths` (never hard-coded here);
//! * writes are refused with a typed read-only error before anything is staged;
//! * elevation runs through the injected [`Elevator`] exactly once, on the explicit
//!   "Enable global writes" action;
//! * locks are always read through [`GlobalLockReader`], i.e. from the `locks` table, and are
//!   reported even when the store is read-only or absent. The Global store is never a lock source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use at_fs::privilege::{
    Elevator, EnableError, EnableOutcome, GlobalStore, GlobalStoreStatus, GlobalWriteError,
    StoreOwner, enable_global_writes, platform_elevator,
};
use at_fs::{WriteOutcome, WriteRequest};
use at_model::Lock;
use at_store::paths::{AppPaths, GlobalStoreSource};

/// Reads Global locks from the `locks` table.
///
/// Implementations must query SQLite and must never read a lock file from the Global store.
/// The production implementation lands with TA-002 (`at-engine::locks`); the shell depends on
/// this trait so the UI path cannot grow a second lock source.
pub trait GlobalLockReader: Send + Sync {
    /// Every lock whose scope is Global, in a stable order.
    ///
    /// # Errors
    /// [`LockReadError`] when the lock index cannot be queried.
    fn global_locks(&self) -> Result<Vec<Lock>, LockReadError>;
}

impl<F> GlobalLockReader for F
where
    F: Fn() -> Result<Vec<Lock>, LockReadError> + Send + Sync,
{
    fn global_locks(&self) -> Result<Vec<Lock>, LockReadError> {
        self()
    }
}

/// Why Global locks could not be read. The message never contains lock contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("could not read Global locks from the locks table: {message}")]
pub struct LockReadError {
    /// Redacted, human-readable explanation.
    pub message: String,
}

impl LockReadError {
    /// Builds an error from a database explanation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Everything the Global Overview card needs, in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalOverview {
    /// Resolved Global store directory.
    pub path: PathBuf,
    /// Where [`Self::path`] came from (`environment`, `settings` or `default`).
    pub source: GlobalStoreSource,
    /// Current availability with its reason.
    pub status: GlobalStoreStatus,
    /// Locks from the `locks` table, shown regardless of [`Self::status`].
    pub locks: Vec<Lock>,
}

impl GlobalOverview {
    /// Whether the read-only banner and "Enable global writes" action are required.
    #[must_use]
    pub fn needs_elevation(&self) -> bool {
        !self.status.is_available()
    }
}

/// Shell-facing Global scope: path, status, guarded writes, one-shot elevation and locks.
pub struct GlobalStoreService {
    paths: AppPaths,
    elevator: Arc<dyn Elevator>,
}

impl GlobalStoreService {
    /// Builds the service from already-resolved paths and an authorization runner. Tests inject a
    /// recording [`Elevator`]; production uses [`Self::with_platform_elevator`].
    #[must_use]
    pub fn new(paths: AppPaths, elevator: Arc<dyn Elevator>) -> Self {
        Self { paths, elevator }
    }

    /// Builds the service with the operating system's consent-based elevator.
    #[must_use]
    pub fn with_platform_elevator(paths: AppPaths) -> Self {
        Self::new(paths, platform_elevator())
    }

    /// Every resolved root, including the settings override applied to the Global store.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// The resolved Global store directory. Displayed by the Global card.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.paths.global_store()
    }

    /// Where [`Self::path`] came from.
    #[must_use]
    pub fn source(&self) -> GlobalStoreSource {
        self.paths.global_store_source()
    }

    /// Applies the `global_store_path` settings value once the database has opened.
    ///
    /// `AGENTUNA_GLOBAL_STORE` still wins and a relative settings value resolves against the
    /// support directory. Returns whether the resolved path changed, so the caller can refresh the
    /// overview; this method never probes or writes the filesystem itself.
    pub fn apply_settings_global_store(&mut self, setting: Option<&Path>) -> bool {
        self.paths.apply_settings_global_store(setting)
    }

    /// Re-probes availability. Cheap enough to call on every overview refresh.
    #[must_use]
    pub fn status(&self) -> GlobalStoreStatus {
        GlobalStoreStatus::probe(self.path())
    }

    /// Reads availability and the Global locks in one call.
    ///
    /// The lock query is independent of the filesystem status: a read-only or absent store still
    /// lists its locks, because locks live in SQLite.
    ///
    /// # Errors
    /// [`LockReadError`] when the lock index cannot be queried.
    pub fn overview(&self, locks: &dyn GlobalLockReader) -> Result<GlobalOverview, LockReadError> {
        Ok(GlobalOverview {
            path: self.path().to_path_buf(),
            source: self.source(),
            status: self.status(),
            locks: locks.global_locks()?,
        })
    }

    /// Runs the explicit "Enable global writes" action: one privileged directory preparation
    /// behind the operating-system authorization prompt.
    ///
    /// No write intent calls this automatically; a read-only store returns a typed error instead.
    ///
    /// # Errors
    /// [`EnableError`] when the path is invalid, the prompt is cancelled or denied, elevation is
    /// unsupported, the privileged command fails, or success is not observable afterwards.
    pub fn enable_global_writes(&self) -> Result<EnableOutcome, EnableError> {
        enable_global_writes(self.path(), StoreOwner::current(), self.elevator.as_ref())
    }

    /// Writes inside the Global store after the read-only gate.
    ///
    /// # Errors
    /// [`GlobalWriteError::ReadOnly`] / [`GlobalWriteError::Unavailable`] before any temp file is
    /// staged, [`GlobalWriteError::OutsideStore`] for a foreign path, or the underlying
    /// [`at_fs::FsError`].
    pub fn write(&self, request: WriteRequest) -> Result<WriteOutcome, GlobalWriteError> {
        GlobalStore::new(self.path()).write(request)
    }
}
