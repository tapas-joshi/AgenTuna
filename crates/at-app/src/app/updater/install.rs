//! Installing a staged update: Team ID check, bundle swap, relaunch and the deferred queue.
//!
//! The install path is deliberately split from [`super::check_and_stage`]: that function only
//! authenticates and stages an archive, while this module owns everything that touches the
//! running application bundle. Both external effects — reading the code signature and launching
//! the new bundle — sit behind traits so the whole flow runs offline in temporary directories.
//!
//! The policy is the story's: the staged bundle must be signed by the **same** Team ID as the
//! running app. An ad-hoc or unsigned running app has no Team ID, so no staged bundle can be
//! proven to come from the same identity; an automatic install is refused and the user installs
//! manually. Nothing is ever installed while an operation, job, modal or dirty editor is active:
//! the request is queued and persisted until the app is idle again.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use at_fs::bundle::{
    BundleSwap, BundleSwapError, BundleSwapJournal, BundleSwapPhase, BundleSwapRequest,
    commit_bundle, read_bundle_swap_journal, rollback_bundle, swap_bundle,
};
use at_fs::concurrency::ExpectedState;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

/// Upper bound on the files one update archive may contain.
pub const MAX_ARCHIVE_ENTRIES: usize = 100_000;
/// Upper bound on the bytes one update archive may expand to (4 GiB).
pub const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Default bound for one helper process (codesign, open).
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// Clock used for queue timestamps, injected so tests are deterministic.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_unix_ms(&self) -> i64;
}

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        let now = time::OffsetDateTime::now_utc();
        i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
    }
}

/// Reads the code-signing Team ID of a bundle.
pub trait SignatureVerifier: Send + Sync {
    /// The Team ID, or `None` for an ad-hoc or unsigned bundle.
    ///
    /// # Errors
    ///
    /// [`SignatureError`] when the bundle is unsigned or the tool cannot run.
    fn team_id(&self, bundle: &Path) -> Result<Option<String>, SignatureError>;
}

/// Failure of the external code-signing check.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SignatureError {
    /// The bundle is not signed at all.
    #[error("{} is not code-signed", .path.display())]
    NotSigned {
        /// Bundle that is unsigned.
        path: PathBuf,
    },
    /// `codesign` ran but reported an invalid signature.
    #[error("codesign rejected {}: {status}", .path.display())]
    CodesignFailed {
        /// Bundle whose signature is invalid.
        path: PathBuf,
        /// Exit status of the tool.
        status: String,
    },
    /// The helper could not run or finish in time.
    #[error(transparent)]
    Tool(#[from] ProcessError),
}

/// Failure to run a helper process.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProcessError {
    /// The executable could not be started.
    #[error("could not start {}: {source}", .program.display())]
    Spawn {
        /// Absolute program path.
        program: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The process did not finish inside the bound and was killed.
    #[error("{} did not finish within {seconds} s", .program.display())]
    Timeout {
        /// Absolute program path.
        program: PathBuf,
        /// Bound in whole seconds.
        seconds: u64,
    },
    /// Reading the captured output failed.
    #[error("could not read the output of {}: {source}", .program.display())]
    Output {
        /// Absolute program path.
        program: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

/// Production signature verifier: `/usr/bin/codesign -dv --verbose=4`.
#[derive(Clone, Debug)]
pub struct CodesignVerifier {
    program: PathBuf,
    timeout: Duration,
}

impl Default for CodesignVerifier {
    fn default() -> Self {
        Self::system()
    }
}

impl CodesignVerifier {
    /// The system tool with a bounded wait.
    #[must_use]
    pub fn system() -> Self {
        Self {
            program: PathBuf::from("/usr/bin/codesign"),
            timeout: HELPER_TIMEOUT,
        }
    }

    /// A verifier that runs `program` with `timeout`, for tests and non-standard tool locations.
    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
        }
    }
}

