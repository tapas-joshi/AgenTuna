//! Item state and sync-label derivation for one (item, agent, scope) deployment.
//!
//! [`classify`] turns the three content hashes AgenTuna keeps — the store copy,
//! the deployed target and the baseline recorded when AgenTuna last wrote or
//! adopted the target — into an [`ItemState`] and, when it applies, one
//! [`SyncLabel`]. This module is the only place in AgenTuna that derives either
//! value (integration decision 12): the sync-and-drift screen reads the stored
//! `deployments` row and never recomputes the label itself.
//!
//! State precedence is `Conflict > Drifted > Native > Linked > Off`:
//!
//! | Situation | State | Label |
//! | --- | --- | --- |
//! | Store and target differ, no baseline, or both differ from it | `Conflict` | none |
//! | Target changed since the baseline, store unchanged | `Drifted` | `ProjectNewer` |
//! | Real target AgenTuna did not create | `Native` | none |
//! | Target present and matching the store, or only the store moved | `Linked` | `InSync` / `LibraryNewer` |
//! | Target missing, or the store has no copy | `Off` | `ProjectOnly` with a baseline |
//!
//! A `Native` item is by definition unmanaged, so it never carries a baseline and
//! never becomes `Linked`, whatever its content hash happens to be.

use at_model::{Deployment, ItemState, SyncLabel};

/// Where the thing at the deployment target came from.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOrigin {
    /// AgenTuna created the target, as a link or a copy, or adopted it.
    Managed,
    /// A real folder or entry exists that AgenTuna did not create.
    Native,
}

/// Everything needed to classify one deployment.
///
/// A hash is the content hash of that side, or `None` when nothing is there:
/// `target_hash` is `None` when the target path has nothing at it, and
/// `store_hash` is `None` when the store holds no copy of the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentFacts<'a> {
    /// Content hash of the canonical store copy.
    pub store_hash: Option<&'a str>,
    /// Content hash currently found at the deployment target.
    pub target_hash: Option<&'a str>,
    /// Hash recorded when AgenTuna last wrote or adopted the target.
    pub baseline_hash: Option<&'a str>,
    /// Whether AgenTuna created the target.
    pub origin: TargetOrigin,
}

impl<'a> DeploymentFacts<'a> {
    /// A deployment AgenTuna created.
    #[must_use]
    pub const fn managed(
        store_hash: Option<&'a str>,
        target_hash: Option<&'a str>,
        baseline_hash: Option<&'a str>,
    ) -> Self {
        Self {
            store_hash,
            target_hash,
            baseline_hash,
            origin: TargetOrigin::Managed,
        }
    }

    /// A real folder or entry that AgenTuna did not create. It has no baseline.
    #[must_use]
    pub const fn native(target_hash: Option<&'a str>) -> Self {
        Self {
            store_hash: None,
            target_hash,
            baseline_hash: None,
            origin: TargetOrigin::Native,
        }
    }

    /// Reads the hashes off a stored deployment row. Such a row is always managed.
    #[must_use]
    pub fn from_deployment(deployment: &'a Deployment) -> Self {
        Self::managed(
            deployment.store_hash.as_deref(),
            deployment.target_hash.as_deref(),
            deployment.baseline_hash.as_deref(),
        )
    }
}

/// The state and label one deployment has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedState {
    /// Current deployment state.
    pub state: ItemState,
    /// Library-versus-project relation, when one is meaningful.
    pub sync_label: Option<SyncLabel>,
}

/// Classifies one deployment.
///
/// The single derivation of [`ItemState`] and [`SyncLabel`] in AgenTuna:
///
/// ```
/// use at_engine::state::{DeploymentFacts, classify};
/// use at_model::{ItemState, SyncLabel};
///
/// let derived = classify(DeploymentFacts::managed(Some("abc"), Some("abc"), Some("abc")));
/// assert_eq!(derived.state, ItemState::Linked);
/// assert_eq!(derived.sync_label, Some(SyncLabel::InSync));
///
/// let drifted = classify(DeploymentFacts::managed(Some("abc"), Some("def"), Some("abc")));
/// assert_eq!(drifted.state, ItemState::Drifted);
/// assert_eq!(drifted.sync_label, Some(SyncLabel::ProjectNewer));
/// ```
///
/// Callers that already store the result must not recompute it (decision 12);
/// `TC-015` reads `deployments.state` and `deployments.sync_label` as written.
#[must_use]
pub fn classify(facts: DeploymentFacts<'_>) -> DerivedState {
    let conflict = is_conflict(facts);
    let drifted = !conflict && is_drifted(facts);
    let native = facts.origin == TargetOrigin::Native && facts.target_hash.is_some();
    // Linked means AgenTuna deployed it and the copies agree: both sides must exist.
    let linked = facts.origin == TargetOrigin::Managed
        && facts.store_hash.is_some()
        && facts.target_hash.is_some()
        && !conflict
        && !drifted;

    let state = if conflict {
        ItemState::Conflict
    } else if drifted {
        ItemState::Drifted
    } else if native {
        ItemState::Native
    } else if linked {
        ItemState::Linked
    } else {
        ItemState::Off
    };

    DerivedState {
        state,
        sync_label: derive_label(facts),
    }
}

/// Store and target both exist and disagree without a shared known baseline.
fn is_conflict(facts: DeploymentFacts<'_>) -> bool {
    if facts.origin != TargetOrigin::Managed {
        return false;
    }
    let (Some(store), Some(target)) = (facts.store_hash, facts.target_hash) else {
        return false;
    };
    if store == target {
        return false;
    }
    match facts.baseline_hash {
        None => true,
        Some(baseline) => store != baseline && target != baseline,
    }
}

/// The target moved away from the baseline while the store stayed where it was.
fn is_drifted(facts: DeploymentFacts<'_>) -> bool {
    if facts.origin != TargetOrigin::Managed {
        // A native item has no baseline of ours to drift away from.
        return false;
    }
    matches!(
        (facts.store_hash, facts.target_hash, facts.baseline_hash),
        (Some(store), Some(target), Some(baseline)) if store == baseline && target != baseline
    )
}

/// The only [`SyncLabel`] derivation. `None` means the two copies cannot be
/// compared with confidence: a conflict, a native item, or a store-less deployment.
fn derive_label(facts: DeploymentFacts<'_>) -> Option<SyncLabel> {
    if facts.origin != TargetOrigin::Managed {
        return None;
    }
    match (facts.store_hash, facts.target_hash) {
        (Some(store), Some(target)) if store == target => Some(SyncLabel::InSync),
        (Some(store), Some(target)) => match facts.baseline_hash {
            Some(baseline) if baseline == store => Some(SyncLabel::ProjectNewer),
            Some(baseline) if baseline == target => Some(SyncLabel::LibraryNewer),
            _ => None,
        },
        (Some(_), None) => facts
            .baseline_hash
            .is_some()
            .then_some(SyncLabel::ProjectOnly),
        _ => None,
    }
}
