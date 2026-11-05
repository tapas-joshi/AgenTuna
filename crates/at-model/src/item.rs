//! Item kinds and the opaque item identifier.

use core::fmt;
use core::str::FromStr;

use uuid::Uuid;

use crate::wire::wire_enum;

wire_enum! {
    /// What a managed item is.
    pub enum ItemKind {
        /// An agent skill (a folder with `SKILL.md`).
        Skill => "skill",
        /// An MCP server definition.
        McpServer => "mcp_server",
        /// A rule or instruction file.
        Rule => "rule",
    }
}

const PREFIX: &str = "itm_";

/// Why a string is not a valid [`ItemId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ItemIdError {
    /// The `itm_` prefix is missing (this includes the legacy `kind:scope:slug` shape).
    #[error("item id must start with `itm_`: `{0}`")]
    MissingPrefix(String),
    /// The part after the prefix is not a UUID.
    #[error("item id does not contain a valid UUID: `{0}`")]
    InvalidUuid(String),
    /// The UUID is not version 7.
    #[error("item id UUID must be version 7: `{0}`")]
    NotVersion7(String),
    /// The UUID is valid but not lowercase hyphenated.
    #[error("item id UUID must be lowercase and hyphenated: `{0}`")]
    NotCanonical(String),
}

/// Opaque, stable identity of an item: `itm_` followed by a UUIDv7.
///
/// Allocated once at creation and kept across rename and scope moves. It never encodes
/// kind, scope, slug or path, and nothing derives one from a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(Uuid);

impl ItemId {
    /// Allocates a fresh id. Ids from one process sort in creation order.
    // A `Default` impl would hide that every call allocates a new identity.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.0.hyphenated())
    }
}

impl FromStr for ItemId {
    type Err = ItemIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| ItemIdError::MissingPrefix(s.to_owned()))?;
        let uuid = Uuid::parse_str(rest).map_err(|_| ItemIdError::InvalidUuid(s.to_owned()))?;
        if uuid.get_version_num() != 7 {
            return Err(ItemIdError::NotVersion7(s.to_owned()));
        }
        let mut buf = Uuid::encode_buffer();
        if uuid.hyphenated().encode_lower(&mut buf) != rest {
            return Err(ItemIdError::NotCanonical(s.to_owned()));
        }
        Ok(Self(uuid))
    }
}

impl serde::Serialize for ItemId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ItemId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
