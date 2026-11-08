//! Atomic application-bundle replacement with a retained rollback copy.
//!
//! An in-app update cannot use [`crate::apply`]: the target is a directory tree, not a file, and
//! the previous version must survive until the new one has proven it can start. [`swap_bundle`]
//! performs two same-directory renames — the installed bundle aside to `rollback`, the staged
//! bundle into `target` — after writing a small journal, so an interrupted swap is recognisable
//! and reversible at the next launch. [`commit_bundle`] deletes the rollback copy once the new
//! version reports healthy; [`rollback_bundle`] puts the previous bundle back.
//!
//! Every rename is same-directory: a reader never observes a partially written bundle, only the
//! two short windows between the renames. The caller owns the health policy; this module owns
//! only the file operations and their journal.

use std::fs;
use std::fs::symlink_metadata;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::FsError;
use crate::failpoint::{self, FaultPoint};

/// Version of the on-disk journal this module writes.
pub const BUNDLE_SWAP_JOURNAL_VERSION: u32 = 1;

/// Infix of the temporary file used to replace the journal in one rename.
const JOURNAL_TEMP_INFIX: &str = ".agentuna-tmp-journal";

/// How far a swap got, as recorded in the journal.
///
/// The next launch reads this to decide whether the previous bundle is still intact
/// ([`BundleSwapPhase::Prepared`] and [`BundleSwapPhase::Aside`]) or the new one is installed but
/// unconfirmed ([`BundleSwapPhase::Installed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BundleSwapPhase {
    /// The journal was written; nothing has moved.
    Prepared,
    /// The installed bundle is at `rollback`; the staged bundle is not in place yet.
    Aside,
    /// The staged bundle is installed at `target`; `rollback` holds the previous bundle.
    Installed,
}

/// Durable record of one bundle swap.
///
/// Paths are absolute. `installed_version` and `previous_version` are caller bookkeeping so the
/// next launch can tell which bundle is running without executing anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSwapJournal {
    /// Format version; [`BUNDLE_SWAP_JOURNAL_VERSION`].
    pub version: u32,
    /// Progress of the swap.
    pub phase: BundleSwapPhase,
    /// Installed bundle the swap replaces.
    pub target: PathBuf,
    /// Verified new bundle that becomes `target`.
    pub staged: PathBuf,
    /// Where the previous bundle is kept until the new one is confirmed healthy.
    pub rollback: PathBuf,
    /// Version the new bundle reports, when the caller supplied it.
    #[serde(default)]
    pub installed_version: Option<String>,
    /// Version that was replaced, when the caller supplied it.
    #[serde(default)]
    pub previous_version: Option<String>,
}

/// One bundle swap: the new bundle, the installed bundle and where the old copy goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleSwapRequest {
    /// Verified new `.app` directory.
    pub staged: PathBuf,
    /// Currently installed `.app` directory.
    pub target: PathBuf,
    /// Must not exist yet; receives the previous bundle.
    pub rollback: PathBuf,
    /// Journal file, usually one fixed path per application.
    pub journal: PathBuf,
    /// Version of the new bundle, recorded in the journal.
    pub installed_version: Option<String>,
    /// Version of the replaced bundle, recorded in the journal.
    pub previous_version: Option<String>,
}

/// A completed swap whose previous bundle is still on disk.
///
/// The handle is not a lock: it names the paths the caller must [`commit_bundle`] or
/// [`rollback_bundle`] before the next launch reconciles the journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleSwap {
    journal: BundleSwapJournal,
    journal_path: PathBuf,
}

impl BundleSwap {
    /// Rebuilds a handle from a journal read at startup.
    #[must_use]
    pub fn from_journal(journal_path: PathBuf, journal: BundleSwapJournal) -> Self {
        Self {
            journal,
            journal_path,
        }
    }

    /// The journal this swap is recorded in.
    #[must_use]
    pub fn journal(&self) -> &BundleSwapJournal {
        &self.journal
    }

