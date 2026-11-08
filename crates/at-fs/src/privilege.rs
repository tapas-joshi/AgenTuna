//! Global store availability and the one-shot elevated preparation of its directory.
//!
//! The Global scope is machine-wide (by default
//! `/Library/Application Support/AgenTuna/global`), so the resolved directory can exist outside
//! the user's write permission. This module answers three questions and nothing else:
//!
//! 1. [`GlobalStoreStatus::probe`] reports whether the resolved directory is writable, without
//!    creating or modifying anything.
//! 2. [`GlobalStore::write`] refuses a write with [`GlobalWriteError::ReadOnly`] before a temp
//!    file is staged, so a read-only store never leaves partial files behind.
//! 3. [`enable_global_writes`] performs exactly one privileged `mkdir`/`chown`/`chmod` through an
//!    injected [`Elevator`]. The production elevator asks macOS for administrator authorization
//!    with a visible prompt; dismissing or denying it changes nothing and is never retried. There
//!    is no silent `sudo`.
//!
//! Preparing the directory is the only elevated operation. Every subsequent file mutation goes
//! through [`crate::apply`], with its atomic replace, backup and temp-file cleanup guarantees.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use rustix::process::{getgid, getuid};
use serde::Serialize;

use crate::atomic::{WriteOutcome, WriteRequest, apply};
use crate::error::FsError;

/// Result of checking the resolved Global store directory.
///
/// The variants are the stable contract for the Overview card and for every Global write intent:
/// only [`Self::Available`] permits a write without elevation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GlobalStoreStatus {
    /// The directory exists and this process may create and replace files in it.
    Available,
    /// The directory exists but this process may not write to it.
    ReadOnly {
        /// The directory that was checked.
        path: PathBuf,
        /// Why writing is refused.
        reason: String,
    },
    /// The directory is absent, is not a directory, or is otherwise unusable.
    Unavailable {
        /// The directory that was checked.
        path: PathBuf,
        /// Why the directory cannot be used.
        reason: String,
    },
}

impl GlobalStoreStatus {
    /// Checks `path` without leaving anything behind: nothing is created, chmodded or modified
    /// on Unix, where `access(2)` answers the question directly. On Windows the probe creates and
    /// removes one uniquely named file, because there is no ACL query without unsafe calls and a
    /// probe that never writes cannot prove a write will succeed.
    ///
    /// An absent directory is [`Self::Unavailable`] (first-run state); an existing directory the
    /// current user cannot write to is [`Self::ReadOnly`]. Relative paths, `..` components,
    /// control characters and non-UTF-8 paths are [`Self::Unavailable`] with the reason, because
    /// the elevated preparation step could not pass them to the authorization prompt safely.
    #[must_use]
    pub fn probe(path: &Path) -> Self {
        if let Err(reason) = validate_store_path(path) {
            return Self::Unavailable {
                path: path.to_path_buf(),
                reason: reason.to_owned(),
            };
        }
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::Unavailable {
                    path: path.to_path_buf(),
                    reason: "directory does not exist".to_owned(),
                };
            }
            Err(error) => {
                return Self::Unavailable {
                    path: path.to_path_buf(),
                    reason: format!("could not inspect the directory: {error}"),
                };
            }
        };
        if !metadata.is_dir() {
            return Self::Unavailable {
                path: path.to_path_buf(),
                reason: "path is not a directory".to_owned(),
            };
        }
        // A store directory must be both writable and searchable for files to be created in it.
        match crate::probe::probe_directory_write(path) {
            Ok(()) => Self::Available,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Self::ReadOnly {
                path: path.to_path_buf(),
                reason: "directory is not writable by this user".to_owned(),
            },
            Err(error) => Self::ReadOnly {
                path: path.to_path_buf(),
                reason: format!("could not verify write access: {error}"),
            },
        }
    }

    /// Whether a write may proceed without elevation.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// Stable machine identifier used by logs, diagnostics and the UI.
    #[must_use]
    pub const fn state(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::ReadOnly { .. } => "read_only",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    /// Why the store cannot be written, or `None` when it is available.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::ReadOnly { reason, .. } | Self::Unavailable { reason, .. } => Some(reason),
        }
    }
}

