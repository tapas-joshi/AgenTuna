//! Packaged `agentuna` CLI: locate the bundled binary and install or remove its PATH symlink.
//!
//! The CLI ships at `AgenTuna.app/Contents/Resources/bin/agentuna`. [`CliInstaller`] exposes
//! the two supported link locations (`/usr/local/bin` and `~/.local/bin`), refuses to
//! overwrite anything it does not own, and never edits shell profile files: the UI shows the
//! exact export line instead. Every path is injectable so tests stay inside a temporary
//! sandbox.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use rustix::fs::{Access, access};

/// Name of the packaged command-line tool.
pub const CLI_NAME: &str = "agentuna";
/// Default install directory that is already on most macOS login shells' `PATH`.
pub const PRIMARY_DIRECTORY: &str = "/usr/local/bin";

/// Where the CLI symlink may be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum CliInstallLocation {
    /// `/usr/local/bin`, the preferred location when the user may write to it.
    Primary,
    /// `~/.local/bin`, offered when the primary directory is not writable.
    Fallback,
}

/// Typed failure from the packaged CLI installer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CliInstallError {
    /// The bundled binary is not where the app bundle layout puts it.
    #[error("bundled CLI binary not found: {path}")]
    BundledBinaryMissing {
        /// Expected path.
        path: PathBuf,
    },
    /// A non-symlink file already occupies the link path.
    #[error("refusing to overwrite a non-symlink file: {path}")]
    ForeignFile {
        /// Offending path.
        path: PathBuf,
    },
    /// A symlink pointing somewhere else already occupies the link path.
    #[error("refusing to replace symlink {path}; it points to {target}")]
    ForeignSymlink {
        /// Offending link path.
        path: PathBuf,
        /// Where the foreign link points.
        target: PathBuf,
    },
    /// The directory exists but the process may not create entries in it.
    #[error("directory is not writable: {path}")]
    NotWritable {
        /// Offending directory.
        path: PathBuf,
    },
    /// The process has no `HOME`, so `~/.local/bin` is unknown.
    #[error("HOME is not set; cannot locate ~/.local/bin")]
    HomeMissing,
    /// A filesystem operation failed.
    #[error("filesystem error at {path}: {message}")]
    Io {
        /// Path the operation targeted.
        path: PathBuf,
        /// OS error text.
        message: String,
    },
}

/// One candidate install directory, presented to the install dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CliLocation {
    /// Directory that would receive the `agentuna` symlink.
    pub directory: String,
    /// Full link path (`<directory>/agentuna`).
    pub link_path: String,
    /// True when the process may create entries in the directory (or its parent, if the
    /// directory does not exist yet).
    pub writable: bool,
    /// True when the directory is already on the current `PATH`.
    pub on_path: bool,
    /// Exact shell line that puts the directory on `PATH`, when it is not there yet.
    pub path_export: Option<String>,
}

/// An existing symlink that points at this app's bundled binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CliInstallation {
    /// Installed link path.
    pub link_path: String,
    /// Resolved path the link points at.
    pub target: String,
}

/// State shown by `Help > Install Command Line Tool` before the user chooses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CliInstallStatus {
    /// Version of the bundled tool (matches the app version).
    pub bundled_version: String,
    /// Absolute path of the bundled binary.
    pub bundled_binary: String,
    /// Preferred location: `/usr/local/bin`.
    pub primary: CliLocation,
    /// Fallback location: `~/.local/bin`.
    pub fallback: CliLocation,
    /// A valid installation found at either location.
    pub installed: Option<CliInstallation>,
}

/// Result of a successful install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CliInstallReport {
    /// Created (or already present) link path.
    pub link_path: String,
    /// Resolved path the link points at.
    pub target: String,
    /// False when the correct link already existed and was left in place.
    pub created: bool,
    /// Raw `--version` output of the installed tool, when it could be executed.
    pub reported_version: Option<String>,
    /// True when the installed tool reports the same version as the app.
    pub matches_app_version: bool,
}

/// Result of an uninstall attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CliUninstallReport {
    /// Link path that was inspected.
    pub link_path: String,
    /// True when the link belonged to this app and was removed.
    pub removed: bool,
    /// Why nothing was removed; foreign files and symlinks are never touched.
    pub warning: Option<String>,
}

/// Locates the bundled CLI and manages its PATH symlink.
#[derive(Debug, Clone)]
pub struct CliInstaller {
    bundled_binary: PathBuf,
    home: PathBuf,
    primary_dir: PathBuf,
    path_var: Option<OsString>,
    version: String,
}

