//! Safe write pipeline, diffing, atomic writes, backups, and file watching.

pub mod atomic;
pub mod backup;
pub mod concurrency;
pub mod diff;
pub mod symlink;
pub mod watch;
