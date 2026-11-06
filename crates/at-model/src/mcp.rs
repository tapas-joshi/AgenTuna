//! Canonical MCP server definitions and their transport types.
//!
//! Every layer converts to and from these types: the store writes them, adapters
//! project them to per-agent documents, the engine plans against them and the app
//! edits them. Nothing here performs IO or resolves a secret.
//!
//! The transport is an enum, so code that needs a command cannot run against an
//! HTTP server by accident. Environment values and headers hold either a literal
//! or a reference to a secret in the OS credential store, never a resolved value.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::Scope;

const PREFIX: &str = "mcp_";

/// Why a string is not a valid [`McpServerId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum McpServerIdError {
    /// The `mcp_` prefix is missing.
    #[error("mcp server id must start with `mcp_`: `{0}`")]
    MissingPrefix(String),
    /// The part after the prefix is not a UUID.
    #[error("mcp server id does not contain a valid UUID: `{0}`")]
    InvalidUuid(String),
    /// The UUID is not version 7.
    #[error("mcp server id UUID must be version 7: `{0}`")]
    NotVersion7(String),
    /// The UUID is valid but not lowercase hyphenated.
    #[error("mcp server id UUID must be lowercase and hyphenated: `{0}`")]
    NotCanonical(String),
}

/// Opaque identity of an MCP server: `mcp_` followed by a UUIDv7.
///
/// Deliberately a distinct type from [`crate::ItemId`]: a server id is never a
/// store item id or a slug, and the two never convert implicitly. The store
/// provides the bridge between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct McpServerId(Uuid);

impl McpServerId {
    /// Allocates a fresh id. Ids from one process sort in creation order.
    // A `Default` impl would hide that every call allocates a new identity.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl fmt::Display for McpServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.0.hyphenated())
    }
}

impl FromStr for McpServerId {
    type Err = McpServerIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| McpServerIdError::MissingPrefix(s.to_owned()))?;
        let uuid =
            Uuid::parse_str(rest).map_err(|_| McpServerIdError::InvalidUuid(s.to_owned()))?;
        if uuid.get_version_num() != 7 {
            return Err(McpServerIdError::NotVersion7(s.to_owned()));
        }
        let mut buf = Uuid::encode_buffer();
        if uuid.hyphenated().encode_lower(&mut buf) != rest {
            return Err(McpServerIdError::NotCanonical(s.to_owned()));
        }
        Ok(Self(uuid))
    }
}

impl Serialize for McpServerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for McpServerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Reference to a secret held in the OS credential store.
///
/// It stores the environment name the agent will consume and the Keychain account
/// the value lives in; the value itself is never part of a model.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Environment variable name used when the config is rendered, e.g. `GITHUB_TOKEN`.
    pub env_name: String,
    /// Keychain account that stores the value.
    pub keychain_account: String,
}

impl fmt::Debug for SecretRef {
    /// Prints the two references only; there is no resolved value to print.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretRef")
            .field("env_name", &self.env_name)
            .field("keychain_account", &self.keychain_account)
            .finish()
    }
}

/// Value of one environment entry or HTTP header.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    /// A plain value stored as-is, e.g. `info`.
    Literal(String),
    /// A reference to a secret: `env_name` plus the Keychain account holding it.
    Secret(SecretRef),
}

impl fmt::Debug for EnvValue {
    /// Literals may hold a pasted credential, so they never reach a log through
    /// `Debug`; the secret variant prints its two references.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => f.write_str("Literal(***)"),
            Self::Secret(secret) => f.debug_tuple("Secret").field(secret).finish(),
        }
    }
}

/// OAuth profile for an HTTP MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthProfile {
    /// Authorization server issuer; an absolute URL, validated on save.
    pub issuer: String,
    /// Client id registered with the authorization server.
    pub client_id: String,
    /// Scopes the client requests.
    pub scopes: Vec<String>,
    /// Keychain account that stores the token. A reference, never a token.
    pub token_ref: String,
}

/// How AgenTuna talks to an MCP server.
///
/// The two variants are the only two modes: stdio code cannot run against an HTTP
/// server and vice versa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// A child process speaking MCP over stdin and stdout.
    Stdio {
        /// Executable to run; never a shell string.
        command: String,
        /// Arguments passed verbatim to the executable.
        args: Vec<String>,
        /// Working directory, or `None` to inherit the caller's.
        cwd: Option<PathBuf>,
        /// Environment entries for the child process.
        env: BTreeMap<String, EnvValue>,
    },
    /// A streamable HTTP endpoint.
    Http {
        /// Absolute endpoint URL.
        url: Url,
        /// Request headers, including secret references for credentials.
        headers: BTreeMap<String, EnvValue>,
        /// OAuth configuration, when the server requires it.
        oauth: Option<OAuthProfile>,
    },
}

impl McpTransport {
    /// The stable transport tag (`stdio` or `http`) used in JSON.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http { .. } => "http",
        }
    }

    /// Whether this transport launches a child process.
    #[must_use]
    pub const fn is_stdio(&self) -> bool {
        matches!(self, Self::Stdio { .. })
    }

    /// Whether this transport talks to an HTTP endpoint.
    #[must_use]
    pub const fn is_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }
}

/// The content of an MCP server definition, without identity, scope or path.
///
/// This is what the canonical store holds per slug; identity, kind and scope live
/// in the store manifest. Unknown per-server keys are carried opaquely in `extra`
/// and survive load then save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerDef {
    /// Name shown in the app and in agent documents.
    pub display_name: String,
    /// Optional longer description.
    pub description: Option<String>,
    /// Transport and its settings.
    pub transport: McpTransport,
    /// Native keys AgenTuna does not model, carried opaquely.
    ///
    /// A `BTreeMap` gives a deterministic lexicographic order, so a load then save
    /// keeps every unknown key and its position relative to the others byte-stable.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One MCP server as every layer of AgenTuna passes it around.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    /// Stable identity of the server.
    pub id: McpServerId,
    /// Name shown in the app and in agent documents.
    pub display_name: String,
    /// Optional longer description.
    pub description: Option<String>,
    /// Transport and its settings.
    pub transport: McpTransport,
    /// Scope the definition was resolved from.
    pub scope: Scope,
    /// Native keys AgenTuna does not model, carried opaquely.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl McpServer {
    /// Assembles a server from its identity, scope and stored content.
    #[must_use]
    pub fn from_def(id: McpServerId, scope: Scope, def: McpServerDef) -> Self {
        Self {
            id,
            display_name: def.display_name,
            description: def.description,
            transport: def.transport,
            scope,
            extra: def.extra,
        }
    }

    /// Splits the server into the content the canonical store persists.
    #[must_use]
    pub fn into_def(self) -> McpServerDef {
        McpServerDef {
            display_name: self.display_name,
            description: self.description,
            transport: self.transport,
            extra: self.extra,
        }
    }
}