impl CliInstaller {
    /// Creates an installer for an explicit bundle layout (used by tests).
    #[must_use]
    pub fn new(bundled_binary: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            bundled_binary: absolutize(bundled_binary.into()),
            home: absolutize(home.into()),
            primary_dir: PathBuf::from(PRIMARY_DIRECTORY),
            path_var: std::env::var_os("PATH"),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Creates an installer for the running app bundle.
    ///
    /// # Errors
    ///
    /// [`CliInstallError::BundledBinaryMissing`] when neither
    /// `Contents/Resources/bin/agentuna` nor a sibling `agentuna` executable exists, and
    /// [`CliInstallError::HomeMissing`] when `HOME` is unset.
    pub fn for_current_app() -> Result<Self, CliInstallError> {
        let exe = std::env::current_exe().map_err(|error| CliInstallError::Io {
            path: PathBuf::from("<current executable>"),
            message: error.to_string(),
        })?;
        let bundled = exe
            .parent()
            .and_then(Path::parent)
            .map(|contents| contents.join("Resources").join("bin").join(CLI_NAME))
            .filter(|candidate| candidate.is_file())
            .unwrap_or_else(|| exe.with_file_name(CLI_NAME));
        if !bundled.is_file() {
            return Err(CliInstallError::BundledBinaryMissing { path: bundled });
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(CliInstallError::HomeMissing)?;
        Ok(Self::new(bundled, home))
    }

    /// Overrides the primary directory (defaults to `/usr/local/bin`).
    #[must_use]
    pub fn with_primary_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.primary_dir = absolutize(directory.into());
        self
    }

    /// Overrides the `PATH` used to decide whether an export line is needed.
    #[must_use]
    pub fn with_path_var(mut self, path: Option<OsString>) -> Self {
        self.path_var = path;
        self
    }

    /// Absolute path of the bundled binary.
    #[must_use]
    pub fn bundled_binary(&self) -> &Path {
        &self.bundled_binary
    }

    /// Version string shared with the app.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Describes both install locations and any existing valid installation.
    #[must_use]
    pub fn status(&self) -> CliInstallStatus {
        let primary = self.location(&self.primary_dir);
        let fallback = self.location(&self.fallback_dir());
        let installed = self
            .installed_at(&primary)
            .or_else(|| self.installed_at(&fallback));
        CliInstallStatus {
            bundled_version: self.version.clone(),
            bundled_binary: display_path(&self.bundled_binary),
            primary,
            fallback,
            installed,
        }
    }

    /// Installs a symlink at `location`. Idempotent: an existing link to this app's binary
    /// is reported with `created: false` and left untouched.
    ///
    /// # Errors
    ///
    /// Refuses to replace a foreign file or symlink, an unwritable directory, or a missing
    /// bundled binary.
    pub fn install(
        &self,
        location: CliInstallLocation,
    ) -> Result<CliInstallReport, CliInstallError> {
        if !self.bundled_binary.is_file() {
            return Err(CliInstallError::BundledBinaryMissing {
                path: self.bundled_binary.clone(),
            });
        }
        let directory = self.directory(location);
        let link = directory.join(CLI_NAME);

        if !directory.exists() {
            at_fs::ensure_dir(&directory).map_err(|error| CliInstallError::Io {
                path: directory.clone(),
                message: error.to_string(),
            })?;
        }
        if !directory_writable(&directory) {
            return Err(CliInstallError::NotWritable { path: directory });
        }

        match std::fs::symlink_metadata(&link) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&link).map_err(|error| CliInstallError::Io {
                    path: link.clone(),
                    message: error.to_string(),
                })?;
                if self.resolves_to_bundle(&link, &target) {
                    return self.report(&link, false);
                }
                return Err(CliInstallError::ForeignSymlink { path: link, target });
            }
            Ok(_) => return Err(CliInstallError::ForeignFile { path: link }),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CliInstallError::Io {
                    path: link,
                    message: error.to_string(),
                });
            }
        }

        std::os::unix::fs::symlink(&self.bundled_binary, &link).map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                CliInstallError::ForeignFile { path: link.clone() }
            } else {
                CliInstallError::Io {
                    path: link.clone(),
                    message: error.to_string(),
                }
            }
        })?;
        self.report(&link, true)
    }

    /// Removes the `agentuna` symlink when (and only when) it points at this app's bundled
    /// binary. Foreign files, foreign symlinks and missing links produce a warning instead.
    ///
    /// With `location = None`, the primary directory is inspected first, then the fallback.
    #[must_use]
    pub fn uninstall(&self, location: Option<CliInstallLocation>) -> CliUninstallReport {
        let candidates = match location {
            Some(location) => vec![self.directory(location)],
            None => vec![self.primary_dir.clone(), self.fallback_dir()],
        };
        for directory in candidates {
            let link = directory.join(CLI_NAME);
            let metadata = match std::fs::symlink_metadata(&link) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return CliUninstallReport {
                        link_path: display_path(&link),
                        removed: false,
                        warning: Some(error.to_string()),
                    };
                }
            };
            if !metadata.file_type().is_symlink() {
                return CliUninstallReport {
                    link_path: display_path(&link),
                    removed: false,
                    warning: Some(format!(
                        "{} is not a symlink; it was left untouched",
                        display_path(&link)
                    )),
                };
            }
            let target = match std::fs::read_link(&link) {
                Ok(target) => target,
                Err(error) => {
                    return CliUninstallReport {
                        link_path: display_path(&link),
                        removed: false,
                        warning: Some(error.to_string()),
                    };
                }
            };
            if !self.resolves_to_bundle(&link, &target) {
                return CliUninstallReport {
                    link_path: display_path(&link),
                    removed: false,
                    warning: Some(format!(
                        "{} points to {}; a foreign symlink is left alone",
                        display_path(&link),
                        display_path(&target)
                    )),
                };
            }
            return match std::fs::remove_file(&link) {
                Ok(()) => CliUninstallReport {
                    link_path: display_path(&link),
                    removed: true,
                    warning: None,
                },
                Err(error) => CliUninstallReport {
                    link_path: display_path(&link),
                    removed: false,
                    warning: Some(error.to_string()),
                },
            };
        }
        CliUninstallReport {
            link_path: display_path(&self.primary_dir.join(CLI_NAME)),
            removed: false,
            warning: Some("agentuna is not installed in either location".to_owned()),
        }
    }

    fn directory(&self, location: CliInstallLocation) -> PathBuf {
        match location {
            CliInstallLocation::Primary => self.primary_dir.clone(),
            CliInstallLocation::Fallback => self.fallback_dir(),
        }
    }

    fn fallback_dir(&self) -> PathBuf {
        self.home.join(".local").join("bin")
    }

    fn location(&self, directory: &Path) -> CliLocation {
        let link = directory.join(CLI_NAME);
        let on_path = self.is_on_path(directory);
        CliLocation {
            directory: display_path(directory),
            link_path: display_path(&link),
            writable: directory_writable(directory),
            on_path,
            path_export: if on_path {
                None
            } else {
                Some(self.export_line(directory))
            },
        }
    }

    fn is_on_path(&self, directory: &Path) -> bool {
        self.path_var.as_ref().is_some_and(|path| {
            std::env::split_paths(path).any(|entry| same_directory(&entry, directory))
        })
    }

    fn export_line(&self, directory: &Path) -> String {
        let shown = directory.strip_prefix(&self.home).ok().map_or_else(
            || display_path(directory),
            |rest| {
                if rest.as_os_str().is_empty() {
                    "$HOME".to_owned()
                } else {
                    format!("$HOME/{}", rest.display())
                }
            },
        );
        format!("export PATH=\"{shown}:$PATH\"")
    }

    fn installed_at(&self, location: &CliLocation) -> Option<CliInstallation> {
        let link = Path::new(&location.link_path);
        let metadata = std::fs::symlink_metadata(link).ok()?;
        if !metadata.file_type().is_symlink() {
            return None;
        }
        let target = std::fs::read_link(link).ok()?;
        if !self.resolves_to_bundle(link, &target) {
            return None;
        }
        Some(CliInstallation {
            link_path: location.link_path.clone(),
            target: display_path(&std::fs::canonicalize(link).unwrap_or(target)),
        })
    }

    fn resolves_to_bundle(&self, link: &Path, target: &Path) -> bool {
        let absolute_target = if target.is_absolute() {
            target.to_path_buf()
        } else {
            link.parent()
                .map_or_else(|| target.to_path_buf(), |parent| parent.join(target))
        };
        match (
            std::fs::canonicalize(&absolute_target),
            std::fs::canonicalize(&self.bundled_binary),
        ) {
            (Ok(link_target), Ok(bundled)) => link_target == bundled,
            _ => absolutize(absolute_target) == absolutize(self.bundled_binary.clone()),
        }
    }

    fn report(&self, link: &Path, created: bool) -> Result<CliInstallReport, CliInstallError> {
        let target = std::fs::canonicalize(link).map_err(|error| CliInstallError::Io {
            path: link.to_path_buf(),
            message: error.to_string(),
        })?;
        let reported_version = probe_version(link);
        let expected = format!("{} {}", CLI_NAME, self.version);
        let matches_app_version = reported_version
            .as_deref()
            .is_some_and(|reported| reported == expected || reported == self.version);
        Ok(CliInstallReport {
            link_path: display_path(link),
            target: display_path(&target),
            created,
            reported_version,
            matches_app_version,
        })
    }
}

fn probe_version(binary: &Path) -> Option<String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

fn directory_writable(directory: &Path) -> bool {
    let mut candidate = directory;
    loop {
        if candidate.is_dir() {
            return access(candidate, Access::WRITE_OK).is_ok();
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => return false,
        }
    }
}

fn same_directory(candidate: &Path, directory: &Path) -> bool {
    absolutize(candidate.to_path_buf()) == absolutize(directory.to_path_buf())
}

fn absolutize(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests;
