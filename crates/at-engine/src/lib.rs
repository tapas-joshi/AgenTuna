//! Scope resolution, effective config, and change planning engine.

pub mod error;
pub mod link_mode;
pub mod local_scope;
pub mod locks;
pub mod resolve;
pub mod skill_templates;
pub mod state;
pub mod tombstones;

pub use error::EngineError;
pub use tombstones::{
    ManifestTombstones, MemoryTombstones, TombstoneError, TombstoneSource, disable,
    disable_inherited, enable,
};
