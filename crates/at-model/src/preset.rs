//! Presets: named sets of items applied together.

use serde::{Deserialize, Serialize};

use crate::{ItemId, ItemKind, Scope};

/// A member of a [`Preset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetMember {
    /// Referenced item.
    pub item_id: ItemId,
    /// Kind of the referenced item.
    pub kind: ItemKind,
    /// Whether applying the preset enables this member.
    pub enabled: bool,
}

/// A named set of items that can be applied to an agent or project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// Stable preset identifier.
    pub id: String,
    /// Display name, unique per scope ignoring case.
    pub name: String,
    /// Free-text description.
    pub description: String,
    /// Scope the preset belongs to.
    pub scope: Scope,
    /// Shipped with AgenTuna rather than user-created.
    pub is_builtin: bool,
    /// Items in the preset.
    pub members: Vec<PresetMember>,
}