impl SignatureVerifier for CodesignVerifier {
    fn team_id(&self, bundle: &Path) -> Result<Option<String>, SignatureError> {
        let output = run_bounded(
            &self.program,
            &[
                OsStr::new("-dv"),
                OsStr::new("--verbose=4"),
                bundle.as_os_str(),
            ],
            self.timeout,
        )?;
        if !output.status.success() {
            if output.text.contains("not signed at all") {
                return Err(SignatureError::NotSigned {
                    path: bundle.to_path_buf(),
                });
            }
            return Err(SignatureError::CodesignFailed {
                path: bundle.to_path_buf(),
                status: output.status.to_string(),
            });
        }
        Ok(parse_team_identifier(&output.text))
    }
}

/// Extracts the Team ID from `codesign -dv --verbose=4` output.
#[must_use]
pub fn parse_team_identifier(output: &str) -> Option<String> {
    if output.contains("Signature=adhoc") {
        return None;
    }
    output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("TeamIdentifier=")?.trim();
        (!value.is_empty() && value != "not set").then(|| value.to_owned())
    })
}

/// Launches the newly installed bundle.
pub trait Relauncher: Send + Sync {
    /// Starts the application at `bundle`.
    ///
    /// # Errors
    ///
    /// [`RelaunchError`] when the launch cannot be requested.
    fn relaunch(&self, bundle: &Path) -> Result<(), RelaunchError>;
}

/// Failure to request a relaunch.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RelaunchError {
    /// The helper could not run or finish in time.
    #[error(transparent)]
    Tool(#[from] ProcessError),
    /// `open` exited with a failure status.
    #[error("open exited with {status}")]
    Failed {
        /// Exit status of the tool.
        status: String,
    },
}

/// Production relauncher: `/usr/bin/open -n <bundle>`.
#[derive(Clone, Debug)]
pub struct OpenRelauncher {
    program: PathBuf,
    timeout: Duration,
}

impl Default for OpenRelauncher {
    fn default() -> Self {
        Self::system()
    }
}

impl OpenRelauncher {
    /// The system launcher with a bounded wait.
    #[must_use]
    pub fn system() -> Self {
        Self {
            program: PathBuf::from("/usr/bin/open"),
            timeout: HELPER_TIMEOUT,
        }
    }

    /// A relauncher that runs `program` with `timeout`, for tests and non-standard tool locations.
    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
        }
    }
}

impl Relauncher for OpenRelauncher {
    fn relaunch(&self, bundle: &Path) -> Result<(), RelaunchError> {
        let output = run_bounded(
            &self.program,
            &[OsStr::new("-n"), bundle.as_os_str()],
            self.timeout,
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(RelaunchError::Failed {
                status: output.status.to_string(),
            })
        }
    }
}

/// Facts about the running app that the installer checks against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningApp {
    /// Version of the running binary.
    pub version: String,
    /// The `.app` directory that contains the running executable.
    pub bundle: PathBuf,
    /// Team ID of the running bundle, `None` for an ad-hoc or unsigned build.
    pub team_id: Option<String>,
}

impl RunningApp {
    /// Walks up from `executable` to the enclosing `.app` directory.
    #[must_use]
    pub fn bundle_for_executable(executable: &Path) -> Option<PathBuf> {
        executable
            .ancestors()
            .find(|candidate| {
                candidate
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
            })
            .map(Path::to_path_buf)
    }
}

/// One build's facts as the installer needs them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRequest {
    /// Version of the staged bundle.
    pub version: String,
    /// Verified update archive (the `.zip` [`super::check_and_stage`] produced).
    pub archive: PathBuf,
    /// Directory for the extracted bundle; must sit on the same filesystem as the installed app.
    pub staging_root: PathBuf,
    /// Directory that keeps the previous bundle until the new one reports healthy.
    pub rollback_root: PathBuf,
    /// Journal file for the swap.
    pub journal: PathBuf,
}

/// What a successful install did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    /// Installed version.
    pub version: String,
    /// Installed bundle path.
    pub bundle: PathBuf,
    /// Retained copy of the previous bundle.
    pub rollback: PathBuf,
    /// Journal describing the retained rollback copy.
    pub journal: PathBuf,
}

