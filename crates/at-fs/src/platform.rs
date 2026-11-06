//! Platform selection for directory links, file links, junctions and copies (TF-038).
//!
//! The decision logic in this module is platform-neutral: callers pass a [`HostPlatform`], so
//! macOS tests can assert the Windows fallback order and removal rules. Only [`SystemLinker`]
//! and [`remove_link`] contain `cfg(windows)` code.
//!
//! **The Windows implementation has never run here.** The crate is Unix-only until the Windows
//! port lifts the `compile_error!` in `lib.rs`; the `#[cfg(windows)]` code and its tests only
//! compile on a Windows CI runner. Everything else in this module is covered by macOS tests.
//!
//! A junction is a reparse point: removing it must use `RemoveDirectory`, which deletes the
//! point and never the directory it points at. A symbolic link is removed with `remove_file`.
//! Neither removal is allowed to touch a target outside an AgenTuna store root.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::FsError;
use crate::guard::resolve_existing_prefix;

/// `ERROR_PRIVILEGE_NOT_HELD` from `CreateSymbolicLinkW` on Windows.
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// The operating system a decision is made for.
///
/// Passed explicitly instead of read from `cfg!` inside the decision functions so tests on one
/// platform can verify the rules for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HostPlatform {
    /// macOS.
    MacOs,
    /// Linux and other Unix-like systems.
    Linux,
    /// Windows.
    Windows,
    /// Anything else.
    Other,
}

/// The platform this binary runs on.
#[must_use]
pub const fn host_platform() -> HostPlatform {
    #[cfg(target_os = "macos")]
    {
        HostPlatform::MacOs
    }
    #[cfg(target_os = "linux")]
    {
        HostPlatform::Linux
    }
    #[cfg(windows)]
    {
        HostPlatform::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        HostPlatform::Other
    }
}

/// What a link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkTargetKind {
    /// A directory (a skill folder).
    Directory,
    /// A regular file.
    File,
}

/// One concrete way to materialise a deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkMethod {
    /// A symbolic link (the preferred method on every platform).
    Symlink,
    /// A directory junction (Windows only, needs no administrator rights).
    Junction,
    /// A recursive copy with a deletion-free relationship to the store.
    Copy,
}

/// The `deployments.mode` value of a junction deployment.
///
/// The column CHECK must accept it; TF-013 owns that migration and the DB lane was told.
pub const MODE_JUNCTION: &str = "junction";

/// Deployment mode as stored in `deployments.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StoredLinkMode {
    /// A symbolic link; stored as `symlink`.
    Symlink,
    /// A directory junction; stored as [`MODE_JUNCTION`].
    Junction,
    /// A recursive copy; stored as `copy`.
    Copy,
}

impl StoredLinkMode {
    /// The text stored in `deployments.mode`.
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::Junction => MODE_JUNCTION,
            Self::Copy => "copy",
        }
    }

    /// Parses a `deployments.mode` value. `None` for `native`, `none` and unknown text.
    #[must_use]
    pub fn from_db(text: &str) -> Option<Self> {
        match text {
            "symlink" => Some(Self::Symlink),
            MODE_JUNCTION => Some(Self::Junction),
            "copy" => Some(Self::Copy),
            _ => None,
        }
    }

    /// The link method that produced this mode.
    #[must_use]
    pub const fn method(self) -> LinkMethod {
        match self {
            Self::Symlink => LinkMethod::Symlink,
            Self::Junction => LinkMethod::Junction,
            Self::Copy => LinkMethod::Copy,
        }
    }
}

/// Rejects a stored mode this platform cannot have produced (TF-038 acceptance: a junction row
/// read on macOS is refused by the platform guard).
///
/// `path` is the deployment path from the row, so the error names something the user recognises.
///
/// # Errors
/// [`FsError::UnsupportedPlatform`] when `mode` is a junction and `platform` is not Windows.
pub fn ensure_mode_supported(
    mode: StoredLinkMode,
    platform: HostPlatform,
    path: &Path,
) -> Result<(), FsError> {
    if mode == StoredLinkMode::Junction && platform != HostPlatform::Windows {
        return Err(FsError::UnsupportedPlatform {
            path: path.to_path_buf(),
            reason: "a directory junction exists only on Windows",
        });
    }
    Ok(())
}

