//! Windows and Linux support, state and cache roots, resolved from injected environment values
//! (TF-037).
//!
//! `AppPaths` on macOS keeps its existing layout; this module adds the other platforms' rules
//! as pure functions so `AppPaths` can call them during the port, and so tests can assert every
//! path without changing the developer's environment.
//!
//! - macOS: support `~/Library/Application Support/AgenTuna`, cache `.../AgenTuna/cache`.
//! - Linux: `XDG_CONFIG_HOME` or `~/.config` + `agentuna` for support/configuration,
//!   `XDG_DATA_HOME` or `~/.local/share` + `agentuna` for local state,
//!   `XDG_CACHE_HOME` or `~/.cache` + `agentuna` for cache.
//! - Windows: `%APPDATA%\AgenTuna` for support, `%LOCALAPPDATA%\AgenTuna` for local state.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::pathkey::{PathKey, PortablePlatform};
use crate::paths::APP_DIR_NAME;

/// Directory name of the per-user support tree on Linux (lowercase, matching `~/.agentuna`).
pub const LINUX_APP_DIR_NAME: &str = "agentuna";

/// Every root AgenTuna needs on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRoots {
    /// The user's home directory.
    pub home: PathBuf,
    /// Support directory (database, logs, backups).
    pub support_dir: PathBuf,
    /// Local state directory; equals `support_dir` on macOS.
    pub local_state_dir: PathBuf,
    /// Cache directory.
    pub cache_dir: PathBuf,
}

/// Why the roots could not be resolved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformRootError {
    /// No home directory was supplied or discoverable.
    #[error("cannot determine the home directory for this platform")]
    HomeUnavailable,
    /// A required environment variable is unset or empty.
    #[error("{variable} is not set; it is required on this platform")]
    MissingVariable {
        /// The variable name.
        variable: &'static str,
    },
    /// An environment variable that must hold an absolute path was relative.
    #[error("{variable} must be an absolute path, found {value}")]
    RelativeVariable {
        /// The variable name.
        variable: &'static str,
        /// The rejected value.
        value: PathBuf,
    },
}

/// Resolves every root for `platform`.
///
/// `home` is normally `AppPaths`' injected home; when it is `None` the Windows path falls back
/// to `%USERPROFILE%` and Linux falls back to `$HOME` through `lookup`. Environment values are
/// read through `lookup`, never from the process, so tests inject their own roots.
///
/// # Errors
/// [`PlatformRootError::HomeUnavailable`] when a root needs a home and none is available,
/// [`PlatformRootError::MissingVariable`] for an unset `%APPDATA%`/`%LOCALAPPDATA%`, and
/// [`PlatformRootError::RelativeVariable`] for a relative value where an absolute one is
/// required.
pub fn resolve_platform_roots(
    platform: PortablePlatform,
    home: Option<&Path>,
    lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<PlatformRoots, PlatformRootError> {
    match platform {
        PortablePlatform::MacOs | PortablePlatform::Other => {
            macos_roots(home.ok_or(PlatformRootError::HomeUnavailable)?)
        }
        PortablePlatform::Linux => linux_roots(home, lookup),
        PortablePlatform::Windows => windows_roots(home, lookup),
    }
}

fn macos_roots(home: &Path) -> Result<PlatformRoots, PlatformRootError> {
    let support = home
        .join("Library")
        .join("Application Support")
        .join(APP_DIR_NAME);
    Ok(PlatformRoots {
        home: home.to_path_buf(),
        local_state_dir: support.clone(),
        cache_dir: support.join("cache"),
        support_dir: support,
    })
}

fn linux_roots(
    home: Option<&Path>,
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<PlatformRoots, PlatformRootError> {
    let config =
        xdg_root("XDG_CONFIG_HOME", home, ".config", &mut lookup)?.join(LINUX_APP_DIR_NAME);
    let data =
        xdg_root("XDG_DATA_HOME", home, ".local/share", &mut lookup)?.join(LINUX_APP_DIR_NAME);
    let cache = xdg_root("XDG_CACHE_HOME", home, ".cache", &mut lookup)?.join(LINUX_APP_DIR_NAME);
    Ok(PlatformRoots {
        home: home
            .ok_or(PlatformRootError::HomeUnavailable)?
            .to_path_buf(),
        support_dir: config,
        local_state_dir: data,
        cache_dir: cache,
    })
}

fn xdg_root(
    variable: &'static str,
    home: Option<&Path>,
    fallback: &str,
    lookup: &mut impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf, PlatformRootError> {
    // The XDG spec says a relative value is invalid and must be ignored, not an error.
    if let Some(raw) = lookup(variable) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() && path.is_absolute() {
            return Ok(path);
        }
    }
    let home = home.ok_or(PlatformRootError::HomeUnavailable)?;
    let mut path = home.to_path_buf();
    for part in fallback.split('/') {
        path.push(part);
    }
    Ok(path)
}

fn windows_roots(
    home: Option<&Path>,
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<PlatformRoots, PlatformRootError> {
    let home = match home {
        Some(home) => home.to_path_buf(),
        None => {
            env_windows("USERPROFILE", &mut lookup)?.ok_or(PlatformRootError::HomeUnavailable)?
        }
    };
    let roaming =
        env_windows("APPDATA", &mut lookup)?.ok_or(PlatformRootError::MissingVariable {
            variable: "APPDATA",
        })?;
    let local =
        env_windows("LOCALAPPDATA", &mut lookup)?.ok_or(PlatformRootError::MissingVariable {
            variable: "LOCALAPPDATA",
        })?;
    let support = join_child(&roaming, APP_DIR_NAME);
    let local_state = join_child(&local, APP_DIR_NAME);
    let cache = join_child(&local_state, "cache");
    Ok(PlatformRoots {
        home,
        support_dir: support,
        local_state_dir: local_state,
        cache_dir: cache,
    })
}

/// Reads a Windows path variable. The absolute check uses Windows rules even when the tests run
/// on macOS, so `C:\Users\a` is accepted and `relative` is not.
fn env_windows(
    variable: &'static str,
    lookup: &mut impl FnMut(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, PlatformRootError> {
    let Some(raw) = lookup(variable) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    let text = path.to_string_lossy();
    if PathKey::from_text(&text, PortablePlatform::Windows).is_err() {
        return Err(PlatformRootError::RelativeVariable {
            variable,
            value: path,
        });
    }
    Ok(Some(path))
}

/// Appends a child with a `\` separator when the base already uses Windows separators, so the
/// pure function returns the documented Windows path even when called from a Unix test host.
pub(crate) fn join_child(base: &Path, name: &str) -> PathBuf {
    let text = base.to_string_lossy();
    if text.contains('\\') {
        PathBuf::from(format!("{}\\{name}", text.trim_end_matches('\\')))
    } else {
        base.join(name)
    }
}