impl fmt::Display for GlobalStoreStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available => f.write_str("available"),
            Self::ReadOnly { path, reason } => {
                write!(f, "read-only at {}: {reason}", path.display())
            }
            Self::Unavailable { path, reason } => {
                write!(f, "unavailable at {}: {reason}", path.display())
            }
        }
    }
}

/// Why a Global write did not happen. No temp file or backup is created for the first three.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GlobalWriteError {
    /// The store exists but is not writable; the caller may offer "Enable global writes".
    #[error("{}: the Global store is read-only: {reason}", path.display())]
    ReadOnly {
        /// The store directory.
        path: PathBuf,
        /// Why it is read-only.
        reason: String,
    },
    /// The store directory is absent or unusable; elevation can create it.
    #[error("{}: the Global store is unavailable: {reason}", path.display())]
    Unavailable {
        /// The store directory.
        path: PathBuf,
        /// Why it is unavailable.
        reason: String,
    },
    /// The request targeted a path outside the store the gate protects.
    #[error(
        "{}: outside the Global store root {}",
        path.display(),
        root.display()
    )]
    OutsideStore {
        /// The rejected request path.
        path: PathBuf,
        /// The store root the request had to be inside.
        root: PathBuf,
    },
    /// The guarded atomic write failed.
    #[error(transparent)]
    Write(#[from] FsError),
}

/// A Global store directory plus the read-only gate for every write inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalStore {
    root: PathBuf,
}

impl GlobalStore {
    /// Wraps an already-resolved store directory. Resolution is the caller's job
    /// (`at_store::AppPaths`); this type never reads the environment.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Current availability, re-probed from the filesystem on every call so an elevation or an
    /// external permission change is visible immediately.
    #[must_use]
    pub fn status(&self) -> GlobalStoreStatus {
        GlobalStoreStatus::probe(&self.root)
    }

    /// Returns [`GlobalWriteError::ReadOnly`] / [`GlobalWriteError::Unavailable`] when the store
    /// cannot be written, without touching the filesystem.
    ///
    /// # Errors
    /// [`GlobalWriteError::ReadOnly`] or [`GlobalWriteError::Unavailable`] with the reason.
    pub fn ensure_writable(&self) -> Result<(), GlobalWriteError> {
        match self.status() {
            GlobalStoreStatus::Available => Ok(()),
            GlobalStoreStatus::ReadOnly { path, reason } => {
                Err(GlobalWriteError::ReadOnly { path, reason })
            }
            GlobalStoreStatus::Unavailable { path, reason } => {
                Err(GlobalWriteError::Unavailable { path, reason })
            }
        }
    }

    /// Lexical containment check: `path` is the root or below it, component by component, with no
    /// `..` segment that could climb out of the store.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    }

    /// Status gate, then the normal guarded atomic write.
    ///
    /// The gate runs first: a read-only store returns [`GlobalWriteError::ReadOnly`] before
    /// [`apply`] creates a temp file, so nothing is staged and nothing can be left behind.
    ///
    /// # Errors
    /// [`GlobalWriteError::OutsideStore`] for a path outside [`Self::root`]; the read-only and
    /// unavailable variants; any [`FsError`] from [`apply`].
    pub fn write(&self, request: WriteRequest) -> Result<WriteOutcome, GlobalWriteError> {
        if !self.contains(&request.path) {
            return Err(GlobalWriteError::OutsideStore {
                path: request.path,
                root: self.root.clone(),
            });
        }
        self.ensure_writable()?;
        apply(&request).map_err(GlobalWriteError::from)
    }
}

/// Ownership applied to the Global store after elevation.
///
/// Numeric IDs are used so the elevation request is independent of the directory service and
/// stays valid for the account that is currently running AgenTuna.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreOwner {
    /// User ID that will own the store directory.
    pub uid: u32,
    /// Group ID that will own the store directory.
    pub gid: u32,
}

impl StoreOwner {
    /// An explicit owner, used by tests and by callers that already resolved the account.
    #[must_use]
    pub const fn new(uid: u32, gid: u32) -> Self {
        Self { uid, gid }
    }

