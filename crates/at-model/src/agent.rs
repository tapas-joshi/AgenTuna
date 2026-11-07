//! Agent identity and detection state.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

const BUILTIN: &[(&str, AgentId)] = &[
    ("claude_code", AgentId::ClaudeCode),
    ("codex", AgentId::Codex),
    ("omp", AgentId::Omp),
    ("cursor", AgentId::Cursor),
];
const CUSTOM_PREFIX: &str = "custom:";
const MAX_CUSTOM_LEN: usize = 64;

/// Why a string is not a valid [`AgentId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentIdError {
    /// Neither a built-in name nor `custom:<name>`.
    #[error("unknown agent `{0}`; expected claude_code, codex, omp, cursor or custom:<name>")]
    Unknown(String),
    /// The custom name is empty, too long or has characters outside `[a-z0-9_-]`.
    #[error("invalid custom agent name `{0}`; use 1-64 characters from [a-z0-9_-]")]
    InvalidCustomName(String),
    /// The custom name collides with a built-in agent.
    #[error("custom agent name `{0}` is reserved by a built-in agent")]
    ReservedCustomName(String),
}

/// Identifies a coding agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum AgentId {
    /// Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// Oh My Pi.
    Omp,
    /// Cursor.
    Cursor,
    /// A user-defined agent, wire form `custom:<name>`.
    Custom(String),
}

impl AgentId {
    /// Builds a custom agent id after validating the name.
    ///
    /// # Errors
    /// Returns [`AgentIdError`] when the name is malformed or reserved.
    pub fn custom(name: &str) -> Result<Self, AgentIdError> {
        let valid = !name.is_empty()
            && name.len() <= MAX_CUSTOM_LEN
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !valid {
            return Err(AgentIdError::InvalidCustomName(name.to_owned()));
        }
        if BUILTIN.iter().any(|(builtin, _)| *builtin == name) {
            return Err(AgentIdError::ReservedCustomName(name.to_owned()));
        }
        Ok(Self::Custom(name.to_owned()))
    }
}

impl AgentId {
    /// Human-readable name used in UI copy before an agent reports its own label.
    ///
    /// Detection records this as the `agents.display_name` seed, so screens never
    /// fall back to the wire id (`claude_code`) unless the agent is custom.
    #[must_use]
    pub fn default_display_name(&self) -> &str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Omp => "omp",
            Self::Cursor => "Cursor",
            Self::Custom(name) => name,
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => f.write_str("claude_code"),
            Self::Codex => f.write_str("codex"),
            Self::Omp => f.write_str("omp"),
            Self::Cursor => f.write_str("cursor"),
            Self::Custom(name) => write!(f, "{CUSTOM_PREFIX}{name}"),
        }
    }
}

impl FromStr for AgentId {
    type Err = AgentIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((_, id)) = BUILTIN.iter().find(|(name, _)| *name == s) {
            return Ok(id.clone());
        }
        match s.strip_prefix(CUSTOM_PREFIX) {
            Some(name) => Self::custom(name),
            None => Err(AgentIdError::Unknown(s.to_owned())),
        }
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for AgentId {
    type Error = AgentIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// An agent as known to AgenTuna: identity plus detection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Stable identity.
    pub id: AgentId,
    /// Name shown in the UI.
    pub display_name: String,
    /// Whether an installation was detected on this machine.
    pub detected: bool,
    /// Detected version string, when the agent reports one.
    pub version: Option<String>,
}
