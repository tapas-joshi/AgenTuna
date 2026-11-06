//! Scope resolution: one winning definition per logical id, every loser reported.
//!
//! Precedence is nearest-wins: Local > Project > User > Global ([`Scope::precedence`]).
//! Skills and rules merge by normalized name; MCP servers merge by their declared id,
//! falling back to name when they declare none. Rule *content* composition across
//! scopes is a rendering concern (TA-011), not a merge decision here.
//!
//! Resolution is pure: it reads only the [`ResolveInput`] it is given. Later layers of
//! the feature (Global locks in TA-002, disable tombstones in TA-003) attach to the same
//! input and the same [`ResolvedSet`] without changing this function's signature.
//!
//! ## Global locks
//!
//! [`ResolveInput::with_locks`] attaches a [`LockStore`]. A lock pins its item to the
//! definition at the locking scope, so that definition wins even when a nearer scope
//! defines the same logical id; the winner carries the lock in
//! [`ResolvedItem::lock`] and every other definition of that logical id is reported as
//! blocked ([`Shadowed::blocked`], the story's `Blocked{lock_id}`) instead of merely
//! shadowed. The store is read on every call, never cached: a lock created by the CLI
//! is visible in an already running GUI on its next resolve. A lock whose item has no
//! definition in the given layers pins nothing and is left for
//! [`crate::locks::inspect`] to report as an orphan.

use core::fmt;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use at_model::{AgentId, Definition, ItemId, LogicalId, Scope, ScopeSnapshot};
use at_store::manifest::DisabledAgents;

use crate::locks::{LockMark, LockRecord, LockStore};
use crate::tombstones::TombstoneSource;

/// Input to [`resolve`].
///
/// Fields are private on purpose: later resolution inputs (Global locks, tombstones,
/// agent filters) arrive as additional constructor and builder methods, so every
/// caller written against [`ResolveInput::new`] keeps compiling.
#[derive(Clone, Copy, Default)]
pub struct ResolveInput<'a> {
    layers: &'a [ScopeSnapshot],
    locks: Option<&'a dyn LockStore>,
    tombstones: Option<&'a dyn TombstoneSource>,
    agent: Option<&'a AgentId>,
}

impl fmt::Debug for ResolveInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolveInput")
            .field("layers", &self.layers)
            .field("locks_attached", &self.locks.is_some())
            .field("tombstones_attached", &self.tombstones.is_some())
            .field("agent", &self.agent)
            .finish()
    }
}

impl<'a> ResolveInput<'a> {
    /// Resolution over the given scope layers, supplied in any order.
    #[must_use]
    pub const fn new(layers: &'a [ScopeSnapshot]) -> Self {
        Self {
            layers,
            locks: None,
            tombstones: None,
            agent: None,
        }
    }

    /// Attaches the Global lock source.
    ///
    /// The store is read on every [`resolve`] call, so locks created after the
    /// input was built take effect on the next call without rebuilding it.
    #[must_use]
    pub const fn with_locks(mut self, locks: &'a dyn LockStore) -> Self {
        self.locks = Some(locks);
        self
    }

    /// Attaches the per-scope tombstone source (TA-003).
    #[must_use]
    pub const fn with_tombstones(mut self, tombstones: &'a dyn TombstoneSource) -> Self {
        self.tombstones = Some(tombstones);
        self
    }

    /// Limits resolution to a specific agent, so per-agent tombstones apply (TA-003).
    #[must_use]
    pub const fn for_agent(mut self, agent: &'a AgentId) -> Self {
        self.agent = Some(agent);
        self
    }
}

/// One definition that won its logical id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedItem {
    /// Identity shared across scopes.
    pub logical_id: LogicalId,
    /// Scope the winning definition came from.
    pub scope: Scope,
    /// The winning definition, original spelling preserved.
    pub definition: Definition,
    /// The lock that pins this item to its definition at [`LockMark::locked_by_scope`].
    ///
    /// `None` when no lock names the winning definition's item. When present the
    /// winner is the locked definition even if a nearer scope also defined the id.
    pub lock: Option<LockMark>,
}