    /// The real user and group running this process.
    ///
    /// Windows has no numeric owner ids. This returns zeros there and documents it, because the
    /// only consumer is an elevation request and [`platform_elevator`] refuses every request on a
    /// platform without a consent-based elevator, so the value is never applied to a directory.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(unix)]
        {
            Self {
                uid: getuid().as_raw(),
                gid: getgid().as_raw(),
            }
        }
        #[cfg(not(unix))]
        {
            Self { uid: 0, gid: 0 }
        }
    }
}

/// The single directory operation an [`Elevator`] may perform.
///
/// The Global store contains only AgenTuna-owned files, so preparing the directory is enough:
/// once it exists and is owned by the invoking user, ordinary guarded writes need no further
/// authorization. There is deliberately no way to express an arbitrary privileged copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDirectoryRequest {
    /// Absolute, validated store directory.
    pub path: PathBuf,
    /// Ownership to apply after creating or finding the directory.
    pub owner: StoreOwner,
}

impl StoreDirectoryRequest {
    /// Builds the one-shot preparation request for `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, owner: StoreOwner) -> Self {
        Self {
            path: path.into(),
            owner,
        }
    }
}

/// Why the privileged step did not complete.
///
/// Cancellation and denial are distinct so the UI can say "you cancelled" versus "authorization
/// was denied" without guessing from a message.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ElevationError {
    /// The user dismissed the operating-system authorization prompt.
    #[error("the authorization prompt was dismissed")]
    Cancelled,
    /// The authorization was rejected (wrong credentials or an explicit denial).
    #[error("authorization was denied")]
    NotAuthorized,
    /// The privileged command could not run or failed for another reason.
    #[error("elevation failed: {message}")]
    Failed {
        /// Redacted, length-bounded explanation.
        message: String,
    },
    /// No consent-based elevation mechanism exists on this platform.
    #[error("elevated writes are not supported on this platform")]
    Unsupported,
}

/// Performs one privileged operation after explicit operating-system authorization.
///
/// Implementations must not retry, must not run a silent `sudo`, and must report cancellation and
/// denial as distinct errors. Tests inject a recording fake; production uses
/// [`platform_elevator`].
pub trait Elevator: Send + Sync {
    /// Creates (or finds) the store directory and makes it owned by the requesting user.
    ///
    /// # Errors
    /// [`ElevationError`] when the prompt is dismissed, denied, unsupported or the privileged
    /// command fails.
    fn elevate(&self, request: &StoreDirectoryRequest) -> Result<(), ElevationError>;
}

/// What [`enable_global_writes`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnableOutcome {
    /// The store was already writable; no prompt was shown and nothing changed.
    AlreadyWritable,
    /// The store directory was created and is now writable.
    Prepared,
}

/// Why enabling Global writes did not produce a writable store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnableError {
    /// The resolved path is not a safe target for a privileged operation.
    #[error("{}: {reason}", path.display())]
    InvalidPath {
        /// The rejected path.
        path: PathBuf,
        /// Why it was rejected.
        reason: String,
    },
    /// The user dismissed the authorization prompt. Nothing was written.
    #[error("enabling Global writes was cancelled")]
    Cancelled {
        /// Status after the cancellation; the store must be exactly as it was.
        status: GlobalStoreStatus,
    },
    /// The authorization was denied. Nothing was written.
    #[error("enabling Global writes was not authorized")]
    NotAuthorized {
        /// Status after the denial; the store must be exactly as it was.
        status: GlobalStoreStatus,
    },
    /// The privileged command failed.
    #[error("could not enable Global writes: {message}")]
    Failed {
        /// Redacted, length-bounded explanation.
        message: String,
    },
    /// This platform has no consent-based elevation mechanism.
    #[error("elevated writes are not supported on this platform")]
    Unsupported,
    /// The elevator reported success but the directory is still not writable.
    #[error("Global store is still not writable after elevation: {status}")]
    NotEffective {
        /// The status observed after the privileged step.
        status: GlobalStoreStatus,
    },
}