/// Verifies, swaps and relaunches an update.
pub struct BundleInstaller {
    verifier: Arc<dyn SignatureVerifier>,
    relauncher: Arc<dyn Relauncher>,
    clock: Arc<dyn Clock>,
}

impl BundleInstaller {
    /// Builds an installer from its two external seams.
    #[must_use]
    pub fn new(
        verifier: Arc<dyn SignatureVerifier>,
        relauncher: Arc<dyn Relauncher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            verifier,
            relauncher,
            clock,
        }
    }

    /// Extracts, verifies and swaps the staged update, then relaunches it.
    ///
    /// The previous bundle stays at the returned rollback path until
    /// [`reconcile_bundle_swap`] sees the new version running. A failed relaunch restores the
    /// previous bundle immediately.
    ///
    /// # Errors
    ///
    /// [`InstallError`] for a missing or unsafe archive, a Team ID mismatch, a failed swap and a
    /// failed relaunch; the previous bundle is restored wherever that is possible.
    pub fn install(
        &self,
        request: &InstallRequest,
        running: &RunningApp,
    ) -> Result<InstallOutcome, InstallError> {
        let outcome = self.prepare_and_swap(request, running)?;
        self.relaunch(&outcome)?;
        Ok(outcome)
    }

    /// Extracts, verifies and swaps the staged update **without** relaunching.
    ///
    /// Split from [`Self::relaunch`] so the shell can release its single-instance record between
    /// the two steps: the relaunched copy must become the primary instance, not focus the process
    /// that is about to exit.
    ///
    /// # Errors
    ///
    /// [`InstallError`] for a missing or unsafe archive, a Team ID mismatch or a failed swap; the
    /// previous bundle stays installed when the swap fails.
    pub fn prepare_and_swap(
        &self,
        request: &InstallRequest,
        running: &RunningApp,
    ) -> Result<InstallOutcome, InstallError> {
        validate_version(&request.version)?;
        if !request.archive.is_file() {
            return Err(InstallError::ArchiveMissing(request.archive.clone()));
        }
        let work = request.staging_root.join(format!(
            "extract-{}-{}",
            request.version,
            std::process::id()
        ));
        if work.exists() {
            discard_staging(&work);
        }
        at_fs::ensure_dir(&work)?;
        let result = self.install_staged(request, running, &work);
        if result.is_err() {
            discard_staging(&work);
        }
        result
    }

    /// Relaunches the installed bundle, restoring the previous bundle if that fails.
    ///
    /// # Errors
    ///
    /// [`InstallError::Relaunch`] with `rolled_back` telling whether the previous bundle was
    /// restored.
    pub fn relaunch(&self, outcome: &InstallOutcome) -> Result<(), InstallError> {
        if let Err(source) = self.relauncher.relaunch(&outcome.bundle) {
            let rolled_back = read_bundle_swap_journal(&outcome.journal)
                .ok()
                .flatten()
                .is_some_and(|journal| {
                    rollback_bundle(&BundleSwap::from_journal(outcome.journal.clone(), journal))
                        .is_ok()
                });
            return Err(InstallError::Relaunch {
                source,
                rolled_back,
            });
        }
        Ok(())
    }

    fn install_staged(
        &self,
        request: &InstallRequest,
        running: &RunningApp,
        work: &Path,
    ) -> Result<InstallOutcome, InstallError> {
        let staged_bundle = extract_archive(&request.archive, work)?;
        let staged_team = self.verifier.team_id(&staged_bundle)?;
        check_team_id(running.team_id.as_deref(), staged_team.as_deref())?;
        at_fs::ensure_dir(&request.rollback_root)?;
        let rollback =
            request
                .rollback_root
                .join(format!("{}-{}", request.version, self.clock.now_unix_ms()));
        let swap = swap_bundle(&BundleSwapRequest {
            staged: staged_bundle,
            target: running.bundle.clone(),
            rollback,
            journal: request.journal.clone(),
            installed_version: Some(request.version.clone()),
            previous_version: Some(running.version.clone()),
        })?;
        discard_staging(work);
        Ok(InstallOutcome {
            version: request.version.clone(),
            bundle: running.bundle.clone(),
            rollback: swap.rollback().to_path_buf(),
            journal: request.journal.clone(),
        })
    }
}

