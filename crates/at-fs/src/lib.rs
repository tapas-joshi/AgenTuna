//! Safe write pipeline: diff preview, backup, atomic replace and optimistic concurrency.
//!
//! Unix only for now (macOS first); Windows support arrives with TF-038.
//!
//! Every production mutation of a config or store file goes through [`apply`]. It never
//! overwrites silently: the caller states what it expects to find, and a mismatch is an
//! error that writes nothing.

#[cfg(not(unix))]
compile_error!("at-fs currently supports Unix platforms only");

pub mod atomic;
pub mod backup;
pub mod concurrency;
pub mod deploy;
pub mod diff;
mod error;
pub mod guard;
mod hash;
pub mod symlink;
pub mod watch;

/// Deployment mode, re-exported so callers of [`deploy()`] need no extra import.
pub use at_model::LinkMode;
pub use atomic::{BackupPolicy, WriteMode, WriteOutcome, WriteRequest, apply};
pub use backup::{BackupError, BackupMeta, BackupSidecar};
pub use concurrency::ExpectedState;
pub use deploy::{
    COPY_MANIFEST_FILE, COPY_MANIFEST_VERSION, CopyManifest, CopyManifestEntry, CopyOutcome,
    DeployOutcome, DeployRequest, LinkOutcome, RemoveOutcome, RemoveRequest, RemovedDeployment,
    copy_dir, deploy, link_dir, remove_deployment,
};
#[doc(hidden)]
pub use deploy::{DeployFs, SystemFs, deploy_with};
pub use error::FsError;
pub use guard::{
    Classification, DeploymentLookup, MoveAsideOutcome, OverwriteGuard, OwnershipContext,
    OwnershipGuard, Remedy, TargetKind, classify_target, create_parents_if_target_absent,
    move_aside,
};
pub use hash::sha256_hex;
pub use symlink::SymlinkWrite;

/// Largest file `at-fs` will read or write in one operation (64 MiB).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
