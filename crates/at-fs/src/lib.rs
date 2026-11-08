//! Safe write pipeline: diff preview, backup, atomic replace and optimistic concurrency.
//!
//! Every production mutation of a config or store file goes through [`apply`]. It never
//! overwrites silently: the caller states what it expects to find, and a mismatch is an
//! error that writes nothing.
//!
//! # Platforms
//!
//! macOS and Linux are the primary targets; Windows is compiled and exercised by the `windows`
//! CI job since TF-038 slice 1. The pipeline keeps the same guarantees on Windows with three
//! documented differences, each of which is a platform limit rather than a policy choice:
//!
//! * **No directory fsync.** Windows offers no equivalent of `fsync` on a directory, so after the
//!   rename the directory entry ordering is not forced to disk (see [`atomic`]). The rename itself
//!   is still atomic, and the new bytes are flushed before it.
//! * **Renames are retried.** Windows refuses to replace a file another process holds open
//!   (`ERROR_SHARING_VIOLATION`), where Unix succeeds; [`atomic`] retries a bounded number of times
//!   with a short backoff and then fails with a typed error. Nothing is left half-written.
//! * **No POSIX mode bits.** Permission modes are captured and restored on Unix only. An
//!   operation that cannot provide its guarantee on Windows returns
//!   [`FsError::UnsupportedPlatform`] instead of reporting success.

#![allow(clippy::disallowed_methods)] // at-fs is the canonical safe write pipeline implementation and legitimately calls std::fs primitives.

pub mod atomic;
pub mod backup;
pub mod bundle;
/// Journaled, rollback-capable multi-file changes.
pub use changeset::{
    ApplyContext, ApplyOutcome, ApplyRoots, ChangeSetApplyError, DbOperationRecorder,
    OperationRecorder, RollbackHandle, apply as apply_changeset, apply_with_roots,
    apply_with_roots_and_registry, apply_with_roots_reversible,
    apply_with_roots_reversible_and_registry, commit_reversible, hash_node, rollback_committed,
};
pub mod changeset;
pub mod concurrency;
pub mod deploy;
pub mod diff;
mod durability;
mod errno;
mod error;
/// Test-only fault injection. Compiled only with the `faults` or `testkit` cargo feature, or in test builds.
#[cfg(any(test, feature = "testkit", feature = "faults"))]
pub mod failpoint;
#[cfg(not(any(test, feature = "testkit", feature = "faults")))]
pub(crate) mod failpoint;
pub mod guard;
mod hash;
pub mod managed_block;
pub mod platform;
pub mod privilege;
pub mod probe;
pub mod recovery;
pub mod remedies;
pub mod restore;
pub mod strategy;
pub mod symlink;
/// Shared test fixtures. Compiled only with the `testkit` cargo feature.
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod watch;

/// Deployment mode, re-exported so callers of [`deploy()`] need no extra import.
pub use at_model::LinkMode;
pub use atomic::{BackupPolicy, WriteMode, WriteOutcome, WriteRequest, apply, apply_with_registry};
pub use backup::BackupError::BackupFailed;
pub use backup::{BackupError, BackupFiles, BackupMeta, BackupSidecar, operation_backup_dir};
pub use concurrency::ExpectedState;
pub use deploy::{
    COPY_MANIFEST_FILE, COPY_MANIFEST_VERSION, CopyManifest, CopyManifestEntry, CopyOutcome,
    DeployOutcome, DeployRequest, LinkOutcome, RemoveOutcome, RemoveRequest, RemovedDeployment,
    copy_dir, deploy, install_dir_tree, link_dir, remove_deployment,
};
#[doc(hidden)]
pub use deploy::{DeployFs, SystemFs, deploy_with, install_dir_tree_with};
pub use error::FsError;
pub use guard::{
    Classification, DeploymentLookup, MoveAsideOutcome, OverwriteGuard, OwnershipContext,
    OwnershipGuard, Remedy, TargetKind, classify_target, create_parents_if_target_absent,
    ensure_dir, ensure_dir_with_mode, move_aside, set_directory_mode, set_permissions,
};
pub use hash::sha256_hex;
pub use platform::{
    HostPlatform, LinkAttempt, LinkMade, LinkMethod, LinkRemoval, LinkRemovalRequest, LinkRequest,
    LinkShape, LinkTargetKind, Linker, MODE_JUNCTION, RemovalOperation, StoredLinkMode,
    SystemLinker, classify_link_shape, ensure_mode_supported, host_platform, link_attempt_order,
    link_with_fallback, removal_operation, remove_link,
};
pub use probe::probe_directory_write;
#[doc(hidden)]
pub use remedies::MoveFs;
/// TF-042's store move. Kept out of the root re-export because
/// [`guard::move_aside`] already owns that name (TF-005's same-parent rename).
pub use remedies::{
    ADOPT_FLOW_ID, InverseMove, MoveAction, MoveAsideRequest, MoveDraft, MoveJournal,
    MoveOperationId, MoveRecord, MoveStrategy, RefusalRemedies, RefusalRemedy, UndoMoveOutcome,
    encode_path, undo_move,
};
pub use restore::{RestoreError, RestoreOutcome, RestoreRequest, restore_backup};
#[doc(hidden)]
pub use strategy::deploy_skill_with;
pub use strategy::{
    SkillDeployOutcome, SkillDeployRequest, SkillStrategy, SkillWarning, SystemVolumes,
    VolumeLookup, deploy_skill, normalize_error, normalize_io, skill_mode,
};
pub use symlink::SymlinkWrite;
pub use watch::{
    DEFAULT_BURST_QUIET, DEFAULT_DEBOUNCE, DEFAULT_MAX_BATCH_PATHS, DEFAULT_QUEUE_CAPACITY,
    DEFAULT_START_TIMEOUT, DEFAULT_STOP_TIMEOUT, DEFAULT_SUPPRESS_TTL, IgnoreRules, RecentWrites,
    WatchBatch, WatchChange, WatchConfig, WatchError, WatchEvent, WatchRoot, WatchRootKind,
    WatchService, WatchStats,
};

/// Largest file `at-fs` will read or write in one operation (64 MiB).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
