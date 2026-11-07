//! Resolution inputs: definitions found in one scope and their logical identity.
//!
//! These types are the pure, IO-free half of scope resolution. [`crate::Scope`]
//! orders the layers; the merge itself lives in `at-engine::resolve`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{ItemId, ItemKind, Scope};

/// Normalizes a display name so the same item compares equal across scopes.
///
/// Surrounding whitespace is trimmed, the result is lowercased, brought to
/// Unicode NFC and stripped of trailing periods. `" Code-Review "`,
/// `"code-review"` and `"code-review."` therefore compare equal, as do a
/// composed name and its decomposed equivalent.
///
/// Only the comparison form is returned; [`Definition::name`] keeps the spelling
/// that was authored so the UI can display it.
#[must_use]
pub fn normalized_name(raw: &str) -> String {
    let mut normalized: String = raw.trim().to_lowercase().nfc().collect();
    while normalized.ends_with('.') {
        normalized.pop();
    }
    normalized
}

/// One item definition found in one scope layer.
///
/// A definition is a declaration, not a file operation: it names the item, points at
/// where it was found and carries whichever identities the source provided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    /// What the definition is.
    pub kind: ItemKind,
    /// Name as authored. Never canonicalized here: use [`Definition::normalized_name`]
    /// for comparison and this field for display.
    pub name: String,
    /// Identity the definition declares for itself.
    ///
    /// An MCP server's canonical `id` belongs here; when it is absent the server merges
    /// by name instead and the fallback stays visible on its
    /// [`LogicalId::is_mcp_name_fallback`]. Skills and rules identify by name only and
    /// leave this `None`.
    pub declared_id: Option<String>,
    /// AgenTuna's opaque identity, when the definition is managed by a store.
    ///
    /// It plays no part in merging (skills and rules merge by name, MCP servers by
    /// [`Definition::declared_id`]) and exists to link a resolved item to locks and rows.
    pub item_id: Option<ItemId>,
    /// File or directory the definition was read from.
    pub path: PathBuf,
}

impl Definition {
    /// The comparison form of [`Definition::name`].
    #[must_use]
    pub fn normalized_name(&self) -> String {
        normalized_name(&self.name)
    }

    /// The identity this definition shares with its copies in other scopes.
    ///
    /// Skills and rules merge by normalized name. MCP servers merge by their declared
    /// id, falling back to the normalized name when no id was declared.
    #[must_use]
    pub fn logical_id(&self) -> LogicalId {
        let key = match self.kind {
            ItemKind::McpServer => match &self.declared_id {
                Some(id) => LogicalKey::Declared(id.clone()),
                None => LogicalKey::Name(self.normalized_name()),
            },
            ItemKind::Skill | ItemKind::Rule => LogicalKey::Name(self.normalized_name()),
        };
        LogicalId {
            kind: self.kind,
            key,
        }
    }
}

/// Identity shared by every copy of one item across scopes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogicalId {
    /// Kind the identity belongs to; identities of different kinds never merge.
    pub kind: ItemKind,
    /// Merge key inside the kind.
    pub key: LogicalKey,
}

impl LogicalId {
    /// Whether this is an MCP server that merged by name because it declared no id.
    #[must_use]
    pub fn is_mcp_name_fallback(&self) -> bool {
        self.kind == ItemKind::McpServer && matches!(self.key, LogicalKey::Name(_))
    }
}

/// The merge key of a [`LogicalId`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogicalKey {
    /// Normalized name. For an MCP server this records that it declared no id.
    Name(String),
    /// Identifier declared by the definition, compared verbatim (an MCP server `id`).
    Declared(String),
}

/// Every definition found in one scope.
///
/// A scope with nothing defined is either left out or supplied with no definitions;
/// both resolve the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSnapshot {
    /// Scope these definitions live in.
    pub scope: Scope,
    /// Definitions found there, in source order.
    pub definitions: Vec<Definition>,
}
