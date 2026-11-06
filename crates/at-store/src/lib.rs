//! Canonical AgenTuna store manifests, file storage and every filesystem root.

pub mod paths;
pub mod skill;

pub use paths::{
    AppPaths, GlobalStoreSource, HomeSource, PathEnvironment, PathError, PathOverrides,
    SETTINGS_KEY_GLOBAL_STORE,
};
pub use skill::{
    LoadedSkill, SkillReadError, parse_skill_doc, parse_skill_doc_in_folder, read_skill_doc,
    read_skill_doc_in_folder, set_description,
};