/// Ordered attempts for `kind`, most faithful first.
///
/// Windows tries a real symlink first and falls back to a junction for directories when the
/// process lacks `SeCreateSymbolicLinkPrivilege`; file symlinks fall back to a copy, never to a
/// junction. Unix tries a symlink and then, only when the caller allows it, a copy.
#[must_use]
pub fn link_attempt_order(
    platform: HostPlatform,
    kind: LinkTargetKind,
    allow_junction: bool,
    allow_copy: bool,
) -> Vec<LinkMethod> {
    let mut attempts = vec![LinkMethod::Symlink];
    if platform == HostPlatform::Windows && allow_junction && kind == LinkTargetKind::Directory {
        attempts.push(LinkMethod::Junction);
    }
    if allow_copy {
        attempts.push(LinkMethod::Copy);
    }
    attempts
}

/// Filesystem primitives the Windows half of the deployment primitive needs.
///
/// Injected so the fallback order can be tested without a Windows machine, and so the
/// production implementation (which needs the `junction` crate) stays behind `cfg(windows)`.
///
/// `copy_dir` is the low-level copy used for a file-link fallback; the deployment-level copy
/// with a manifest remains [`crate::deploy::copy_dir`] and is not reimplemented here.
pub trait Linker: fmt::Debug {
    /// Creates a symbolic link to a directory.
    ///
    /// # Errors
    /// The platform refuses the link (for example missing symlink privilege on Windows).
    fn symlink_dir(&self, link_target: &Path, link_path: &Path) -> io::Result<()>;
    /// Creates a symbolic link to a file.
    ///
    /// # Errors
    /// The platform refuses the link.
    fn symlink_file(&self, link_target: &Path, link_path: &Path) -> io::Result<()>;
    /// Creates an NTFS directory junction.
    ///
    /// # Errors
    /// [`io::ErrorKind::Unsupported`] on every non-Windows platform.
    fn junction_dir(&self, link_target: &Path, link_path: &Path) -> io::Result<()>;
    /// Copies a directory tree into an absent destination.
    ///
    /// # Errors
    /// IO errors, or [`io::ErrorKind::InvalidInput`] for a symlink or special file inside the
    /// tree (this low-level copy never follows or recreates links).
    fn copy_dir(&self, source: &Path, destination: &Path) -> io::Result<()>;
}

/// The real linker for the running platform.
#[derive(Debug, Clone, Copy)]
pub struct SystemLinker;

impl Linker for SystemLinker {
    fn symlink_dir(&self, link_target: &Path, link_path: &Path) -> io::Result<()> {
        sys_symlink_dir(link_target, link_path)
    }

    fn symlink_file(&self, link_target: &Path, link_path: &Path) -> io::Result<()> {
        sys_symlink_file(link_target, link_path)
    }

    fn junction_dir(&self, link_target: &Path, link_path: &Path) -> io::Result<()> {
        sys_junction_dir(link_target, link_path)
    }

    fn copy_dir(&self, source: &Path, destination: &Path) -> io::Result<()> {
        copy_tree(source, destination)
    }
}

fn sys_symlink_dir(link_target: &Path, link_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(link_target, link_path)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(link_target, link_path)
    }
}

fn sys_symlink_file(link_target: &Path, link_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(link_target, link_path)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(link_target, link_path)
    }
}

fn sys_junction_dir(link_target: &Path, link_path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        junction::create(link_target, link_path)
    }
    #[cfg(not(windows))]
    {
        let _ = (link_target, link_path);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory junctions exist only on Windows",
        ))
    }
}

/// A recursive copy that refuses symlinks and special files instead of following them.
fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{}: the linker does not copy symbolic links; use the deployment copy",
                    from.display()
                ),
            ));
        }
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: not a regular file or directory", from.display()),
            ));
        }
    }
    Ok(())
}

/// One deployment to materialise.
#[derive(Debug, Clone)]
pub struct LinkRequest<'a> {
    /// Canonical store directory.
    pub source: &'a Path,
    /// Path inside the agent folder; must not exist yet.
    pub link: &'a Path,
    /// Directory or file.
    pub kind: LinkTargetKind,
    /// Allow the Windows junction fallback for directories.
    pub allow_junction: bool,
    /// Allow the copy fallback when every link method is refused.
    pub allow_copy: bool,
}

/// One method that was tried and failed.
#[derive(Debug)]
pub struct LinkAttempt {
    /// Method that failed.
    pub method: LinkMethod,
    /// What the platform returned.
    pub error: io::Error,
}

/// The method that succeeded and what was tried before it.
#[derive(Debug)]
pub struct LinkMade {
    /// Method that created the deployment.
    pub method: LinkMethod,
    /// Earlier methods, in order, that the platform refused.
    pub attempts: Vec<LinkAttempt>,
}