    /// Path of the journal file.
    #[must_use]
    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    /// The installed bundle.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.journal.target
    }

    /// Where the previous bundle is kept.
    #[must_use]
    pub fn rollback(&self) -> &Path {
        &self.journal.rollback
    }
}

/// Failures while swapping or reconciling an application bundle.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BundleSwapError {
    /// The request describes an unsafe layout; nothing was touched.
    #[error("invalid bundle swap request: {reason}")]
    InvalidRequest {
        /// What is wrong.
        reason: String,
    },
    /// The installed bundle could not be moved aside; `target` is untouched.
    #[error("could not set the installed bundle aside: {source}")]
    Aside {
        /// Underlying IO failure.
        #[source]
        source: Box<FsError>,
    },
    /// The staged bundle could not be installed; the previous bundle is back at `target`.
    #[error("could not install the staged bundle; the previous bundle was restored: {source}")]
    Install {
        /// Underlying IO failure.
        #[source]
        source: Box<FsError>,
    },
    /// The staged bundle could not be installed and the previous bundle could not be restored.
    /// The journal stays on disk for manual recovery.
    #[error(
        "could not install the staged bundle ({source}); restoring the previous bundle also failed ({restore})"
    )]
    InstallUnrecovered {
        /// Failure of the install rename.
        source: Box<FsError>,
        /// Failure of the restore rename.
        restore: Box<FsError>,
    },
    /// The previous bundle could not be put back during an explicit rollback.
    #[error("could not restore the previous bundle: {source}")]
    RestoreFailed {
        /// Underlying IO failure.
        #[source]
        source: Box<FsError>,
    },
    /// The rollback copy could not be deleted after a confirmed-healthy launch.
    #[error("could not remove the previous bundle copy at {}: {source}", .path.display())]
    CleanupFailed {
        /// Rollback path that survived.
        path: PathBuf,
        /// Underlying IO failure.
        #[source]
        source: io::Error,
    },
    /// The journal could not be written, read or removed.
    #[error("bundle swap journal at {}: {source}", .path.display())]
    Journal {
        /// Journal path.
        path: PathBuf,
        /// Underlying IO failure.
        #[source]
        source: io::Error,
    },
    /// The journal exists but is not a record this version understands.
    #[error("bundle swap journal at {} is malformed", .path.display())]
    JournalMalformed {
        /// Journal path.
        path: PathBuf,
    },
    /// The platform has no equivalent of this operation; nothing was touched.
    #[error("the application-bundle swap is unavailable on this platform: {reason}")]
    UnsupportedPlatform {
        /// Why the platform cannot perform it.
        reason: &'static str,
    },
}

impl BundleSwapError {
    /// Whether the previous bundle is provably still intact at `target` or at `rollback`.
    ///
    /// Journal and unrecovered failures make no promise; an operator must inspect the paths.
    #[must_use]
    pub fn previous_bundle_intact(&self) -> bool {
        matches!(
            self,
            Self::InvalidRequest { .. }
                | Self::Aside { .. }
                | Self::Install { .. }
                | Self::UnsupportedPlatform { .. }
        )
    }
}