/// Checks that the staged bundle is signed by the same Team ID as the running app.
///
/// An ad-hoc or unsigned running app has no Team ID and therefore cannot vouch for any staged
/// bundle: the profile refuses with [`InstallError::RunningUnverified`] so an unsigned copy can
/// never replace a build whose origin is unknown. The user can still install manually.
///
/// # Errors
///
/// [`InstallError::TeamIdMismatch`], [`InstallError::StagedUnsigned`] or
/// [`InstallError::RunningUnverified`].
pub fn check_team_id(running: Option<&str>, staged: Option<&str>) -> Result<(), InstallError> {
    match (running, staged) {
        (Some(running), Some(staged)) if running == staged => Ok(()),
        (Some(running), Some(staged)) => Err(InstallError::TeamIdMismatch {
            running: running.to_owned(),
            staged: staged.to_owned(),
        }),
        (Some(_), None) => Err(InstallError::StagedUnsigned),
        (None, _) => Err(InstallError::RunningUnverified),
    }
}

/// Failures while verifying or installing a staged update.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InstallError {
    /// The staged bundle failed the signature check.
    #[error(transparent)]
    Signature(#[from] SignatureError),
    /// The staged bundle is signed by a different Team ID.
    #[error("the staged bundle is signed by Team ID {staged}, but the running app by {running}")]
    TeamIdMismatch {
        /// Team ID of the running app.
        running: String,
        /// Team ID of the staged bundle.
        staged: String,
    },
    /// The staged bundle has no Team ID.
    #[error("the staged bundle has no Developer ID Team ID")]
    StagedUnsigned,
    /// The running app has no Team ID (ad-hoc or unsigned build).
    #[error("the running app has no Developer ID Team ID, so no update can be verified against it")]
    RunningUnverified,
    /// The bundle swap failed.
    #[error(transparent)]
    Swap(#[from] BundleSwapError),
    /// The archive is missing.
    #[error("the staged update archive is missing: {}", .0.display())]
    ArchiveMissing(PathBuf),
    /// The archive is not a readable zip file.
    #[error("the update archive could not be read")]
    Archive(#[from] zip::result::ZipError),
    /// The archive contains no application bundle, or more than one.
    #[error("the update archive does not contain exactly one application bundle")]
    NoBundle,
    /// The archive contains an entry that would escape the extraction directory.
    #[error("the update archive contains an unsafe path: {path}")]
    UnsafeEntry {
        /// Rejected entry name.
        path: String,
    },
    /// The archive expands to more files or bytes than the installer accepts.
    #[error("the update archive expands past the installer limit")]
    ArchiveTooLarge,
    /// Reading or writing the extracted bundle failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The at-fs pipeline refused or failed a staging write.
    #[error(transparent)]
    Fs(#[from] at_fs::FsError),
    /// The archive name or a path component is not a safe version string.
    #[error("invalid update version: {reason}")]
    InvalidVersion {
        /// What is wrong.
        reason: String,
    },
    /// The running app is not inside an application bundle.
    #[error("the running app is not inside an application bundle")]
    NotBundled,
    /// The relaunch was refused after the swap.
    #[error("the new version is installed but could not be relaunched: {source}")]
    Relaunch {
        /// Failure of the launch.
        #[source]
        source: RelaunchError,
        /// Whether the previous bundle was restored.
        rolled_back: bool,
    },
    /// The queued request could not be persisted or cleared.
    #[error("the queued install at {}: {source}", .path.display())]
    Queue {
        /// Queue record path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

/// Rejects a version string that could alter a path or a queue record.
pub(crate) fn validate_version(version: &str) -> Result<(), InstallError> {
    if version.is_empty() {
        return Err(InstallError::InvalidVersion {
            reason: "version is empty".to_owned(),
        });
    }
    if version.len() > 64 {
        return Err(InstallError::InvalidVersion {
            reason: "version is longer than 64 bytes".to_owned(),
        });
    }
    if !version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
    {
        return Err(InstallError::InvalidVersion {
            reason: "version may only contain ASCII letters, digits, `.`, `-`, `_` and `+`"
                .to_owned(),
        });
    }
    Ok(())
}

fn extract_archive(archive: &Path, destination: &Path) -> Result<PathBuf, InstallError> {
    let file =
        fs::File::open(archive).map_err(|_| InstallError::ArchiveMissing(archive.to_path_buf()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(InstallError::ArchiveTooLarge);
    }
    at_fs::ensure_dir(destination)?;
    let mut total = 0_u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| InstallError::UnsafeEntry {
                path: entry
                    .name()
                    .map_or_else(|_| "<unreadable>".to_owned(), |name| name.into_owned()),
            })?;
        total = total.saturating_add(entry.size());
        if total > MAX_EXTRACTED_BYTES || entry.size() > at_fs::MAX_FILE_BYTES {
            return Err(InstallError::ArchiveTooLarge);
        }
        let target = destination.join(&enclosed);
        let mode = entry.unix_mode();
        if entry.is_dir() {
            at_fs::ensure_dir(&target)?;
            apply_archive_mode(&target, mode)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            at_fs::ensure_dir(parent)?;
        }
        if entry.is_symlink() {
            let mut link_target = String::new();
            entry.read_to_string(&mut link_target)?;
            if link_target.starts_with('/')
                || Path::new(&link_target)
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(InstallError::UnsafeEntry {
                    path: enclosed.display().to_string(),
                });
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &target)?;
            #[cfg(not(unix))]
            return Err(InstallError::UnsafeEntry {
                path: enclosed.display().to_string(),
            });
        } else {
            // Bound the read as well: a zip header may understate the decompressed size.
            let mut bytes = Vec::new();
            entry
                .take(at_fs::MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > at_fs::MAX_FILE_BYTES {
                return Err(InstallError::ArchiveTooLarge);
            }
            at_fs::apply(&at_fs::WriteRequest::create(&target, bytes))?;
            apply_archive_mode(&target, mode)?;
        }
    }
    find_bundle(destination)
}

/// Removes the private extraction tree under the OS temp directory.
///
/// Deliberately direct `std::fs`: this tree is outside every store and native-agent root and is
/// never a reviewed user file. The safety-audit allowlist names this call and its reason.
fn discard_staging(path: &Path) {
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
}

/// Restores the executable and directory bits the archive records.
///
/// An app bundle whose main executable lost its execute bit cannot be launched, so the archive's
/// Unix mode is applied instead of the `File::create` default.
fn apply_archive_mode(target: &Path, mode: Option<u32>) -> Result<(), InstallError> {
    if let Some(mode) = mode {
        at_fs::set_permissions(target, mode & 0o777)?;
    }
    Ok(())
}

fn find_bundle(directory: &Path) -> Result<PathBuf, InstallError> {
    let mut bundles = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
            && path.is_dir()
        {
            bundles.push(path);
        }
    }
    match bundles.len() {
        1 => bundles.pop().ok_or(InstallError::NoBundle),
        _ => Err(InstallError::NoBundle),
    }
}

/// Why an install request must wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum InstallDeferReason {
    /// A deploy, migration or other tracked operation is running.
    OperationRunning,
    /// A background job is running.
    JobRunning,
    /// An editor has unsaved changes.
    EditorDirty,
    /// A modal dialog is open.
    ModalOpen,
}

/// What the UI shows for an update install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum UpdateInstallStatus {
    /// This platform does not support in-place updates.
    Unsupported {
        /// Why in-place updates are unavailable and where users can get a release.
        reason: String,
    },
    /// Nothing is staged or requested.
    Idle,
    /// A verified archive is staged; installing is a single explicit action.
    Ready {
        /// Version that will be installed.
        version: String,
    },
    /// The request is waiting for the app to become idle.
    Queued {
        /// Version that will be installed.
        version: String,
        /// Why the install is waiting.
        reason: InstallDeferReason,
        /// RFC 3339 time of the request.
        requested_at: String,
    },
    /// The bundle swap is running.
    Installing {
        /// Version being installed.
        version: String,
    },
    /// The new bundle is installed and the relaunch was requested.
    Installed {
        /// Version now on disk.
        version: String,
    },
    /// The install failed before the bundle was replaced.
    Failed {
        /// Version that was being installed, when known.
        version: Option<String>,
        /// Secret-free failure description.
        message: String,
        /// What the user can do next.
        remedy: String,
        /// Whether the previous bundle was restored.
        rolled_back: bool,
    },
    /// The new version did not start; the previous bundle was put back.
    RolledBack {
        /// Version that did not start.
        version: String,
        /// Secret-free failure description.
        reason: String,
        /// What the user can do next.
        remedy: String,
    },
}

/// What the UI may do with an update install right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct UpdateInstallGate {
    /// Current install status.
    pub status: UpdateInstallStatus,
    /// Whether an install can be requested from this status.
    pub can_install: bool,
    /// Why an install must wait, when it must.
    pub defer_reason: Option<InstallDeferReason>,
    /// Whether a queued request can be cancelled.
    pub can_cancel: bool,
}

