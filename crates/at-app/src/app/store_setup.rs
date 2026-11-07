//! Presentation of a store initialisation folded into a reviewed write plan.
//!
//! Several screens plan a write into a scope whose store may not exist yet: the MCP form, the
//! Add Skill wizard and the MCP copy dialog. Each preview has to show the store creation as its
//! own line before the item files, so the user reviews it like any other write. The planner
//! lives in [`at_store::store_init`]; this module owns only the typed view every screen shares.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use at_store::store_init::PlannedStoreInit;

use super::commands::{CommandError, CommandErrorCode};

/// One "Create store at `<path>`" line in a reviewed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct StoreSetupView {
    /// Absolute store root the plan creates.
    pub path: String,
    /// Layout entries the plan creates below the root.
    pub files_created: u64,
    /// Foreign entries that are left byte-identical.
    pub foreign: Vec<String>,
}

impl StoreSetupView {
    /// Builds the view for one planned initialisation.
    #[must_use]
    pub fn from_plan(init: &PlannedStoreInit) -> Self {
        Self {
            path: init.root.display().to_string(),
            files_created: u64::try_from(init.created.len()).unwrap_or(u64::MAX),
            foreign: init
                .foreign
                .iter()
                .map(|entry| entry.display().to_string())
                .collect(),
        }
    }
}

/// Maps a store initialisation refusal onto the command error contract.
///
/// An unwritable Global root is a consent problem, not a plan problem: the remedy is the
/// existing "Enable global writes" action, and the plan writes nothing.
#[must_use]
pub fn store_init_error(
    layout: &at_store::layout::StoreLayout,
    error: at_store::StoreInitError,
) -> CommandError {
    match error {
        at_store::StoreInitError::GlobalConsentRequired { path, .. } => CommandError::new(
            CommandErrorCode::Locked,
            format!(
                "Enable Global writes before this plan can create {}. Open Overview, choose the Global scope and use Enable global writes.",
                path.display()
            ),
        ),
        other => CommandError::new(
            CommandErrorCode::Conflict,
            format!(
                "{}: the store setup for this plan cannot be reviewed: {other}",
                layout.root().display()
            ),
        ),
    }
}
