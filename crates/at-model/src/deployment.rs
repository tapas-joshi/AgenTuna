//! Per-agent deployment state of an item.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// Deployment state of an item for one agent.
    pub enum ItemState {
        /// Deployed by AgenTuna and matching the store.
        Linked => "linked",
        /// Known to AgenTuna but not deployed to the agent.
        Off => "off",
        /// Found only in the agent's own folder; not managed.
        Native => "native",
        /// Store and agent copies both exist and differ without a known baseline.
        Conflict => "conflict",
        /// Deployed copy changed outside AgenTuna since the baseline.
        Drifted => "drifted",
    }
}
wire_enum! {
    /// Projected state of a skill for one agent in the skills index.
    pub enum DeploymentState {
        /// A managed link or copy is present.
        Linked => "linked",
        /// The item is known but disabled for this agent.
        Off => "off",
        /// A deployment conflicts with another definition.
        Conflict => "conflict",
        /// A deployed copy has changed outside AgenTuna.
        Drifted => "drifted",
        /// A native, unmanaged item is present.
        Native => "native",
        /// A higher-precedence definition wins for this agent.
        Shadowed => "shadowed",
        /// The agent is installed but has no deployment.
        None => "none",
        /// The agent is not installed.
        NotInstalled => "not_installed",
    }
}

wire_enum! {
    #[non_exhaustive]
    /// How a project copy relates to the library copy.
    pub enum SyncLabel {
        /// Both copies are identical.
        InSync => "in_sync",
        /// Exists only in the project.
        ProjectOnly => "project_only",
        /// The library copy is newer.
        LibraryNewer => "library_newer",
        /// The project copy is newer.
        ProjectNewer => "project_newer",
        /// Exists only in the User library.
        LibraryOnly => "library_only",
        /// Both copies changed from their shared base.
        Conflict => "conflict",
        /// The Project copy changed outside AgenTuna.
        Drifted => "drifted",
    }
}

wire_enum! {
    /// How a deployment is materialised in the agent folder.
    pub enum LinkMode {
        /// A symbolic link to the canonical store.
        Link => "link",
        /// A recursive copy with a manifest.
        Copy => "copy",
    }
}

/// One item deployed to one agent at one scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    /// Current state.
    pub state: ItemState,
    /// Link or copy.
    pub mode: LinkMode,
    /// Path inside the agent's folder.
    pub target_path: PathBuf,
    /// Content hash of the canonical store copy.
    pub store_hash: Option<String>,
    /// Content hash currently found at `target_path`.
    pub target_hash: Option<String>,
    /// Hash recorded when AgenTuna last wrote or adopted the target.
    pub baseline_hash: Option<String>,
    /// Library-versus-project relation, when applicable.
    pub sync_label: Option<SyncLabel>,
}
