//! Raw operating-system error codes the safe-write pipeline has to classify.
//!
//! `std::io::Error::kind()` is not precise enough for the overwrite and backup guarantees: a
//! "cross-device" rename, a "directory not empty" refusal and a "sharing violation" all arrive
//! as one of a handful of generic kinds. [`std::io::Error::raw_os_error`] is precise, but its
//! numbers are platform-specific, so every code lives here with a `unix` arm built from `libc`
//! and a `windows` arm built from the Win32 `ERROR_*` values Windows actually returns.
//!
//! A predicate with no equivalent on a platform returns `false`, which makes the caller fall
//! back to its generic path (a typed [`crate::FsError`] built from the error kind) instead of
//! guessing that an operation meant something it did not.

use std::io;

/// `libc` has no Windows arm; these tables are the only place raw codes are spelled out.
#[cfg(unix)]
mod codes {
    /// The rename or link would cross filesystems.
    pub(super) const CROSS_DEVICE: &[i32] = &[libc::EXDEV];
    /// The platform refuses to create the link at all.
    pub(super) const LINK_REFUSAL: &[i32] =
        &[libc::EPERM, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS];
    /// The directory that would be replaced still holds entries.
    pub(super) const DIR_NOT_EMPTY: &[i32] = &[libc::ENOTEMPTY, libc::EEXIST];
    /// The path resolves through a symlink cycle.
    pub(super) const SYMLINK_LOOP: &[i32] = &[libc::ELOOP];
    /// The volume is full.
    pub(super) const NO_SPACE: &[i32] = &[libc::ENOSPC];
    /// A quota is exhausted.
    pub(super) const QUOTA_EXCEEDED: &[i32] = &[libc::EDQUOT];
    /// Unix file locks do not block a rename, so nothing maps to a sharing violation.
    pub(super) const SHARING_VIOLATION: &[i32] = &[];
}

#[cfg(windows)]
mod codes {
    /// `ERROR_NOT_SAME_DEVICE`.
    pub(super) const CROSS_DEVICE: &[i32] = &[17];
    /// `ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED`, `ERROR_PRIVILEGE_NOT_HELD`.
    pub(super) const LINK_REFUSAL: &[i32] = &[1, 50, 1314];
    /// `ERROR_DIR_NOT_EMPTY`, `ERROR_ALREADY_EXISTS`.
    pub(super) const DIR_NOT_EMPTY: &[i32] = &[145, 183];
    /// `ERROR_CANT_ACCESS_FILE`, `ERROR_CANT_RESOLVE_FILENAME`: a reparse cycle.
    pub(super) const SYMLINK_LOOP: &[i32] = &[1920, 1921];
    /// `ERROR_HANDLE_DISK_FULL`, `ERROR_DISK_FULL`.
    pub(super) const NO_SPACE: &[i32] = &[39, 112];
    /// `ERROR_DISK_QUOTA_EXCEEDED`.
    pub(super) const QUOTA_EXCEEDED: &[i32] = &[1295];
    /// `ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`: another process holds the file open.
    pub(super) const SHARING_VIOLATION: &[i32] = &[32, 33];
}

fn has(error: &io::Error, table: &[i32]) -> bool {
    match error.raw_os_error() {
        Some(code) => table.contains(&code),
        None => false,
    }
}

/// Whether the error reports a rename or link across filesystems.
pub(crate) fn is_cross_device(error: &io::Error) -> bool {
    has(error, codes::CROSS_DEVICE)
}

/// `ERROR_PRIVILEGE_NOT_HELD`, Windows' "the process lacks `SeCreateSymbolicLinkPrivilege`".
///
/// Checked on every platform because the link policy is decided for an injected
/// [`crate::HostPlatform`]: a macOS test asserts the Windows fallback order by handing the
/// decision function this exact code, and a real Windows build receives it from
/// `CreateSymbolicLinkW`.
const PRIVILEGE_NOT_HELD: i32 = 1314;

/// Whether the platform refused to create a link for a reason another method may survive.
pub(crate) fn is_link_refusal(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || has(error, codes::LINK_REFUSAL)
        || error.raw_os_error() == Some(PRIVILEGE_NOT_HELD)
}

/// Whether the refusal means the directory that would be replaced still holds entries.
pub(crate) fn is_dir_not_empty(error: &io::Error) -> bool {
    has(error, codes::DIR_NOT_EMPTY)
}

/// Whether the error reports a symlink cycle.
pub(crate) fn is_symlink_loop(error: &io::Error) -> bool {
    has(error, codes::SYMLINK_LOOP)
}

/// Whether the write failed because the volume is full.
pub(crate) fn is_no_space(error: &io::Error) -> bool {
    has(error, codes::NO_SPACE)
}

/// Whether the write failed because a quota is exhausted.
pub(crate) fn is_quota_exceeded(error: &io::Error) -> bool {
    has(error, codes::QUOTA_EXCEEDED)
}

/// Whether another process holds the file open, so a rename must be retried.
///
/// Only Windows reports this; on Unix the predicate is always `false` and the retry loop in
/// [`crate::atomic`] performs its single attempt without sleeping.
pub(crate) fn is_sharing_violation(error: &io::Error) -> bool {
    has(error, codes::SHARING_VIOLATION)
}

/// Raw code used to synthesise an IO failure in fault-injected restore paths.
///
/// `5` is `EIO` on Unix and `ERROR_ACCESS_DENIED` on Windows; both are honest "the write failed"
/// reports for a failure the caller injected on purpose.
pub(crate) const FAULT_IO: i32 = 5;