/// Builds the UI gate from a status and the current activity.
///
/// `can_install` is true only for a staged update: a failed or rolled-back install needs a new
/// check rather than a blind retry, and an install in flight must not be requested twice.
#[must_use]
pub fn install_gate(
    status: UpdateInstallStatus,
    defer_reason: Option<InstallDeferReason>,
) -> UpdateInstallGate {
    let can_install = matches!(status, UpdateInstallStatus::Ready { .. });
    let can_cancel = matches!(status, UpdateInstallStatus::Queued { .. });
    UpdateInstallGate {
        status,
        can_install,
        defer_reason,
        can_cancel,
    }
}

/// One persisted install request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInstall {
    /// Version to install.
    pub version: String,
    /// Why the install was deferred at request time.
    pub reason: InstallDeferReason,
    /// Epoch milliseconds of the request.
    pub requested_at_unix_ms: i64,
}

/// What the installer checks before touching the bundle.
pub trait IdleSignal: Send + Sync {
    /// `None` when nothing defers the install; otherwise the first blocking activity.
    fn defer_reason(&self) -> Option<InstallDeferReason>;
}

/// A fixed answer from the caller that already knows the current activity.
#[derive(Clone, Copy, Debug)]
pub struct StaticIdle {
    reason: Option<InstallDeferReason>,
}

