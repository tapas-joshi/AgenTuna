//! Canonical AgenTuna store manifests, file storage and every filesystem root.

pub mod config;
mod fs_identity;
pub mod git_contract;
pub mod instance;
pub mod integrity;
pub mod layout;
pub mod local_store;
pub mod logging;
pub mod manifest;
pub mod manifest_rebuild;
pub mod mcp_store;
pub mod move_journal;
pub mod pathkey;
pub mod paths;
pub mod platform_roots;
pub mod project_scan;
pub mod rebuild;
pub mod remote_backup;
pub mod skill;
pub mod skill_import;
pub mod skill_package;
pub mod store_init;
pub mod store_io;

pub use git_contract::{
    COMMITTED_ENTRIES, GITIGNORE_RULES, GitContractError, GitWarning, GitignoreInstall,
    PER_MACHINE_OVERRIDE_FILE, ProjectGitContract, StageOutcome, StoreGitState,
};

pub use local_store::LocalStorePaths;
pub use manifest::{
    CanonicalJsonError, ContentHash, ContentHashError, DisabledAgents, DisabledEntry, FieldError,
    FieldProblem, HashError, ItemSource, LoadedManifest, MCP_IDENTITY_KEYS, ManifestError,
    ManifestItem, PathTrustError, PlannedManifest, SaveOutcome, SourceKind, StoreManifest,
    StorePath, StorePathError, canonical_json, hash_bytes, hash_mcp_entry, hash_rule_bytes,
    hash_skill_folder, mcp_identity_key, resolve_item_path,
};
pub use mcp_store::{
    LoadedMcpStore, MCP_STORE_MAX_BYTES, MCP_STORE_MAX_SERVERS, McpSaveOutcome, McpStore,
    McpStoreError, expected_state,
};
pub use pathkey::{PathKey, PathKeyError, PortablePlatform};
pub use paths::{
    AppPaths, GlobalStoreSource, HomeSource, PathEnvironment, PathError, PathOverrides,
    SETTINGS_KEY_GLOBAL_STORE,
};
pub use platform_roots::{
    LINUX_APP_DIR_NAME, PlatformRootError, PlatformRoots, default_global_store,
    resolve_platform_roots,
};
pub use project_scan::{
    CancelFlag, DEFAULT_MAX_DEPTH, DEFAULT_PROGRESS_EVERY, DEFAULT_ROOT_DIR, DiscoveredProject,
    MAX_SCAN_DEPTH, NoObserver, PROJECT_MARKERS, ProjectDiff, ProjectMarker, SKIPPED_DIR_NAMES,
    ScanConfig, ScanObserver, ScanOutcome, ScanProgress, ScanWarning, ScanWarningKind, apply_diff,
    scan, scan_with, spawn_scan,
};
pub use remote_backup::{
    BackupError, BackupOutcome, BackupRequest, DeviceNameError, RemoteConflict,
    default_device_name, device_name, is_excluded_from_backup, push_backup, remote_conflict_count,
    restore_from_remote, set_device_name, sync_from_remote, validate_device_name,
};
pub use skill::{
    LoadedSkill, SkillReadError, parse_skill_doc, parse_skill_doc_in_folder, read_skill_doc,
    read_skill_doc_in_folder, set_description,
};
pub use skill_package::{
    LoadedSkillPackage, PackageFile, SKILL_PACKAGE_MAX_BYTES, SKILL_PACKAGE_MAX_DEPTH,
    SKILL_PACKAGE_MAX_FILE_BYTES, SKILL_PACKAGE_MAX_FILES, SKILL_PACKAGE_MAX_PATH_BYTES,
    SkillPackage, SkillPackageError, SkillWriteOutcome,
};

pub use integrity::{DuplicateItemName, StoreIntegrityReport, verify_store};
pub use manifest_rebuild::{
    ManifestRebuildError, ManifestRebuildPlan, ManifestRebuildReport, RebuiltItem,
    SkippedRebuildEntry, apply_manifest_rebuild, plan_manifest_rebuild,
};
pub use rebuild::rebuild_from_app_paths;
pub use store_init::{PlannedStoreInit, StoreInitError, plan_store_init};

pub use store_io::{
    McpFileState, OrphanEntry, PlannedDrop, PlannedImport, ReconcileMode, ReconcileOutcome,
    ReconcilePlan, ReconcileToken, STORE_LOCK_FILE, STORE_LOCK_WAIT, SkippedOrphan, StoreAccess,
    StoreIoError, StoreItemEntry, StoreItemState, StoreLockGuard, StoreSnapshot,
    default_lock_policy, load_store, load_store_with, lock_exclusive, lock_shared, reconcile,
    reconcile_with, save_store, save_store_with, store_lock_path,
};