/// Replaces `target` with `staged`, keeping the previous bundle at `rollback`.
///
/// The journal is written before the first rename and after each phase, so an interrupted run is
/// classified by [`read_bundle_swap_journal`] at the next launch. A failed install rename restores
/// the previous bundle; only a failed restore leaves the journal behind.
///
/// # Errors
///
/// [`BundleSwapError::InvalidRequest`] for an unsafe layout, [`BundleSwapError::Aside`] when the
/// installed bundle cannot be moved, [`BundleSwapError::Install`] when the staged bundle cannot be
/// installed (the previous bundle is restored), and journal errors when the record cannot be
/// maintained.
pub fn swap_bundle(request: &BundleSwapRequest) -> Result<BundleSwap, BundleSwapError> {
    validate_request(request)?;
    let mut journal = BundleSwapJournal {
        version: BUNDLE_SWAP_JOURNAL_VERSION,
        phase: BundleSwapPhase::Prepared,
        target: request.target.clone(),
        staged: request.staged.clone(),
        rollback: request.rollback.clone(),
        installed_version: request.installed_version.clone(),
        previous_version: request.previous_version.clone(),
    };
    write_journal(&request.journal, &journal)?;

    if let Err(source) = rename_checked(
        &request.target,
        &request.rollback,
        FaultPoint::BundleSwapAside,
    ) {
        remove_journal(&request.journal);
        return Err(BundleSwapError::Aside {
            source: Box::new(source),
        });
    }
    journal.phase = BundleSwapPhase::Aside;
    if let Err(error) = write_journal(&request.journal, &journal) {
        // The old bundle is safe at `rollback`, but a crash without the Aside phase would look
        // like a Prepared swap. Restore now; if that also fails the journal stays for recovery.
        if rename_plain(&request.rollback, &request.target).is_ok() {
            remove_journal(&request.journal);
        }
        return Err(error);
    }

    if let Err(source) = rename_checked(
        &request.staged,
        &request.target,
        FaultPoint::BundleSwapInstall,
    ) {
        return Err(restore_previous(request, Box::new(source)));
    }

    journal.phase = BundleSwapPhase::Installed;
    if let Err(error) = write_journal(&request.journal, &journal) {
        // The new bundle is in place but the journal cannot say so; put the old one back rather
        // than leave a swap that the next launch cannot classify.
        let restored = rename_plain(&request.rollback, &request.target);
        if restored.is_ok() {
            let _ = fs::remove_dir_all(&request.staged);
            remove_journal(&request.journal);
        }
        return Err(error);
    }

    Ok(BundleSwap {
        journal,
        journal_path: request.journal.clone(),
    })
}

/// Puts the previous bundle back and discards the unconfirmed new one.
///
/// The new tree is moved to `staged` (a same-directory rename) before the old one is restored, so
/// a failed restore still leaves the new bundle recoverable rather than deleted.
///
/// # Errors
///
/// [`BundleSwapError::RestoreFailed`] when the previous bundle cannot be put back, and
/// [`BundleSwapError::Journal`] when the journal cannot be removed. The rollback copy stays on
/// disk in both cases.
pub fn rollback_bundle(swap: &BundleSwap) -> Result<(), BundleSwapError> {
    let journal = &swap.journal;
    if symlink_metadata(&journal.rollback).is_err() {
        return Err(BundleSwapError::RestoreFailed {
            source: Box::new(FsError::NotFound {
                path: journal.rollback.clone(),
            }),
        });
    }
    if symlink_metadata(&journal.target).is_ok() {
        let _ = fs::remove_dir_all(&journal.staged);
        if let Some(parent) = journal.staged.parent() {
            fs::create_dir_all(parent).map_err(|source| BundleSwapError::RestoreFailed {
                source: Box::new(FsError::from_io(parent, source)),
            })?;
        }
        rename_checked(
            &journal.target,
            &journal.staged,
            FaultPoint::BundleSwapInstall,
        )
        .map_err(|source| BundleSwapError::RestoreFailed {
            source: Box::new(source),
        })?;
    }
    if let Err(source) = rename_plain(&journal.rollback, &journal.target) {
        // Put the unconfirmed bundle back so the installation is no worse than before.
        let _ = rename_plain(&journal.staged, &journal.target);
        return Err(BundleSwapError::RestoreFailed {
            source: Box::new(source),
        });
    }
    let _ = fs::remove_dir_all(&journal.staged);
    remove_journal(&swap.journal_path);
    Ok(())
}

/// Deletes the previous bundle after the new version reported healthy.
///
/// # Errors
///
/// [`BundleSwapError::CleanupFailed`] when the rollback copy cannot be removed; the journal is
/// then kept so the next launch retries. Journal removal failures surface as
/// [`BundleSwapError::Journal`].
pub fn commit_bundle(swap: &BundleSwap) -> Result<(), BundleSwapError> {
    if symlink_metadata(&swap.journal.rollback).is_ok() {
        fs::remove_dir_all(&swap.journal.rollback).map_err(|source| {
            BundleSwapError::CleanupFailed {
                path: swap.journal.rollback.clone(),
                source,
            }
        })?;
    }
    remove_journal(&swap.journal_path);
    Ok(())
}

