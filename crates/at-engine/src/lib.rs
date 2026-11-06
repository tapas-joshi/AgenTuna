//! Scope resolution, effective config, and change planning engine.

pub mod agent_paths;
pub mod deploy;
pub mod effective;
pub mod error;
pub mod health;
pub mod link_mode;
pub mod loader;
pub mod local_scope;
pub mod locks;
pub mod mcp_cli;
pub mod mcp_plan;
pub mod overview;
pub mod presets;
pub mod preview;
pub mod project_sync;
pub mod resolve;
pub mod rule_editor;
pub mod rules;
pub mod skill_add_plan;
pub mod skill_agent;
pub mod skill_import;
pub mod skill_names;
pub mod skill_shadows;
pub mod skill_source;
pub mod skill_templates;
pub mod skill_transfer;
pub mod skill_wizard;
pub mod skills;
pub mod state;
pub mod tombstones;

pub mod change_for_repo;
pub use error::EngineError;
pub mod why;
pub use tombstones::{
    ManifestTombstones, MemoryTombstones, TombstoneError, TombstoneMetadata, TombstoneSource,
    disable, disable_inherited, enable,
};