impl StaticIdle {
    /// Wraps a known deferral.
    #[must_use]
    pub fn new(reason: Option<InstallDeferReason>) -> Self {
        Self { reason }
    }
}

impl IdleSignal for StaticIdle {
    fn defer_reason(&self) -> Option<InstallDeferReason> {
        self.reason
    }
}

/// A restart-surviving queue of install requests.
pub struct DeferredInstall {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    record: Mutex<Option<QueuedInstall>>,
}

impl DeferredInstall {
    /// Loads the queue record at `path`, ignoring and warning about an unreadable one.
    ///
    /// A corrupt record must not block the updater: the user can request the install again.
    #[must_use]
    pub fn load(path: PathBuf, clock: Arc<dyn Clock>) -> Self {
        let record = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<QueuedInstall>(&bytes) {
                Ok(record) => Some(record),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignoring an unreadable update install queue");
                    None
                }
            },
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                tracing::warn!(path = %path.display(), %error, "update install queue unreadable");
                None
            }
            Err(_) => None,
        };
        Self {
            path,
            clock,
            record: Mutex::new(record),
        }
    }

    /// The request waiting for quiescence, if any.
    #[must_use]
    pub fn pending(&self) -> Option<QueuedInstall> {
        lock(&self.record).clone()
    }

    /// Replaces the queued request with `version` and persists it.
    ///
    /// # Errors
    ///
    /// [`InstallError::InvalidVersion`] for an unsafe version and [`InstallError::Queue`] when the
    /// record cannot be written.
    pub fn enqueue(
        &self,
        version: &str,
        reason: InstallDeferReason,
    ) -> Result<QueuedInstall, InstallError> {
        validate_version(version)?;
        let record = QueuedInstall {
            version: version.to_owned(),
            reason,
            requested_at_unix_ms: self.clock.now_unix_ms(),
        };
        persist_queue(&self.path, Some(&record))?;
        *lock(&self.record) = Some(record.clone());
        Ok(record)
    }

    /// Removes any queued request.
    ///
    /// # Errors
    ///
    /// [`InstallError::Queue`] when the record cannot be removed.
    pub fn cancel(&self) -> Result<bool, InstallError> {
        let mut guard = lock(&self.record);
        let Some(queued) = guard.take() else {
            return Ok(false);
        };
        if let Err(error) = persist_queue(&self.path, None) {
            *guard = Some(queued);
            return Err(error);
        }
        Ok(true)
    }

    /// Takes the queued request when `idle` reports no deferring activity.
    ///
    /// # Errors
    ///
    /// [`InstallError::Queue`] when the cleared record cannot be persisted; the request stays
    /// queued in memory so a retry can still run it.
    pub fn take_if_idle(
        &self,
        idle: &dyn IdleSignal,
    ) -> Result<Option<QueuedInstall>, InstallError> {
        if idle.defer_reason().is_some() {
            return Ok(None);
        }
        let mut guard = lock(&self.record);
        let Some(queued) = guard.take() else {
            return Ok(None);
        };
        if let Err(error) = persist_queue(&self.path, None) {
            *guard = Some(queued);
            return Err(error);
        }
        Ok(Some(queued))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn persist_queue(path: &Path, record: Option<&QueuedInstall>) -> Result<(), InstallError> {
    match record {
        Some(record) => {
            let bytes = serde_json::to_vec(record).map_err(|error| InstallError::Queue {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidData, error),
            })?;
            if let Some(parent) = path.parent() {
                at_fs::ensure_dir(parent).map_err(|error| InstallError::Queue {
                    path: path.to_path_buf(),
                    source: io::Error::other(error.to_string()),
                })?;
            }
            let request = match fs::read(path) {
                Ok(current) => at_fs::WriteRequest::replace(
                    path.to_path_buf(),
                    bytes,
                    ExpectedState::Hash(at_fs::sha256_hex(&current)),
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    at_fs::WriteRequest::create(path.to_path_buf(), bytes)
                }
                Err(source) => {
                    return Err(InstallError::Queue {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            };
            at_fs::apply(&request).map_err(|error| InstallError::Queue {
                path: path.to_path_buf(),
                source: io::Error::other(error.to_string()),
            })?;
            Ok(())
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(InstallError::Queue {
                path: path.to_path_buf(),
                source,
            }),
        },
    }
}

/// What happened when the last swap's journal was reconciled at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BundleSwapReconcile {
    /// No journal was pending.
    Nothing,
    /// The retired journal described a swap that never moved anything.
    Disposed,
    /// The new version is running; the previous bundle copy was deleted.
    Committed {
        /// Version now running.
        version: String,
    },
    /// The new version did not start; the previous bundle was restored.
    RolledBack {
        /// Version that did not start.
        version: Option<String>,
    },
    /// The journal describes a state no rule covers; nothing was touched.
    ManualReview {
        /// Why the state is ambiguous.
        reason: String,
    },
}

/// Reconciles a journal left by a previous swap against the version that is running now.
///
/// The phase in the journal is a hint, not proof: a crash can land between a rename and its
/// journal update, so every decision re-reads the directory state.
///
/// # Errors
///
/// [`BundleSwapError`] when a required restore, commit or journal change fails. The journal
/// stays behind for a retry in that case.
pub fn reconcile_bundle_swap(
    journal_path: &Path,
    running_version: &str,
) -> Result<BundleSwapReconcile, BundleSwapError> {
    let Some(journal) = read_bundle_swap_journal(journal_path)? else {
        return Ok(BundleSwapReconcile::Nothing);
    };
    let swap = BundleSwap::from_journal(journal_path.to_path_buf(), journal.clone());
    let target_exists = fs::symlink_metadata(&journal.target).is_ok();
    let rollback_exists = fs::symlink_metadata(&journal.rollback).is_ok();
    match journal.phase {
        BundleSwapPhase::Prepared => {
            if !rollback_exists {
                commit_bundle(&swap)?;
                Ok(BundleSwapReconcile::Disposed)
            } else if !target_exists {
                rollback_bundle(&swap)?;
                Ok(BundleSwapReconcile::RolledBack {
                    version: journal.installed_version.clone(),
                })
            } else {
                Ok(BundleSwapReconcile::ManualReview {
                    reason: "the journal says nothing moved, but both bundles exist".to_owned(),
                })
            }
        }
        BundleSwapPhase::Aside => {
            if !target_exists {
                rollback_bundle(&swap)?;
                return Ok(BundleSwapReconcile::RolledBack {
                    version: journal.installed_version.clone(),
                });
            }
            if !rollback_exists {
                return Ok(BundleSwapReconcile::ManualReview {
                    reason: "the rollback copy is gone".to_owned(),
                });
            }
            settle_installed(&swap, &journal, running_version)
        }
        BundleSwapPhase::Installed => {
            if !rollback_exists {
                // Already committed; only the journal is stale.
                commit_bundle(&swap)?;
                return Ok(BundleSwapReconcile::Disposed);
            }
            if !target_exists {
                return Ok(BundleSwapReconcile::ManualReview {
                    reason: "the installed bundle is gone".to_owned(),
                });
            }
            settle_installed(&swap, &journal, running_version)
        }
        _ => Ok(BundleSwapReconcile::ManualReview {
            reason: "the journal records an unknown phase".to_owned(),
        }),
    }
}

fn settle_installed(
    swap: &BundleSwap,
    journal: &BundleSwapJournal,
    running_version: &str,
) -> Result<BundleSwapReconcile, BundleSwapError> {
    if journal.installed_version.as_deref() == Some(running_version) {
        commit_bundle(swap)?;
        return Ok(BundleSwapReconcile::Committed {
            version: running_version.to_owned(),
        });
    }
    if journal.previous_version.as_deref() == Some(running_version) {
        rollback_bundle(swap)?;
        return Ok(BundleSwapReconcile::RolledBack {
            version: journal.installed_version.clone(),
        });
    }
    Ok(BundleSwapReconcile::ManualReview {
        reason: format!(
            "the running version {running_version} is neither the installed nor the replaced version"
        ),
    })
}

struct ProcessOutput {
    status: std::process::ExitStatus,
    text: String,
}

fn run_bounded(
    program: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<ProcessOutput, ProcessError> {
    if !program.is_absolute() {
        return Err(ProcessError::Spawn {
            program: program.to_path_buf(),
            source: io::Error::from(io::ErrorKind::InvalidInput),
        });
    }
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ProcessError::Spawn {
            program: program.to_path_buf(),
            source,
        })?;
    let output = wait_with_deadline(child, program, timeout)?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(ProcessOutput {
        status: output.status,
        text,
    })
}

fn wait_with_deadline(
    mut child: Child,
    program: &Path,
    timeout: Duration,
) -> Result<std::process::Output, ProcessError> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|source| ProcessError::Output {
                        program: program.to_path_buf(),
                        source,
                    });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcessError::Timeout {
                        program: program.to_path_buf(),
                        seconds: timeout.as_secs(),
                    });
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Spawn {
                    program: program.to_path_buf(),
                    source,
                });
            }
        }
    }
}

/// Formats a queue timestamp for the UI.
#[must_use]
pub fn format_queue_time(unix_ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(unix_ms) * 1_000_000)
        .ok()
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}
