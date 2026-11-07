//! Durability of the safe-write pipeline.
//!
//! Production code always asks the operating system to flush the bytes it wrote. The
//! `testkit` cargo feature — enabled only by crates' dev-dependencies, never by a release
//! build — additionally exposes [`set_fast_sync`](crate::testkit::set_fast_sync), which skips
//! those flushes so file-heavy stateful tests are not bounded by disk durability. The switch
//! cannot exist outside the feature-gated testkit, so a shipped binary cannot turn it on.

use std::fs::File;
use std::io;

#[cfg(feature = "testkit")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "testkit")]
static FAST_SYNC: AtomicBool = AtomicBool::new(false);

/// Flushes `file`'s bytes to the device, unless a test attached fast sync.
pub(crate) fn sync_all(file: &File) -> io::Result<()> {
    #[cfg(feature = "testkit")]
    if FAST_SYNC.load(Ordering::Relaxed) {
        return Ok(());
    }
    file.sync_all()
}

/// Skips every fsync the safe-write pipeline makes for the rest of the process.
///
/// Only compiled with the `testkit` feature, which no production crate enables.
#[cfg(feature = "testkit")]
pub(crate) fn set_fast_sync(enabled: bool) {
    FAST_SYNC.store(enabled, Ordering::Relaxed);
}
