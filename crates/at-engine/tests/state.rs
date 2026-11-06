//! Item state and sync-label derivation: one table row per state transition.

use std::path::PathBuf;

use at_engine::state::{DeploymentFacts, DerivedState, TargetOrigin, classify};
use at_model::{Deployment, ItemState, LinkMode, SyncLabel};

struct Row {
    case: &'static str,
    store: Option<&'static str>,
    target: Option<&'static str>,
    baseline: Option<&'static str>,
    origin: TargetOrigin,
    state: ItemState,
    label: Option<SyncLabel>,
}

impl Row {
    fn managed(
        case: &'static str,
        store: Option<&'static str>,
        target: Option<&'static str>,
        baseline: Option<&'static str>,
        state: ItemState,
        label: Option<SyncLabel>,
    ) -> Self {
        Self {
            case,
            store,
            target,
            baseline,
            origin: TargetOrigin::Managed,
            state,
            label,
        }
    }

    fn facts(&self) -> DeploymentFacts<'_> {
        DeploymentFacts {
            store_hash: self.store,
            target_hash: self.target,
            baseline_hash: self.baseline,
            origin: self.origin,
        }
    }
}

/// One row per transition. The order of the rows follows the precedence chain
/// `Conflict > Drifted > Native > Linked > Off`, so the table is the specification.
#[test]
fn state_precedence_table_covers_every_transition() {
    let rows = [
        // Conflict: both copies exist and disagree without a known baseline.
        Row::managed(
            "no baseline, copies differ",
            Some("a"),
            Some("b"),
            None,
            ItemState::Conflict,
            None,
        ),
        Row::managed(
            "baseline known, both sides moved apart",
            Some("b"),
            Some("c"),
            Some("a"),
            ItemState::Conflict,
            None,
        ),
        // Drifted: the deployed copy moved, the store did not.
        Row::managed(
            "target moved, store unchanged",
            Some("a"),
            Some("b"),
            Some("a"),
            ItemState::Drifted,
            Some(SyncLabel::ProjectNewer),
        ),
        // Native: a real entry AgenTuna did not create, whatever the hashes say.
        Row {
            case: "unmanaged target with a store copy of the same content",
            store: Some("a"),
            target: Some("a"),
            baseline: None,
            origin: TargetOrigin::Native,
            state: ItemState::Native,
            label: None,
        },
        Row {
            case: "unmanaged target with no store copy",
            store: None,
            target: Some("a"),
            baseline: None,
            origin: TargetOrigin::Native,
            state: ItemState::Native,
            label: None,
        },
        Row {
            case: "unmanaged target that differs from a leftover baseline",
            store: Some("a"),
            target: Some("b"),
            baseline: Some("a"),
            origin: TargetOrigin::Native,
            state: ItemState::Native,
            label: None,
        },
        // Linked: AgenTuna's copy and the store agree, or only the store moved.
        Row::managed(
            "identical copies",
            Some("a"),
            Some("a"),
            Some("a"),
            ItemState::Linked,
            Some(SyncLabel::InSync),
        ),
        Row::managed(
            "identical copies, no baseline yet",
            Some("a"),
            Some("a"),
            None,
            ItemState::Linked,
            Some(SyncLabel::InSync),
        ),
        Row::managed(
            "store moved, target still at baseline",
            Some("b"),
            Some("a"),
            Some("a"),
            ItemState::Linked,
            Some(SyncLabel::LibraryNewer),
        ),
        // Off: nothing is deployed at the target, or there is no store copy to deploy.
        Row::managed(
            "target missing, baseline recorded",
            Some("a"),
            None,
            Some("a"),
            ItemState::Off,
            Some(SyncLabel::ProjectOnly),
        ),
        Row::managed(
            "target missing, never deployed",
            Some("a"),
            None,
            None,
            ItemState::Off,
            None,
        ),
        Row::managed(
            "store copy gone, target present",
            None,
            Some("a"),
            Some("a"),
            ItemState::Off,
            None,
        ),
        Row {
            case: "nothing anywhere",
            store: None,
            target: None,
            baseline: None,
            origin: TargetOrigin::Native,
            state: ItemState::Off,
            label: None,
        },
    ];

    for row in &rows {
        let derived = classify(row.facts());
        assert_eq!(derived.state, row.state, "{}: state", row.case);
        assert_eq!(derived.sync_label, row.label, "{}: label", row.case);
    }

    // Every state the model defines is exercised by the table.
    let states: Vec<ItemState> = rows.iter().map(|row| row.state).collect();
    for state in ItemState::ALL {
        assert!(states.contains(state), "{state} has no table row");
    }
    let labels: Vec<Option<SyncLabel>> = rows.iter().map(|row| row.label).collect();
    for label in SyncLabel::ALL {
        assert!(labels.contains(&Some(*label)), "{label} has no table row");
    }
}

