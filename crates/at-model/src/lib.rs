//! Plain domain types for AgenTuna. No IO, no async, no persistence.

mod wire;

pub mod agent;
pub mod deployment;
pub mod error;
pub mod item;
pub mod lock;
pub mod mcp;
pub mod preset;
pub mod resolve;
pub mod scope;
pub mod skill;

pub use agent::{Agent, AgentId, AgentIdError};
pub use deployment::{Deployment, ItemState, LinkMode, SyncLabel};
pub use error::{ErrorKind, ErrorReport, REDACTION_MASK, Remedy, SecretRedactor};
pub use item::{ItemId, ItemIdError, ItemKind};
pub use lock::Lock;
pub use mcp::{
    EnvValue, McpServer, McpServerDef, McpServerId, McpServerIdError, McpTransport, OAuthProfile,
    SecretRef,
};
pub use preset::{Preset, PresetMember};
pub use resolve::{Definition, LogicalId, LogicalKey, ScopeSnapshot, normalized_name};
pub use scope::Scope;
pub use skill::{ByteSpan, ParsedSkill, SkillDoc, SkillError, SkillNameMismatch, SkillOrigin};
pub use wire::ParseWireError;