/// Reads the pending swap journal, when one exists.
///
/// # Errors
///
/// [`BundleSwapError::Journal`] when the file cannot be read and
/// [`BundleSwapError::JournalMalformed`] when it is not a supported record.
pub fn read_bundle_swap_journal(path: &Path) -> Result<Option<BundleSwapJournal>, BundleSwapError> {
    match fs::read(path) {
        Ok(bytes) => {
            let journal: BundleSwapJournal =
                serde_json::from_slice(&bytes).map_err(|_| BundleSwapError::JournalMalformed {
                    path: path.to_path_buf(),
                })?;
            if journal.version != BUNDLE_SWAP_JOURNAL_VERSION {
                return Err(BundleSwapError::JournalMalformed {
                    path: path.to_path_buf(),
                });
            }
            Ok(Some(journal))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(BundleSwapError::Journal {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Restores the previous bundle after an interrupted or failed install rename.
fn restore_previous(request: &BundleSwapRequest, source: Box<FsError>) -> BundleSwapError {
    match rename_plain(&request.rollback, &request.target) {
        Ok(()) => {
            remove_journal(&request.journal);
            BundleSwapError::Install { source }
        }
        Err(restore) => BundleSwapError::InstallUnrecovered {
            source,
            restore: Box::new(restore),
        },
    }
}

fn validate_request(request: &BundleSwapRequest) -> Result<(), BundleSwapError> {
    for (label, path) in [
        ("staged", &request.staged),
        ("target", &request.target),
        ("rollback", &request.rollback),
        ("journal", &request.journal),
    ] {
        if !path.is_absolute() {
            return Err(invalid(format!("{label} path is not absolute")));
        }
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(invalid(format!("{label} path contains `..`")));
        }
    }
    require_directory("staged bundle", &request.staged)?;
    require_directory("installed bundle", &request.target)?;
    if symlink_metadata(&request.rollback).is_ok() {
        return Err(invalid("rollback path already exists"));
    }
    let rollback_parent = request
        .rollback
        .parent()
        .ok_or_else(|| invalid("rollback path has no parent"))?;
    require_directory("rollback parent", rollback_parent)?;
    let journal_parent = request
        .journal
        .parent()
        .ok_or_else(|| invalid("journal path has no parent"))?;
    require_directory("journal parent", journal_parent)?;
    if within(&request.rollback, &request.target) || within(&request.staged, &request.rollback) {
        return Err(invalid("rollback path is inside another swap path"));
    }
    if within(&request.staged, &request.target) || within(&request.target, &request.staged) {
        return Err(invalid("staged and installed bundles overlap"));
    }
    if within(&request.journal, &request.target) {
        return Err(invalid("journal path is inside the installed bundle"));
    }
    let device = device_of(&request.staged)?;
    for (label, path) in [
        ("installed bundle", request.target.as_path()),
        ("rollback parent", rollback_parent),
        ("journal parent", journal_parent),
    ] {
        if device_of(path)? != device {
            return Err(invalid(format!(
                "{label} is on a different filesystem than the staged bundle"
            )));
        }
    }
    Ok(())
}

fn require_directory(label: &str, path: &Path) -> Result<(), BundleSwapError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| invalid(format!("{label} does not exist: {}", path.display())))?;
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!("{label} is a symlink")));
    }
    if !metadata.is_dir() {
        return Err(invalid(format!("{label} is not a directory")));
    }
    Ok(())
}

/// The filesystem identity of `path`, used to prove every swap path shares one volume.
fn device_of(path: &Path) -> Result<u64, BundleSwapError> {
    #[cfg(unix)]
    {
        fs::metadata(path)
            .map(|metadata| metadata.dev())
            .map_err(|_| invalid(format!("cannot read the filesystem of {}", path.display())))
    }
    #[cfg(not(unix))]
    {
        // The swap renames a macOS `.app` bundle. Windows ships a different update mechanism
        // (an installer), and there is no volume identity available here without an unsafe
        // call, so the whole operation reports itself unavailable instead of skipping the check.
        let _ = path;
        Err(BundleSwapError::UnsupportedPlatform {
            reason: "the macOS application-bundle swap has no Windows equivalent",
        })
    }
}