/// Prepares the Global store with exactly one privileged operation.
///
/// * An already writable directory is left untouched and no authorization runner is invoked.
/// * Otherwise the injected [`Elevator`] is called once. Its prompt is the user's confirmation;
///   there is no separate approval flag and no retry loop.
/// * Cancellation and denial re-probe the store and report its unchanged status. Nothing is
///   created, so there is no partial file or directory to clean up.
/// * Success is verified by re-probing; a "successful" elevation that did not make the directory
///   writable is [`EnableError::NotEffective`], never a silent write attempt.
///
/// # Errors
/// [`EnableError::InvalidPath`] for relative paths, `..` components, control characters or
/// non-UTF-8 paths; [`EnableError::Cancelled`], [`EnableError::NotAuthorized`],
/// [`EnableError::Unsupported`], [`EnableError::Failed`] from the elevator;
/// [`EnableError::NotEffective`] when the post-check fails.
pub fn enable_global_writes(
    path: &Path,
    owner: StoreOwner,
    elevator: &dyn Elevator,
) -> Result<EnableOutcome, EnableError> {
    validate_store_path(path).map_err(|reason| EnableError::InvalidPath {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    })?;
    if GlobalStoreStatus::probe(path).is_available() {
        return Ok(EnableOutcome::AlreadyWritable);
    }
    let request = StoreDirectoryRequest::new(path, owner);
    match elevator.elevate(&request) {
        Ok(()) => {}
        Err(ElevationError::Cancelled) => {
            return Err(EnableError::Cancelled {
                status: GlobalStoreStatus::probe(path),
            });
        }
        Err(ElevationError::NotAuthorized) => {
            return Err(EnableError::NotAuthorized {
                status: GlobalStoreStatus::probe(path),
            });
        }
        Err(ElevationError::Failed { message }) => return Err(EnableError::Failed { message }),
        Err(ElevationError::Unsupported) => return Err(EnableError::Unsupported),
    }
    let status = GlobalStoreStatus::probe(path);
    if status.is_available() {
        Ok(EnableOutcome::Prepared)
    } else {
        Err(EnableError::NotEffective { status })
    }
}

/// The program and argument vector handed to the operating system.
///
/// Kept separate from spawning so tests can assert that the store path travels as its own
/// argument and is never interpolated into the AppleScript source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsascriptInvocation {
    /// Absolute path of the program.
    pub executable: PathBuf,
    /// Argument vector, one element per argument.
    pub arguments: Vec<OsString>,
}

/// Path of the macOS script runner used for the authorization prompt.
pub const OSASCRIPT: &str = "/usr/bin/osascript";

/// AppleScript run once, as a constant, with the store path supplied through `argv`.
///
/// `quoted form of` makes each path a single shell word, so the script never pastes untrusted text
/// into shell syntax. `with administrator privileges` is the single system authorization dialog.
const ELEVATION_SCRIPT: &str = "\
on run argv
    set targetPath to item 1 of argv
    set ownerSpec to item 2 of argv
    do shell script \"/bin/mkdir -p \" & quoted form of targetPath & \" && /usr/sbin/chown \" & ownerSpec & \" \" & quoted form of targetPath & \" && /bin/chmod 755 \" & quoted form of targetPath with administrator privileges
end run";

/// Builds the exact `osascript` invocation for one store-directory preparation.
///
/// The path and the numeric owner are separate `argv` items after `--`; only the fixed script
/// constant is passed to `-e`.
#[must_use]
pub fn osascript_invocation(request: &StoreDirectoryRequest) -> OsascriptInvocation {
    OsascriptInvocation {
        executable: PathBuf::from(OSASCRIPT),
        arguments: vec![
            OsString::from("-e"),
            OsString::from(ELEVATION_SCRIPT),
            OsString::from("--"),
            request.path.as_os_str().to_os_string(),
            OsString::from(format!("{}:{}", request.owner.uid, request.owner.gid)),
        ],
    }
}

/// The macOS elevator: one `osascript` run, one administrator prompt.
///
/// This type is never constructed by tests or by the write path; only [`platform_elevator`] and
/// the explicit "Enable global writes" action create it.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OsascriptElevator;