/// Creates the deployment with the first method the platform accepts.
///
/// A method only falls back when the platform refused it in a recoverable way (missing symlink
/// privilege, `EPERM`, `ENOTSUP`, `ENOSYS` or [`io::ErrorKind::Unsupported`]). Any other error,
/// such as the destination already existing, is returned unchanged so a real failure is never
/// masked by a copy.
///
/// # Errors
/// [`FsError::LinkUnsupported`] when every method was refused for a link reason, or the IO error
/// of the last attempt otherwise.
pub fn link_with_fallback(
    linker: &dyn Linker,
    platform: HostPlatform,
    request: &LinkRequest<'_>,
) -> Result<LinkMade, FsError> {
    let order = link_attempt_order(
        platform,
        request.kind,
        request.allow_junction,
        request.allow_copy,
    );
    let mut attempts: Vec<LinkAttempt> = Vec::new();
    for method in order {
        let result = match method {
            LinkMethod::Symlink => match request.kind {
                LinkTargetKind::Directory => linker.symlink_dir(request.source, request.link),
                LinkTargetKind::File => linker.symlink_file(request.source, request.link),
            },
            LinkMethod::Junction => linker.junction_dir(request.source, request.link),
            LinkMethod::Copy => linker.copy_dir(request.source, request.link),
        };
        match result {
            Ok(()) => return Ok(LinkMade { method, attempts }),
            Err(error) => {
                let recoverable = is_link_refusal(&error);
                attempts.push(LinkAttempt { method, error });
                if !recoverable {
                    break;
                }
            }
        }
    }
    let Some(last) = attempts.pop() else {
        return Err(FsError::LinkUnsupported {
            path: request.link.to_path_buf(),
            source: io::Error::new(io::ErrorKind::Unsupported, "no link method was attempted"),
        });
    };
    if last.method == LinkMethod::Symlink && is_link_refusal(&last.error) {
        Err(FsError::LinkUnsupported {
            path: request.link.to_path_buf(),
            source: last.error,
        })
    } else {
        Err(FsError::from_io(request.link, last.error))
    }
}

fn is_link_refusal(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    let Some(code) = error.raw_os_error() else {
        return false;
    };
    if code == ERROR_PRIVILEGE_NOT_HELD {
        return true;
    }
    #[cfg(unix)]
    {
        code == libc::EPERM
            || code == libc::ENOTSUP
            || code == libc::EOPNOTSUPP
            || code == libc::ENOSYS
    }
    #[cfg(not(unix))]
    {
        // `EPERM` is 1 on Windows as well.
        code == 1
    }
}

/// What kind of entry sits at a deployment path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkShape {
    /// A symlink to a file.
    SymlinkFile,
    /// A symlink to a directory.
    SymlinkDir,
    /// An NTFS directory junction (Windows only).
    Junction,
    /// A copy directory carrying a manifest; the caller decides this from the manifest.
    CopyDir,
    /// A real directory.
    RealDir,
    /// A regular file.
    File,
    /// Nothing exists at the path.
    Missing,
}

/// The filesystem call that removes a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RemovalOperation {
    /// `remove_file`: deletes the link entry, never its target.
    RemoveFile,
    /// `RemoveDirectory`: deletes the reparse point, never its target.
    RemoveDirectory,
}

/// Chooses the removal call for a shape, refusing combination that cannot exist here.
///
/// # Errors
/// [`FsError::UnsupportedPlatform`] for a junction on a non-Windows platform, and
/// [`FsError::NotFound`] for [`LinkShape::Missing`].
pub fn removal_operation(
    shape: LinkShape,
    platform: HostPlatform,
    path: &Path,
) -> Result<RemovalOperation, FsError> {
    match shape {
        LinkShape::Junction if platform != HostPlatform::Windows => {
            Err(FsError::UnsupportedPlatform {
                path: path.to_path_buf(),
                reason: "a directory junction exists only on Windows",
            })
        }
        LinkShape::Junction | LinkShape::CopyDir | LinkShape::RealDir => {
            Ok(RemovalOperation::RemoveDirectory)
        }
        LinkShape::SymlinkFile | LinkShape::SymlinkDir | LinkShape::File => {
            Ok(RemovalOperation::RemoveFile)
        }
        LinkShape::Missing => Err(FsError::NotFound {
            path: path.to_path_buf(),
        }),
    }
}

