//! Asking the filesystem whether this process may create an entry in a directory.
//!
//! The safe-write pipeline needs the answer before it starts: a call that is going to fail on
//! permissions should fail with one typed error and nothing staged, not halfway through a write.
//! Every caller that needs the answer asks here, so the platform difference is described once.
//!
//! Unix asks `access(2)` with `W_OK | X_OK`, which honours ACLs the mode bits alone would miss.
//! Windows has no ACL query without an unsafe Win32 call, so the probe creates exactly the kind of
//! entry the caller will create and removes it again. Neither arm leaves anything behind.

use std::io;
use std::path::Path;

#[cfg(not(unix))]
use std::fs;
#[cfg(not(unix))]
use std::sync::atomic::{AtomicU64, Ordering};

/// Checks that `directory` accepts a new entry, without leaving one behind.
///
/// # Errors
/// The OS error that stopped the check: [`io::ErrorKind::PermissionDenied`] when the directory
/// refuses a new entry, and any other IO error (such as [`io::ErrorKind::NotFound`]) when the
/// directory cannot be inspected at all.
pub fn probe_directory_write(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::access(
            directory,
            rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let probe = directory.join(format!(".agentuna-probe-{}-{n}", std::process::id()));
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        drop(created);
        fs::remove_file(&probe)
    }
}
