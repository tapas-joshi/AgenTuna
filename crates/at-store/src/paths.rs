//! Every filesystem root AgenTuna uses, resolved once and passed by reference.
//!
//! This module is the only place that turns the process environment into paths. Everything
//! else receives an [`AppPaths`]; nothing else may read `HOME`, call `dirs::home_dir` or build
//! a store path itself. Tests inject [`PathEnvironment`] and never touch the developer's home.
//!
//! [`AppPaths::resolve`] selects the host platform and delegates the layout to
//! [`crate::platform_roots`]; [`AppPaths::resolve_for`] takes the platform and an environment
//! lookup explicitly so a Linux or Windows layout can be asserted from any host.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::pathkey::PortablePlatform;
use crate::platform_roots::{PlatformRootError, join_child, resolve_platform_roots};

/// Directory name of the per-user and per-project stores (`.agentuna`).
pub const STORE_DIR_NAME: &str = ".agentuna";

/// Application directory name used for the per-user support directory.
pub const APP_DIR_NAME: &str = "AgenTuna";

/// File name of the SQLite index inside the support directory.
pub const DB_FILE_NAME: &str = "agentuna.db";

/// Directory name for logs under the support directory.
pub const LOG_DIR_NAME: &str = "logs";

/// Directory name for backups under the support directory.
pub const BACKUP_DIR_NAME: &str = "backups";

/// Directory name for caches under the support directory.
pub const CACHE_DIR_NAME: &str = "cache";

/// Machine-wide Global store used when neither the environment nor settings override it.
///
/// A leading slash is deliberate: the Global scope is machine-wide, so its default is the
/// system `Application Support` directory, not the per-user one.
pub const DEFAULT_GLOBAL_STORE: &str = "/Library/Application Support/AgenTuna/global";

/// Environment variable relocating the support directory (db, logs, backups, cache).
pub const ENV_HOME: &str = "AGENTUNA_HOME";

/// Environment variable overriding the per-user store.
pub const ENV_USER_STORE: &str = "AGENTUNA_USER_STORE";

/// Environment variable overriding the per-project store.
pub const ENV_PROJECT_STORE: &str = "AGENTUNA_PROJECT_STORE";

/// Environment variable overriding the Global store.
pub const ENV_GLOBAL_STORE: &str = "AGENTUNA_GLOBAL_STORE";

/// Settings key that may override the Global store once the database is open.
pub const SETTINGS_KEY_GLOBAL_STORE: &str = "global_store_path";

/// Where [`AppPaths::global_store`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GlobalStoreSource {
    /// `AGENTUNA_GLOBAL_STORE`.
    Environment,
    /// Settings key [`SETTINGS_KEY_GLOBAL_STORE`], applied after the database opened.
    Settings,
    /// [`DEFAULT_GLOBAL_STORE`].
    Default,
}

impl GlobalStoreSource {
    /// Stable identifier for logs, diagnostics and settings UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Settings => "settings",
            Self::Default => "default",
        }
    }
}

/// Why [`AppPaths`] could not be resolved or created.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// The user's home directory is unknown and no environment override supplies the missing root.
    #[error(
        "cannot determine the home directory; set {ENV_HOME} and {ENV_USER_STORE} to run without one"
    )]
    HomeUnavailable,
    /// A project root was supplied but does not exist.
    #[error("{}: project root does not exist", .path.display())]
    ProjectRootMissing {
        /// The missing root.
        path: PathBuf,
    },
    /// A project root was supplied but is not a directory.
    #[error("{}: project root is not a directory", .path.display())]
    ProjectRootNotDirectory {
        /// The offending path.
        path: PathBuf,
    },
    /// A project store override was supplied without a project root.
    #[error("{}: a project store override requires a project root", .path.display())]
    ProjectRootRequired {
        /// The store path that was overridden.
        path: PathBuf,
    },
    /// A directory could not be created.
    #[error("{}: could not create directory: {source}", .path.display())]
    CreateDir {
        /// Directory being created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A platform-specific root could not be resolved (missing `%APPDATA%`, a relative XDG
    /// value, ...). A missing home is reported as [`PathError::HomeUnavailable`] instead.
    #[error(transparent)]
    PlatformRoot(#[from] PlatformRootError),
}

/// Explicit path overrides, normally read from the process environment.
///
/// Tests build one with [`PathOverrides::from_lookup`] so nothing depends on the real
/// environment; [`PathOverrides::from_env`] is the production entry point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathOverrides {
    /// `AGENTUNA_HOME`: replaces the support directory.
    pub home: Option<PathBuf>,
    /// `AGENTUNA_USER_STORE`: replaces `<home>/.agentuna`.
    pub user_store: Option<PathBuf>,
    /// `AGENTUNA_PROJECT_STORE`: replaces `<project>/.agentuna`.
    pub project_store: Option<PathBuf>,
    /// `AGENTUNA_GLOBAL_STORE`: replaces [`DEFAULT_GLOBAL_STORE`].
    pub global_store: Option<PathBuf>,
}

