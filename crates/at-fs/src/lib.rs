//! Safe write pipeline: diff preview, backup, atomic replace and optimistic concurrency.
//!
//! Unix only for now (macOS first); Windows support arrives with TF-038.
//!
//! Every production mutation of a config or store file goes through [`apply`]. It never
//! overwrites silently: the caller states what it expects to find, and a mismatch is an
//! error that writes nothing.

#![allow(clippy::disallowed_methods)] // at-fs is the canonical safe write pipeline implementation and legitimately calls std::fs primitives.

#[cfg(not(unix))]
compile_error!("at-fs currently supports Unix platforms only");

pub mod atomic;
pub mod backup;
/// Journaled, rollback-capable multi-file changes.
pub use changeset::{
    ApplyContext, ApplyOutcome, ApplyRoots, ChangeSetApplyError, DbOperationRecorder,
    OperationRecorder, apply as apply_changeset, apply_with_roots,
};
pub mod changeset;
pub mod concurrency;
pub mod deploy;
pub mod diff;
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
pub use atomic::{BackupPolicy, WriteMode, WriteOutcome, WriteRequest, apply};
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
    move_aside,
};
pub use hash::sha256_hex;
pub use platform::{
    HostPlatform, LinkAttempt, LinkMade, LinkMethod, LinkRemoval, LinkRemovalRequest, LinkRequest,
    LinkShape, LinkTargetKind, Linker, MODE_JUNCTION, RemovalOperation, StoredLinkMode,
    SystemLinker, classify_link_shape, ensure_mode_supported, host_platform, link_attempt_order,
    link_with_fallback, removal_operation, remove_link,
};
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