fn within(candidate: &Path, ancestor: &Path) -> bool {
    candidate.starts_with(ancestor) && candidate != ancestor
}

fn invalid(reason: impl Into<String>) -> BundleSwapError {
    BundleSwapError::InvalidRequest {
        reason: reason.into(),
    }
}

fn rename_checked(from: &Path, to: &Path, point: FaultPoint) -> Result<(), FsError> {
    if let Err(failure) = failpoint::hit(point, to) {
        return Err(rename_error(from, to, failure.into_io()));
    }
    fs::rename(from, to).map_err(|error| rename_error(from, to, error))
}

fn rename_plain(from: &Path, to: &Path) -> Result<(), FsError> {
    fs::rename(from, to).map_err(|error| rename_error(from, to, error))
}

fn rename_error(from: &Path, to: &Path, error: io::Error) -> FsError {
    if crate::errno::is_cross_device(&error) {
        FsError::CrossDeviceRename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        }
    } else {
        FsError::from_platform_io(to, error)
    }
}

fn write_journal(path: &Path, journal: &BundleSwapJournal) -> Result<(), BundleSwapError> {
    let parent = path.parent().ok_or_else(|| BundleSwapError::Journal {
        path: path.to_path_buf(),
        source: io::Error::from(io::ErrorKind::NotFound),
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BundleSwapError::Journal {
            path: path.to_path_buf(),
            source: io::Error::from(io::ErrorKind::InvalidInput),
        })?;
    let temp = parent.join(format!(
        ".{file_name}{JOURNAL_TEMP_INFIX}-{}",
        std::process::id()
    ));
    let bytes = serde_json::to_vec(journal).map_err(|_| BundleSwapError::JournalMalformed {
        path: path.to_path_buf(),
    })?;
    let write = || -> io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        crate::durability::sync_all(&file)
    };
    if let Err(source) = write() {
        let _ = fs::remove_file(&temp);
        return Err(BundleSwapError::Journal {
            path: path.to_path_buf(),
            source,
        });
    }
    if let Err(source) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(BundleSwapError::Journal {
            path: path.to_path_buf(),
            source,
        });
    }
    sync_directory(parent);
    Ok(())
}

fn remove_journal(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        sync_directory(parent);
    }
}

fn sync_directory(path: &Path) {
    // Best effort: the rename is already durable on journaling filesystems, and a directory that
    // cannot be opened (for example on a read-only volume) must not turn success into failure.
    if let Ok(directory) = fs::File::open(path) {
        let _ = crate::durability::sync_all(&directory);
    }
}

