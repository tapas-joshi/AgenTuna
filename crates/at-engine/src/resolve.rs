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

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::PathBuf;

use at_model::{Definition, ItemId, LogicalId, Scope, ScopeSnapshot};

/// Input to [`resolve`].
///
/// Fields are private on purpose: later resolution inputs (Global locks, tombstones,
/// agent filters) arrive as additional constructor and builder methods, so every
/// caller written against [`ResolveInput::new`] keeps compiling.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveInput<'a> {
    layers: &'a [ScopeSnapshot],
}

impl<'a> ResolveInput<'a> {
    /// Resolution over the given scope layers, supplied in any order.
    #[must_use]
    pub const fn new(layers: &'a [ScopeSnapshot]) -> Self {
        Self { layers }
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

/// Everything one resolution pass produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedSet {
    /// One entry per logical id, ordered by kind, then normalized name, then scope depth.
    pub items: Vec<ResolvedItem>,
    /// Every definition that lost, ordered by kind, normalized name, logical key,
    /// losing scope depth, then path.
    pub shadowed: Vec<Shadowed>,
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

    let mut items: Vec<ResolvedItem> = winners
        .iter()
        .map(|(logical_id, position)| {
            let candidate = &candidates[*position];
            ResolvedItem {
                logical_id: logical_id.clone(),
                scope: candidate.scope,
                definition: candidate.definition.clone(),
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

    ResolvedSet { items, shadowed }
}
