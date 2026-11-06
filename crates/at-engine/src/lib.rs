//! Scope resolution, effective config, and change planning engine.

pub mod error;
pub mod local_scope;
pub mod locks;
pub mod resolve;
pub mod skill_templates;
pub mod state;

pub use error::EngineError;
