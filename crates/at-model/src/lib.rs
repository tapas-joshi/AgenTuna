//! Plain domain types for AgenTuna. No IO, no async, no persistence.

mod wire;

pub mod agent;
pub mod deployment;
pub mod item;
pub mod lock;
pub mod preset;
pub mod scope;

pub use agent::{Agent, AgentId, AgentIdError};
pub use deployment::{Deployment, ItemState, LinkMode, SyncLabel};
pub use item::{ItemId, ItemIdError, ItemKind};
pub use lock::Lock;
pub use preset::{Preset, PresetMember};
pub use scope::Scope;
pub use wire::ParseWireError;