/// Classifies what sits at `path` without following a final link.
///
/// Directory copies and real directories look the same here; the caller uses the copy manifest
/// to tell them apart. On non-Windows platforms [`LinkShape::Junction`] never occurs.
///
/// # Errors
/// [`FsError::NotFound`] never occurs (absence is [`LinkShape::Missing`]); IO errors do.
pub fn classify_link_shape(path: &Path, platform: HostPlatform) -> Result<LinkShape, FsError> {
    #[cfg(not(windows))]
    let _ = platform;
    #[cfg(windows)]
    {
        if platform == HostPlatform::Windows {
            match junction::exists(path) {
                Ok(true) => return Ok(LinkShape::Junction),
                Ok(false) => {}
                Err(e) => return Err(FsError::from_io(path, e)),
            }
        }
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LinkShape::Missing),
        Err(e) => Err(FsError::from_io(path, e)),
        Ok(meta) if meta.file_type().is_symlink() => {
            let points_at_dir = fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
            Ok(if points_at_dir {
                LinkShape::SymlinkDir
            } else {
                LinkShape::SymlinkFile
            })
        }
        Ok(meta) if meta.is_dir() => Ok(LinkShape::RealDir),
        Ok(_) => Ok(LinkShape::File),
    }
}

/// Everything one link removal needs.
#[derive(Debug, Clone)]
pub struct LinkRemovalRequest {
    /// The link or junction to delete.
    pub path: PathBuf,
    /// AgenTuna store roots; the link target must resolve inside one of them.
    pub store_roots: Vec<PathBuf>,
    /// What sits at `path`, from [`classify_link_shape`].
    pub shape: LinkShape,
}

/// What was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRemoval {
    /// The link path that was deleted.
    pub path: PathBuf,
    /// The call that was used.
    pub operation: RemovalOperation,
}

/// Removes a symlink or junction after proving its target is inside an AgenTuna store.
///
/// The target itself is never touched: a junction is removed with `RemoveDirectory` (which
/// deletes the reparse point, not the directory it references) and a symlink with `remove_file`.
/// Copies and real directories belong to [`crate::deploy::remove_deployment`] and are refused
/// here.
///
/// # Errors
/// [`FsError::DeploymentNotOwned`] when the shape is not a link or the resolved target escapes
/// every store root; [`FsError::UnsupportedPlatform`] for a junction off Windows.
pub fn remove_link(
    request: &LinkRemovalRequest,
    platform: HostPlatform,
) -> Result<LinkRemoval, FsError> {
    let operation = removal_operation(request.shape, platform, &request.path)?;
    if !matches!(
        request.shape,
        LinkShape::SymlinkFile | LinkShape::SymlinkDir | LinkShape::Junction
    ) {
        return Err(FsError::DeploymentNotOwned {
            path: request.path.clone(),
            reason: "remove_link deletes symlinks and junctions only; copies use remove_deployment",
        });
    }
    let raw_target = match request.shape {
        LinkShape::Junction => junction_target(&request.path)?,
        _ => fs::read_link(&request.path).map_err(|e| FsError::from_io(&request.path, e))?,
    };
    let joined = if raw_target.is_absolute() {
        raw_target
    } else {
        request
            .path
            .parent()
            .ok_or_else(|| FsError::NoParent {
                path: request.path.clone(),
            })?
            .join(raw_target)
    };
    let resolved = resolve_existing_prefix(&joined)?;
    let mut inside = false;
    for root in &request.store_roots {
        let resolved_root = resolve_existing_prefix(root)?;
        if resolved.starts_with(&resolved_root) {
            inside = true;
            break;
        }
    }
    if !inside {
        return Err(FsError::DeploymentNotOwned {
            path: request.path.clone(),
            reason: "the link target resolves outside every AgenTuna store root",
        });
    }
    match operation {
        RemovalOperation::RemoveFile => {
            fs::remove_file(&request.path).map_err(|e| FsError::from_io(&request.path, e))?;
        }
        RemovalOperation::RemoveDirectory => {
            fs::remove_dir(&request.path).map_err(|e| FsError::from_io(&request.path, e))?;
        }
    }
    Ok(LinkRemoval {
        path: request.path.clone(),
        operation,
    })
}

fn junction_target(path: &Path) -> Result<PathBuf, FsError> {
    #[cfg(windows)]
    {
        junction::get_target(path).map_err(|e| FsError::from_io(path, e))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(FsError::UnsupportedPlatform {
            path: path.to_path_buf(),
            reason: "a directory junction exists only on Windows",
        })
    }
}