impl PathOverrides {
    /// Reads the overrides from the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// Reads the overrides through `lookup`, ignoring variables that are unset or empty.
    #[must_use]
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Self {
        let mut path = |name: &str| {
            lookup(name)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        };
        Self {
            home: path(ENV_HOME),
            user_store: path(ENV_USER_STORE),
            project_store: path(ENV_PROJECT_STORE),
            global_store: path(ENV_GLOBAL_STORE),
        }
    }
}

/// Where the home directory comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HomeSource {
    /// Ask the `directories` crate for the process home (production).
    #[default]
    Process,
    /// Use this directory instead (tests and sandboxes).
    Injected(PathBuf),
    /// No home exists; roots that need it fail with [`PathError::HomeUnavailable`].
    Unavailable,
}

impl HomeSource {
    fn resolve(&self) -> Option<PathBuf> {
        match self {
            Self::Process => process_home(),
            Self::Injected(home) => Some(home.clone()),
            Self::Unavailable => None,
        }
    }
}

/// Inputs for [`AppPaths::resolve`]: where home comes from, explicit overrides and the selected
/// project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathEnvironment {
    /// Source of the home directory.
    pub home: HomeSource,
    /// Explicit root overrides.
    pub overrides: PathOverrides,
    /// Project root when a project is selected.
    pub project_root: Option<PathBuf>,
}

impl PathEnvironment {
    /// Production input: process home, process environment, no project yet.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            home: HomeSource::Process,
            overrides: PathOverrides::from_env(),
            project_root: None,
        }
    }

    /// Replaces the home directory.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = HomeSource::Injected(home.into());
        self
    }

    /// Declares that no home directory exists.
    #[must_use]
    pub fn without_home(mut self) -> Self {
        self.home = HomeSource::Unavailable;
        self
    }

    /// Selects a project root.
    #[must_use]
    pub fn with_project_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.project_root = Some(root.into());
        self
    }

    /// Replaces the overrides.
    #[must_use]
    pub fn with_overrides(mut self, overrides: PathOverrides) -> Self {
        self.overrides = overrides;
        self
    }
}

/// Every filesystem root AgenTuna uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    home: Option<PathBuf>,
    support_dir: PathBuf,
    local_state_dir: PathBuf,
    db_path: PathBuf,
    log_dir: PathBuf,
    backup_dir: PathBuf,
    cache_dir: PathBuf,
    user_store: PathBuf,
    project_store: Option<PathBuf>,
    global_store: PathBuf,
    default_global_store: PathBuf,
    global_store_source: GlobalStoreSource,
}