impl ResolvedItem {
    /// Whether this item is pinned by a lock.
    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// The recorded lock reason, when the item is locked.
    #[must_use]
    pub fn lock_reason(&self) -> Option<&str> {
        self.lock.as_ref().map(|lock| lock.reason.as_str())
    }
}

/// A definition that lost its logical id to another definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed {
    /// Identity the losing and winning definitions share.
    pub logical_id: LogicalId,
    /// Scope the losing definition came from.
    pub scope: Scope,
    /// The losing definition, including the path it was found at.
    pub definition: Definition,
    /// The definition that won.
    pub shadowed_by: ShadowedBy,
    /// The lock that caused this definition to lose, when one did.
    ///
    /// `Some` is the story's `Blocked{lock_id}`: the winner is pinned to its Global
    /// definition and this scope may not override it. `None` means the definition
    /// merely lost on precedence.
    pub blocked: Option<LockMark>,
}

impl Shadowed {
    /// Whether a lock, rather than precedence, made this definition lose.
    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        self.blocked.is_some()
    }

    /// The lock that blocked this definition, when one did.
    #[must_use]
    pub fn blocking_lock(&self) -> Option<&LockMark> {
        self.blocked.as_ref()
    }
}

/// Identifies the definition that outranked a [`Shadowed`] one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowedBy {
    /// Scope of the winning definition.
    pub scope: Scope,
    /// AgenTuna item id of the winner, when it is managed.
    pub item_id: Option<ItemId>,
    /// Path of the winning definition.
    pub path: PathBuf,
}

/// An item that was disabled by a tombstone in one scope (TA-003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledHere {
    /// Identity shared across scopes.
    pub logical_id: LogicalId,
    /// Item id of the disabled item.
    pub item_id: ItemId,
    /// Scope the tombstone was recorded in.
    pub scope: Scope,
    /// Which agents the tombstone applies to (`All` or `Only([..])`).
    pub agents: DisabledAgents,
    /// The definition that is disabled.
    pub definition: Definition,
}

/// Everything one resolution pass produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedSet {
    /// One entry per logical id, ordered by kind, then normalized name, then scope depth.
    pub items: Vec<ResolvedItem>,
    /// Every definition that lost, ordered by kind, normalized name, logical key,
    /// losing scope depth, then path.
    pub shadowed: Vec<Shadowed>,
    /// Items disabled by a tombstone (TA-003).
    pub disabled: Vec<DisabledHere>,
    /// Problems that did not stop resolution, such as an unreadable lock store.
    pub diagnostics: Vec<ResolveDiagnostic>,
}

/// Something resolution could not do, reported instead of failing or guessing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveDiagnostic {
    /// The lock source could not be read, so this pass enforced no locks.
    ///
    /// Callers that must not resolve without lock enforcement treat
    /// [`ResolvedSet::lock_read_failed`] as a failure; callers that can show a
    /// warning use it to explain why a locked item appears unlocked.
    LockReadFailed {
        /// Store error text, already free of secret values.
        message: String,
    },
    /// The tombstone source could not be read for a scope (TA-003).
    TombstoneReadFailed {
        /// Scope whose tombstones failed to load.
        scope: Scope,
        /// Store error text, already free of secret values.
        message: String,
    },
}

impl ResolvedSet {
    /// Whether nothing was defined in any layer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // Shadowed records only exist for a logical id that also has a winner.
        self.items.is_empty()
    }

    /// The winner of `logical_id`, if any layer defined it.
    #[must_use]
    pub fn get(&self, logical_id: &LogicalId) -> Option<&ResolvedItem> {
        self.items
            .iter()
            .find(|item| &item.logical_id == logical_id)
    }

    /// Whether the lock store could not be read during this pass.
    #[must_use]
    pub fn lock_read_failed(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, ResolveDiagnostic::LockReadFailed { .. }))
    }

    /// Whether the tombstone source could not be read during this pass (TA-003).
    #[must_use]
    pub fn tombstone_read_failed(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, ResolveDiagnostic::TombstoneReadFailed { .. }))
    }
}

