//! Global locks.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{ItemId, Scope};

/// Pins an item to its definition at `scope`, blocking lower scopes from overriding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// Scope that holds the locked definition.
    pub scope: Scope,
    /// Locked item.
    pub item_id: ItemId,
    /// Human-readable reason shown wherever the lock is displayed.
    pub reason: String,
    /// Who placed the lock.
    pub locked_by: String,
    /// When the lock was placed.
    #[serde(with = "time::serde::rfc3339")]
    pub locked_at: OffsetDateTime,
}
