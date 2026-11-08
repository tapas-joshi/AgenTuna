//! Deciding whether two `Metadata` values describe the same on-disk entry.
//!
//! Reading a store document safely means looking the path up, opening it, and then proving the
//! open handle is the entry the lookup described, so a swap between the two calls is refused
//! instead of being read as if it were the expected file.
//!
//! Unix answers with device and inode. Windows `std` exposes no file id without an unstable
//! feature, so creation time, size and last-write time stand in for it: on NTFS a file replaced
//! between the two calls differs in at least one of them. Both answers are conservative in the
//! same direction — a false negative refuses the read with "changed" and retries, a false positive
//! cannot happen for a genuinely different entry.

use std::fs;

/// Whether `opened` is the same on-disk entry as `before`.
pub(crate) fn same_file_identity(opened: &fs::Metadata, before: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        opened.dev() == before.dev() && opened.ino() == before.ino()
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::fs::MetadataExt;
        opened.creation_time() == before.creation_time()
            && opened.file_size() == before.file_size()
            && opened.last_write_time() == before.last_write_time()
    }
}