/// A definition that is a candidate for one logical id.
struct Candidate<'a> {
    layer: usize,
    index: usize,
    scope: Scope,
    definition: &'a Definition,
}

/// Merges scope layers into one resolved set. Reads only `input`.
///
/// The definition with the highest [`Scope::precedence`] wins its logical id. Ties at
/// equal precedence go to the earliest layer, then to the earliest definition in that
/// layer, so the result is deterministic even when the caller passes layers out of
/// order or repeats a scope. Every definition that did not win is returned in
/// [`ResolvedSet::shadowed`].
#[must_use]
pub fn resolve(input: ResolveInput<'_>) -> ResolvedSet {
    let mut candidates: Vec<Candidate<'_>> = input
        .layers
        .iter()
        .enumerate()
        .flat_map(|(layer, snapshot)| {
            snapshot
                .definitions
                .iter()
                .enumerate()
                .map(move |(index, definition)| Candidate {
                    layer,
                    index,
                    scope: snapshot.scope,
                    definition,
                })
        })
        .collect();
    candidates.sort_by_key(|candidate| {
        (
            candidate.scope.precedence(),
            candidate.layer,
            candidate.index,
        )
    });

    // Winner position per logical id, keyed by identity so the order below is stable.
    let mut winners: BTreeMap<LogicalId, usize> = BTreeMap::new();
    for (position, candidate) in candidates.iter().enumerate() {
        let logical_id = candidate.definition.logical_id();
        match winners.entry(logical_id) {
            Entry::Vacant(slot) => {
                slot.insert(position);
            }
            Entry::Occupied(mut slot) => {
                let current = &candidates[*slot.get()];
                if candidate.scope.precedence() > current.scope.precedence() {
                    slot.insert(position);
                }
            }
        }
    }

    // Locks are read on every call, never cached: a lock created by the CLI must be
    // visible to an already open GUI on its next resolve (TA-002).
    let mut diagnostics: Vec<ResolveDiagnostic> = Vec::new();
    let mut pinned: BTreeMap<LogicalId, usize> = BTreeMap::new();
    let mut marks: BTreeMap<LogicalId, LockMark> = BTreeMap::new();
    if let Some(store) = input.locks {
        match store.list() {
            Ok(records) => {
                let by_item: BTreeMap<ItemId, LockRecord> = records
                    .into_iter()
                    .map(|record| (record.lock.item_id, record))
                    .collect();
                for (position, candidate) in candidates.iter().enumerate() {
                    let Some(item_id) = candidate.definition.item_id else {
                        continue;
                    };
                    let Some(record) = by_item.get(&item_id) else {
                        continue;
                    };
                    // A lock pins the definition at its own scope; a copy of the same
                    // item in another scope cannot satisfy it.
                    if candidate.scope != record.lock.scope {
                        continue;
                    }
                    let logical_id = candidate.definition.logical_id();
                    if let Entry::Vacant(slot) = pinned.entry(logical_id.clone()) {
                        slot.insert(position);
                        marks.insert(logical_id, LockMark::from(record));
                    }
                }
            }
            Err(error) => diagnostics.push(ResolveDiagnostic::LockReadFailed {
                message: error.to_string(),
            }),
        }
    }
    // A locked definition wins its logical id whatever the nearer scopes say.
    for (logical_id, position) in &pinned {
        winners.insert(logical_id.clone(), *position);
    }

    // Tombstones are applied after precedence: the item is removed from the effective
    // set for that scope and all deeper scopes, optionally only for the listed agents (TA-003).
    // A Global lock pin dominates a tombstone — a locked item cannot be disabled.
    let mut disabled: Vec<DisabledHere> = Vec::new();
    let mut disabled_ids: BTreeSet<LogicalId> = BTreeSet::new();

    if let Some(source) = input.tombstones {
        let mut participating_scopes: Vec<Scope> = input
            .layers
            .iter()
            .map(|layer| layer.scope)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        participating_scopes.sort_by_key(|s| s.precedence());

        for scope in participating_scopes {
            match source.disabled(scope) {
                Ok(entries) => {
                    for entry in entries {
                        let Some((logical_id, &position)) = winners.iter().find(|(_, pos)| {
                            candidates[**pos].definition.item_id == Some(entry.item_id)
                        }) else {
                            continue;
                        };

                        let candidate = &candidates[position];

                        // A Global lock dominates a tombstone.
                        if marks.contains_key(logical_id) {
                            continue;
                        }

                        // The tombstone applies to that scope and all deeper scopes.
                        if scope.precedence() < candidate.scope.precedence() {
                            continue;
                        }

                        let applies_to_agent = match (&entry.agents, input.agent) {
                            (DisabledAgents::All, _) => true,
                            (DisabledAgents::Only(list), Some(agent)) => list.contains(agent),
                            (DisabledAgents::Only(_), None) => false,
                        };

                        disabled.push(DisabledHere {
                            logical_id: logical_id.clone(),
                            item_id: entry.item_id,
                            scope,
                            agents: entry.agents.clone(),
                            definition: candidate.definition.clone(),
                        });

                        if applies_to_agent {
                            disabled_ids.insert(logical_id.clone());
                        }
                    }
                }
                Err(error) => diagnostics.push(ResolveDiagnostic::TombstoneReadFailed {
                    scope,
                    message: error.to_string(),
                }),
            }
        }
    }

    disabled.sort_by_cached_key(|entry| {
        (
            entry.logical_id.kind,
            entry.definition.normalized_name(),
            entry.scope.precedence(),
            entry.logical_id.key.clone(),
        )
    });
    disabled.dedup_by(|a, b| a.item_id == b.item_id && a.scope == b.scope);

    for id in &disabled_ids {
        winners.remove(id);
    }
    let mut items: Vec<ResolvedItem> = winners
        .iter()
        .map(|(logical_id, position)| {
            let candidate = &candidates[*position];
            ResolvedItem {
                logical_id: logical_id.clone(),
                scope: candidate.scope,
                definition: candidate.definition.clone(),
                lock: marks.get(logical_id).cloned(),
            }
        })
        .collect();

    let mut shadowed: Vec<Shadowed> = Vec::new();
    for (position, candidate) in candidates.iter().enumerate() {
        let logical_id = candidate.definition.logical_id();
        let Some(&winner_position) = winners.get(&logical_id) else {
            continue;
        };
        if winner_position == position {
            continue;
        }
        let winner = &candidates[winner_position];
        shadowed.push(Shadowed {
            blocked: marks.get(&logical_id).cloned(),
            logical_id,
            scope: candidate.scope,
            definition: candidate.definition.clone(),
            shadowed_by: ShadowedBy {
                scope: winner.scope,
                item_id: winner.definition.item_id,
                path: winner.definition.path.clone(),
            },
        });
    }

    items.sort_by_cached_key(|item| {
        (
            item.logical_id.kind,
            item.definition.normalized_name(),
            item.scope.precedence(),
            item.logical_id.key.clone(),
        )
    });
    shadowed.sort_by_cached_key(|entry| {
        (
            entry.logical_id.kind,
            entry.definition.normalized_name(),
            entry.logical_id.key.clone(),
            entry.scope.precedence(),
            entry.definition.path.clone(),
        )
    });

    ResolvedSet {
        items,
        shadowed,
        disabled,
        diagnostics,
    }
}