#[cfg(target_os = "macos")]
impl OsascriptElevator {
    /// Creates the production elevator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
impl Elevator for OsascriptElevator {
    fn elevate(&self, request: &StoreDirectoryRequest) -> Result<(), ElevationError> {
        // Defence in depth: the public trait may be called without going through
        // `enable_global_writes`, and a privileged command must never run for an unsafe path.
        if let Err(reason) = validate_store_path(&request.path) {
            return Err(ElevationError::Failed {
                message: format!("refusing to elevate an unsafe path: {reason}"),
            });
        }
        let invocation = osascript_invocation(request);
        let output = std::process::Command::new(&invocation.executable)
            .args(&invocation.arguments)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| ElevationError::Failed {
                message: format!("could not start the authorization prompt: {error}"),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(classify_osascript_failure(&String::from_utf8_lossy(
            &output.stderr,
        )))
    }
}

/// Stand-in for platforms without a consent-based elevation mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnsupportedElevator;

impl Elevator for UnsupportedElevator {
    fn elevate(&self, _request: &StoreDirectoryRequest) -> Result<(), ElevationError> {
        Err(ElevationError::Unsupported)
    }
}

/// The production elevator for this platform.
#[cfg(target_os = "macos")]
#[must_use]
pub fn platform_elevator() -> Arc<dyn Elevator> {
    Arc::new(OsascriptElevator::new())
}

/// The production elevator for this platform.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn platform_elevator() -> Arc<dyn Elevator> {
    Arc::new(UnsupportedElevator)
}

/// Maps `osascript` stderr to a cancellation, a denial or a generic failure.
#[cfg(any(test, target_os = "macos"))]
fn classify_osascript_failure(stderr: &str) -> ElevationError {
    let lower = stderr.to_lowercase();
    if lower.contains("(-128)")
        || lower.contains("(-60006)")
        || lower.contains("user canceled")
        || lower.contains("user cancelled")
    {
        ElevationError::Cancelled
    } else if lower.contains("(-60005)")
        || lower.contains("authorization was denied")
        || lower.contains("not authorized")
    {
        ElevationError::NotAuthorized
    } else {
        ElevationError::Failed {
            message: first_line(stderr),
        }
    }
}

/// First non-empty line, control characters removed and bounded to 512 characters.
#[cfg(any(test, target_os = "macos"))]
fn first_line(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("the authorization command failed");
    line.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(512)
        .collect()
}

/// Structural checks shared by probing and elevation.
///
/// The path must be absolute, must not contain `..`, must not be the filesystem root, must be
/// valid UTF-8 (AppleScript receives it as a string) and must not contain control characters.
fn validate_store_path(path: &Path) -> Result<(), &'static str> {
    if path.as_os_str().is_empty() {
        return Err("path is empty");
    }
    if !path.is_absolute() {
        return Err("path is not absolute");
    }
    if path.to_str().is_none() {
        return Err("path is not valid UTF-8");
    }
    let mut named_components = 0_usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                if name
                    .to_str()
                    .is_none_or(|name| name.chars().any(char::is_control))
                {
                    return Err("path contains a control character");
                }
                named_components += 1;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err("path contains a parent or prefix component");
            }
        }
    }
    if named_components == 0 {
        return Err("path resolves to the filesystem root");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osascript_failure_classification_separates_cancel_deny_and_failure() {
        assert_eq!(
            classify_osascript_failure("execution error: User canceled. (-128)"),
            ElevationError::Cancelled
        );
        assert_eq!(
            classify_osascript_failure("execution error: Authorization was denied. (-60005)"),
            ElevationError::NotAuthorized
        );
        assert!(matches!(
            classify_osascript_failure("execution error: something else (1)"),
            ElevationError::Failed { .. }
        ));
        assert_eq!(
            classify_osascript_failure("error (-60006)"),
            ElevationError::Cancelled
        );
    }

    #[test]
    fn first_line_bounds_and_strips_control_characters() {
        let input = format!("\n\nhello\u{7}world\n{}", "x".repeat(700));
        assert_eq!(first_line(&input), "hello world");
        assert_eq!(first_line(&"y".repeat(700)).chars().count(), 512);
        assert_eq!(first_line("   \n  "), "the authorization command failed");
    }
}
