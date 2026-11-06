//! Plain domain types for AgenTuna. No IO, no async, no persistence.

mod wire;

pub mod agent;
pub mod change_set;
pub mod deployment;
pub mod error;
pub mod item;
pub mod lock;
pub mod mcp;
pub mod mcp_validate;
pub mod network;
pub mod preset;
pub mod redact;
pub mod resolve;
pub mod scope;
pub mod skill;
pub use agent::{Agent, AgentId, AgentIdError};
pub use change_set::{
    ChangeSet, ChangeSetError, ContentHash, DeployMode, ExpectedState, FileOp, OPERATION_KINDS,
    OperationKind,
};
pub use deployment::{Deployment, DeploymentState, ItemState, LinkMode, SyncLabel};
pub use error::{ErrorKind, ErrorReport, REDACTION_MASK, Remedy, SecretRedactor};
pub use item::{ItemId, ItemIdError, ItemKind};
pub use lock::Lock;
pub use mcp::{
    EnvValue, McpServer, McpServerDef, McpServerId, McpServerIdError, McpTransport, OAuthProfile,
    SecretRef,
};
pub use mcp_validate::{
    CommandResolver, ExistingServer, FieldCode, FieldError, ServerCandidate, ShadowNotice,
    Validation, ValidationContext, validate_server,
};
pub use network::{
    BackgroundHealth, DenialReason, InvalidHost, NETWORK_EVENT_CAPACITY, NetworkAction,
    NetworkActionState, NetworkActionStatus, NetworkDenied, NetworkEvent, NetworkEventLog,
    NetworkHost, NetworkPolicy, NetworkTrigger,
};
pub use preset::{Preset, PresetMember};
pub use resolve::{Definition, LogicalId, LogicalKey, ScopeSnapshot, normalized_name};
pub use scope::Scope;
pub use skill::{ByteSpan, ParsedSkill, SkillDoc, SkillError, SkillNameMismatch, SkillOrigin};
pub use wire::ParseWireError;