// Unix-only fixtures: POSIX symlinks, permission bits and raw errno constants. The
// Windows twins arrive with the slice that enables the Windows test suite.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::failpoint::{Fault, TestInjector};
    use std::sync::Arc;

    fn request(root: &Path) -> BundleSwapRequest {
        BundleSwapRequest {
            staged: root.join("staging/AgenTuna.app"),
            target: root.join("Applications/AgenTuna.app"),
            rollback: root.join("rollback/AgenTuna-0.1.0.app"),
            journal: root.join("rollback/bundle-swap.json"),
            installed_version: Some("0.2.0".to_owned()),
            previous_version: Some("0.1.0".to_owned()),
        }
    }

    fn prepare(root: &Path) -> BundleSwapRequest {
        let request = request(root);
        fs::create_dir_all(request.staged.join("Contents")).expect("staged bundle");
        fs::write(request.staged.join("Contents/version"), b"0.2.0").expect("new version");
        fs::create_dir_all(request.target.join("Contents")).expect("installed bundle");
        fs::write(request.target.join("Contents/version"), b"0.1.0").expect("old version");
        fs::create_dir_all(root.join("rollback")).expect("rollback parent");
        request
    }

    #[test]
    fn swap_keeps_the_previous_bundle_until_commit() {
        let root = tempfile::tempdir().expect("temp root");
        let request = prepare(root.path());

        let swap = swap_bundle(&request).expect("swap succeeds");
        assert_eq!(
            fs::read(request.target.join("Contents/version")).expect("new bundle"),
            b"0.2.0"
        );
        assert_eq!(
            fs::read(request.rollback.join("Contents/version")).expect("old bundle"),
            b"0.1.0"
        );
        let journal = read_bundle_swap_journal(&request.journal)
            .expect("journal readable")
            .expect("journal present");
        assert_eq!(journal.phase, BundleSwapPhase::Installed);
        assert_eq!(journal.installed_version.as_deref(), Some("0.2.0"));

        commit_bundle(&swap).expect("commit removes the rollback copy");
        assert!(!request.rollback.exists());
        assert!(
            read_bundle_swap_journal(&request.journal)
                .expect("journal readable")
                .is_none()
        );
    }

    #[test]
    fn rollback_restores_the_previous_bundle_and_removes_the_new_one() {
        let root = tempfile::tempdir().expect("temp root");
        let request = prepare(root.path());
        let swap = swap_bundle(&request).expect("swap succeeds");

        rollback_bundle(&swap).expect("rollback restores the old bundle");
        assert_eq!(
            fs::read(request.target.join("Contents/version")).expect("restored bundle"),
            b"0.1.0"
        );
        assert!(!request.rollback.exists());
        assert!(!request.staged.exists());
        assert!(
            read_bundle_swap_journal(&request.journal)
                .expect("journal readable")
                .is_none()
        );
    }

    #[test]
    fn a_failed_install_rename_leaves_the_previous_bundle_in_place() {
        let root = tempfile::tempdir().expect("temp root");
        let request = prepare(root.path());
        let injector = Arc::new(TestInjector::new());
        injector.arm(FaultPoint::BundleSwapInstall, Fault::Fail(libc::EACCES));

        let result = crate::failpoint::with_injector(injector, || swap_bundle(&request));

        assert!(matches!(result, Err(BundleSwapError::Install { .. })));
        assert_eq!(
            fs::read(request.target.join("Contents/version")).expect("old bundle kept"),
            b"0.1.0"
        );
        assert!(!request.rollback.exists());
        // The staged bundle was never moved, so it survives for a retry.
        assert!(request.staged.exists());
        assert!(
            read_bundle_swap_journal(&request.journal)
                .expect("journal readable")
                .is_none()
        );
    }

    #[test]
    fn a_failed_aside_rename_changes_nothing() {
        let root = tempfile::tempdir().expect("temp root");
        let request = prepare(root.path());
        let injector = Arc::new(TestInjector::new());
        injector.arm(FaultPoint::BundleSwapAside, Fault::Fail(libc::EACCES));

        let result = crate::failpoint::with_injector(injector, || swap_bundle(&request));

        assert!(matches!(result, Err(BundleSwapError::Aside { .. })));
        assert!(request.staged.exists());
        assert_eq!(
            fs::read(request.target.join("Contents/version")).expect("old bundle kept"),
            b"0.1.0"
        );
        assert!(
            read_bundle_swap_journal(&request.journal)
                .expect("journal readable")
                .is_none()
        );
    }

    #[test]
    fn refuses_a_rollback_inside_the_installed_bundle() {
        let root = tempfile::tempdir().expect("temp root");
        let mut request = prepare(root.path());
        request.rollback = request.target.join("old.app");
        assert!(matches!(
            swap_bundle(&request),
            Err(BundleSwapError::InvalidRequest { .. })
        ));
        assert!(request.staged.exists());
        assert!(!request.journal.exists());
    }

    #[test]
    fn a_malformed_journal_is_reported_not_ignored() {
        let root = tempfile::tempdir().expect("temp root");
        let journal = root.path().join("bundle-swap.json");
        fs::write(&journal, b"not json").expect("malformed journal");
        assert!(matches!(
            read_bundle_swap_journal(&journal),
            Err(BundleSwapError::JournalMalformed { .. })
        ));
    }
}