/// Acceptance row: store hash equals target hash and equals the baseline is
/// `Linked` + `InSync`; a target that moved while the store stayed is
/// `Drifted` + `ProjectNewer`.
#[test]
fn agreement_is_in_sync_and_target_movement_is_drift() {
    assert_eq!(
        classify(DeploymentFacts::managed(
            Some("abc"),
            Some("abc"),
            Some("abc")
        )),
        DerivedState {
            state: ItemState::Linked,
            sync_label: Some(SyncLabel::InSync),
        }
    );
    assert_eq!(
        classify(DeploymentFacts::managed(
            Some("abc"),
            Some("def"),
            Some("abc")
        )),
        DerivedState {
            state: ItemState::Drifted,
            sync_label: Some(SyncLabel::ProjectNewer),
        }
    );
}

/// An unmanaged folder that happens to have the same name and the same bytes as a
/// store skill is still `Native`: ownership, not content, decides, and a native
/// item has no baseline row.
#[test]
fn identical_content_does_not_make_an_unmanaged_folder_linked() {
    let derived = classify(DeploymentFacts {
        store_hash: Some("same"),
        target_hash: Some("same"),
        baseline_hash: None,
        origin: TargetOrigin::Native,
    });
    assert_eq!(derived.state, ItemState::Native);
    assert_eq!(derived.sync_label, None);

    let shadow = classify(DeploymentFacts::managed(Some("same"), Some("same"), None));
    assert_eq!(shadow.state, ItemState::Linked);
}

/// Missing target with a baseline is `Off` + `ProjectOnly`; without one it is
/// `Off` with no label to show.
#[test]
fn missing_target_is_off() {
    assert_eq!(
        classify(DeploymentFacts::managed(Some("abc"), None, Some("abc"))),
        DerivedState {
            state: ItemState::Off,
            sync_label: Some(SyncLabel::ProjectOnly),
        }
    );
    assert_eq!(
        classify(DeploymentFacts::managed(Some("abc"), None, None)),
        DerivedState {
            state: ItemState::Off,
            sync_label: None,
        }
    );
}

/// The sync-and-drift screen reads a stored `deployments` row, so the facts
/// constructor that takes one must classify it exactly like the manual form.
#[test]
fn stored_deployment_row_classifies_from_its_hashes() {
    let row = Deployment {
        state: ItemState::Linked,
        mode: LinkMode::Link,
        target_path: PathBuf::from("/agent/skills/review"),
        store_hash: Some("abc".to_owned()),
        target_hash: Some("def".to_owned()),
        baseline_hash: Some("abc".to_owned()),
        sync_label: Some(SyncLabel::InSync),
    };
    let derived = classify(DeploymentFacts::from_deployment(&row));
    assert_eq!(
        derived,
        DerivedState {
            state: ItemState::Drifted,
            sync_label: Some(SyncLabel::ProjectNewer),
        },
        "the stored label is never trusted over the hashes"
    );
}
