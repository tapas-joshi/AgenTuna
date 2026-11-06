//! Canonical AgenTuna store manifests, file storage and every filesystem root.

pub mod config;
pub mod instance;
pub mod layout;
pub mod local_store;
pub mod pathkey;
pub mod paths;
pub mod platform_roots;
pub mod skill;

pub use local_store::LocalStorePaths;
pub use pathkey::{PathKey, PathKeyError, PortablePlatform};
pub use paths::{
    AppPaths, GlobalStoreSource, HomeSource, PathEnvironment, PathError, PathOverrides,
    SETTINGS_KEY_GLOBAL_STORE,
};
pub use platform_roots::{
    LINUX_APP_DIR_NAME, PlatformRootError, PlatformRoots, resolve_platform_roots,
};
pub use skill::{
    LoadedSkill, SkillReadError, parse_skill_doc, parse_skill_doc_in_folder, read_skill_doc,
    read_skill_doc_in_folder, set_description,
};
