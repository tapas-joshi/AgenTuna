//! Canonical AgenTuna store manifests, file storage and every filesystem root.

pub mod config;
pub mod git_contract;
pub mod instance;
pub mod layout;
pub mod local_store;
pub mod manifest;
pub mod pathkey;
pub mod paths;
pub mod platform_roots;
pub mod project_scan;
pub mod skill;

pub use git_contract::{
    COMMITTED_ENTRIES, GITIGNORE_RULES, GitContractError, GitWarning, GitignoreInstall,
    PER_MACHINE_OVERRIDE_FILE, ProjectGitContract, StageOutcome, StoreGitState,
};

pub use local_store::LocalStorePaths;
pub use manifest::{
    CanonicalJsonError, ContentHash, ContentHashError, DisabledAgents, DisabledEntry, FieldError,
    FieldProblem, HashError, ItemSource, LoadedManifest, MCP_IDENTITY_KEYS, ManifestError,
    ManifestItem, PathTrustError, SaveOutcome, SourceKind, StoreManifest, StorePath,
    StorePathError, canonical_json, hash_bytes, hash_mcp_entry, hash_rule_bytes, hash_skill_folder,
    mcp_identity_key, resolve_item_path,
};
pub use pathkey::{PathKey, PathKeyError, PortablePlatform};
pub use paths::{
    AppPaths, GlobalStoreSource, HomeSource, PathEnvironment, PathError, PathOverrides,
    SETTINGS_KEY_GLOBAL_STORE,
};
pub use platform_roots::{
    LINUX_APP_DIR_NAME, PlatformRootError, PlatformRoots, resolve_platform_roots,
};
pub use project_scan::{
    CancelFlag, DEFAULT_MAX_DEPTH, DEFAULT_PROGRESS_EVERY, DEFAULT_ROOT_DIR, DiscoveredProject,
    MAX_SCAN_DEPTH, NoObserver, PROJECT_MARKERS, ProjectDiff, ProjectMarker, ProjectRegistry,
    SKIPPED_DIR_NAMES, ScanApplyError, ScanConfig, ScanObserver, ScanOutcome, ScanProgress,
    ScanWarning, ScanWarningKind, apply_diff, scan, scan_with, spawn_scan,
};
pub use skill::{
    LoadedSkill, SkillReadError, parse_skill_doc, parse_skill_doc_in_folder, read_skill_doc,
    read_skill_doc_in_folder, set_description,
};