impl AppPaths {
    /// Resolves every root for the host platform.
    ///
    /// The Global store starts from [`DEFAULT_GLOBAL_STORE`] or `AGENTUNA_GLOBAL_STORE`; a
    /// settings override is applied later with [`Self::apply_settings_global_store`] because
    /// settings only become readable once the database is open.
    ///
    /// The per-platform layout lives in [`crate::platform_roots`]; this method only selects the
    /// host platform and the process environment.
    ///
    /// # Errors
    /// [`PathError::HomeUnavailable`] when a root needs `$HOME` and neither an injected home nor
    /// a covering override exists; [`PathError::ProjectRootMissing`] or
    /// [`PathError::ProjectRootNotDirectory`] when a supplied project root is invalid;
    /// [`PathError::ProjectRootRequired`] when a project store is overridden without one;
    /// [`PathError::PlatformRoot`] when a required platform variable is missing or malformed.
    pub fn resolve(env: &PathEnvironment) -> Result<Self, PathError> {
        Self::resolve_for(PortablePlatform::current(), env, |name| {
            std::env::var_os(name)
        })
    }

    /// Resolves every root using `platform`'s rules, reading root variables through `lookup`.
    ///
    /// Production goes through [`Self::resolve`]; tests and the portability checks call this
    /// with an injected home and root values so a Linux or Windows layout can be asserted from
    /// any host without touching the developer's environment.
    ///
    /// # Errors
    /// As [`Self::resolve`].
    pub fn resolve_for(
        platform: PortablePlatform,
        env: &PathEnvironment,
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<Self, PathError> {
        let mut home = env.home.resolve();

        let (support_dir, local_state_dir, cache_dir) = match &env.overrides.home {
            Some(dir) => (dir.clone(), dir.clone(), join_child(dir, CACHE_DIR_NAME)),
            None => {
                let roots = resolve_platform_roots(platform, home.as_deref(), &mut lookup)
                    .map_err(|source| match source {
                        PlatformRootError::HomeUnavailable => PathError::HomeUnavailable,
                        other => PathError::PlatformRoot(other),
                    })?;
                if home.is_none() {
                    // Windows resolves `%USERPROFILE%` inside the platform module.
                    home = Some(roots.home.clone());
                }
                (roots.support_dir, roots.local_state_dir, roots.cache_dir)
            }
        };

        let user_store = match &env.overrides.user_store {
            Some(dir) => dir.clone(),
            None => join_child(
                home.as_ref().ok_or(PathError::HomeUnavailable)?,
                STORE_DIR_NAME,
            ),
        };

        let project_store = match (&env.project_root, &env.overrides.project_store) {
            (Some(root), override_path) => {
                check_project_root(root)?;
                Some(
                    override_path
                        .clone()
                        .unwrap_or_else(|| join_child(root, STORE_DIR_NAME)),
                )
            }
            (None, Some(dir)) => {
                return Err(PathError::ProjectRootRequired { path: dir.clone() });
            }
            (None, None) => None,
        };

        let (global_store, global_store_source) = match &env.overrides.global_store {
            Some(dir) => (dir.clone(), GlobalStoreSource::Environment),
            None => (
                PathBuf::from(DEFAULT_GLOBAL_STORE),
                GlobalStoreSource::Default,
            ),
        };

        Ok(Self {
            home,
            db_path: join_child(&support_dir, DB_FILE_NAME),
            log_dir: join_child(&support_dir, LOG_DIR_NAME),
            backup_dir: join_child(&support_dir, BACKUP_DIR_NAME),
            cache_dir,
            local_state_dir,
            support_dir,
            user_store,
            project_store,
            default_global_store: PathBuf::from(DEFAULT_GLOBAL_STORE),
            global_store,
            global_store_source,
        })
    }

    /// Home directory of the user, when it is known.
    #[must_use]
    pub fn home_dir(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// Returns equivalent resolved paths with `project_root` selected.
    ///
    /// Every other root is kept exactly as resolved, including an injected XDG or `%APPDATA%`
    /// lookup, so switching projects never re-reads the environment. An existing project store
    /// override is not carried over: the selected project's store is `<project>/.agentuna`.
    ///
    /// # Errors
    /// [`PathError::ProjectRootMissing`] or [`PathError::ProjectRootNotDirectory`] when
    /// `project_root` is not an existing directory.
    pub fn for_project(&self, project_root: &Path) -> Result<Self, PathError> {
        check_project_root(project_root)?;
        Ok(Self {
            project_store: Some(join_child(project_root, STORE_DIR_NAME)),
            ..self.clone()
        })
    }

    /// Support directory holding the database, logs, backups and cache.
    #[must_use]
    pub fn support_dir(&self) -> &Path {
        &self.support_dir
    }

    /// Local state directory (`<support>` on macOS; `$XDG_DATA_HOME/agentuna` on Linux;
    /// `%LOCALAPPDATA%\AgenTuna` on Windows).
    #[must_use]
    pub fn local_state_dir(&self) -> &Path {
        &self.local_state_dir
    }

    /// SQLite index path (`<support>/agentuna.db`).
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Log directory (`<support>/logs`).
    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Backup root (`<support>/backups`).
    #[must_use]
    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// Cache directory (`<support>/cache`).
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Per-user store (`~/.agentuna`).
    #[must_use]
    pub fn user_store(&self) -> &Path {
        &self.user_store
    }

    /// Per-project store (`<project>/.agentuna`) when a project is selected.
    #[must_use]
    pub fn project_store(&self) -> Option<&Path> {
        self.project_store.as_deref()
    }

    /// Machine-wide Global store.
    #[must_use]
    pub fn global_store(&self) -> &Path {
        &self.global_store
    }

    /// Where [`Self::global_store`] came from.
    #[must_use]
    pub fn global_store_source(&self) -> GlobalStoreSource {
        self.global_store_source
    }

    /// Applies the `global_store_path` settings value after the database opened.
    ///
    /// `AGENTUNA_GLOBAL_STORE` always wins, so this is a no-op under it. A relative settings path
    /// is resolved against the support directory. Passing `None` reverts a previously applied
    /// settings value to the default.
    ///
    /// Returns whether the Global store changed.
    pub fn apply_settings_global_store(&mut self, setting: Option<&Path>) -> bool {
        if self.global_store_source == GlobalStoreSource::Environment {
            return false;
        }
        let (next, source) = match setting {
            Some(path) => (
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.support_dir.join(path)
                },
                GlobalStoreSource::Settings,
            ),
            None => (
                self.default_global_store.clone(),
                GlobalStoreSource::Default,
            ),
        };
        if self.global_store == next && self.global_store_source == source {
            return false;
        }
        self.global_store = next;
        self.global_store_source = source;
        true
    }

    /// Creates the directories AgenTuna owns: support, logs, backups, cache (0700), the user
    /// store (0700) and, when a project is selected, the project store (0755).
    ///
    /// The Global store is not created: by default it is machine-owned, and writing it is an
    /// explicit administrative action rather than part of first-run setup.
    ///
    /// # Errors
    /// [`PathError::CreateDir`] with the failing path.
    pub fn create_dirs(&self) -> Result<(), PathError> {
        create_dir(&self.support_dir, 0o700)?;
        create_dir(&self.local_state_dir, 0o700)?;
        create_dir(&self.log_dir, 0o700)?;
        create_dir(&self.backup_dir, 0o700)?;
        create_dir(&self.cache_dir, 0o700)?;
        create_dir(&self.user_store, 0o700)?;
        if let Some(project_store) = &self.project_store {
            create_dir(project_store, 0o755)?;
        }
        Ok(())
    }
}

/// The process home directory, via the `directories` crate.
fn process_home() -> Option<PathBuf> {
    BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

fn check_project_root(root: &Path) -> Result<(), PathError> {
    let metadata = std::fs::metadata(root).map_err(|_| PathError::ProjectRootMissing {
        path: root.to_path_buf(),
    })?;
    if !metadata.is_dir() {
        return Err(PathError::ProjectRootNotDirectory {
            path: root.to_path_buf(),
        });
    }
    Ok(())
}

fn create_dir(path: &Path, mode: u32) -> Result<(), PathError> {
    at_fs::ensure_dir_with_mode(path, Some(mode)).map_err(|err| PathError::CreateDir {
        path: path.to_path_buf(),
        source: match err {
            at_fs::FsError::Io { source, .. } => source,
            other => std::io::Error::other(other.to_string()),
        },
    })
}
