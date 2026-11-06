//! Skills list projection for the Skills screen (TS-028).
//!
//! The screen reads this one typed command and renders chips, search and the
//! virtualized table from the same row set, so counts can never disagree with
//! the table. Scope-chip filtering, text search and native/shadowed state
//! happen in the webview; the query loads every canonical row plus
//! native-only discoveries for the active project context.

use std::str::FromStr;

use at_model::{DeploymentState, Scope};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::{
    commands::CommandError,
    state::AppState,
};

/// One agent column with the presence the screen needs to grey cells out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentColumnView {
    /// Stable agent identity.
    pub id: String,
    /// Display name for the column header.
    pub label: String,
    /// `installed`, `configured_only` or `not_installed`.
    pub presence: String,
}

/// One per-agent deployment cell of a skill row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillAgentCellView {
    /// Agent identity.
    pub agent: String,
    /// Deployment-state wire name (`linked`, `off`, `native`, ...).
    pub state: String,
    /// Whether the skill currently loads for this agent.
    pub enabled: bool,
    /// Deployment target when one exists, or the native folder for discoveries.
    pub config_path: Option<String>,
}

/// One skill row of the list projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillRowView {
    /// Canonical item id; empty for a native-only discovery.
    pub item_id: String,
    /// Display name.
    pub name: String,
    /// Scope of the definition or discovery.
    pub scope: String,
    /// Stored item state, or `native` for a native-only discovery.
    pub state: Option<String>,
    /// Scope of the higher-precedence copy, when this row is shadowed.
    pub shadowed_by: Option<String>,
    /// One-line description parsed from SKILL.md at index time.
    pub description: Option<String>,
    /// Whether the row exists only in an agent's own folder.
    pub native: bool,
    /// Source path of a native-only discovery.
    pub native_path: Option<String>,
    /// Whether the source advertises an update (TS-040 renders the badge).
    pub update_available: bool,
    /// Last update timestamp of the canonical row or native scan.
    pub updated_at: String,
    /// One cell per detected agent.
    pub agents: Vec<SkillAgentCellView>,
}

/// The skills list projection behind the Skills screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillsListView {
    /// Every canonical row plus native-only discoveries.
    pub rows: Vec<SkillRowView>,
    /// Agent columns in display order.
    pub agents: Vec<SkillAgentColumnView>,
    /// Active project identity, when one is selected.
    pub project_id: Option<String>,
    /// Active project display name.
    pub project_name: Option<String>,
}

/// Input to the skills list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillsListRequest {
    /// Optional scope filter; `None` loads every scope so the chips can count.
    pub scope: Option<String>,
    /// Registered project identity for project and local scopes.
    pub project_id: Option<String>,
}

fn parse_scope_arg(value: &Option<String>) -> Result<Option<Scope>, CommandError> {
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some(text) => Scope::from_str(text)
            .map(Some)
            .map_err(|_| CommandError::invalid_argument("scope", "expected a supported scope")),
    }
}

/// Whether a deployment state loads for the agent, and its wire name.
fn deployment_cell(state: DeploymentState) -> (&'static str, bool) {
    match state {
        DeploymentState::Linked | DeploymentState::Native => ("linked", true),
        DeploymentState::Off => ("off", false),
        DeploymentState::Conflict | DeploymentState::Drifted => ("conflict", true),
        DeploymentState::Shadowed => ("shadowed", false),
        DeploymentState::None => ("none", false),
        DeploymentState::NotInstalled => ("not_installed", false),
    }
}

/// Reads the skills list projection for the Skills screen.
///
/// # Errors
/// Returns a typed error when the database is unavailable or a stored
/// identifier fails to parse.
#[tauri::command]
pub fn skills_list(
    state: State<'_, AppState>,
    request: SkillsListRequest,
) -> Result<SkillsListView, CommandError> {
    let scope = parse_scope_arg(&request.scope)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let project = super::mcp_servers::project_of(&database, request.project_id.as_deref())?;
    let filter = at_db::skills_index::SkillFilter {
        scope,
        include_native: true,
        ..at_db::skills_index::SkillFilter::default()
    };
    let rows = at_db::skills_index::list_skills(database.connection(), &filter)
        .map_err(|error| CommandError::internal(format!("cannot read skills: {error}")))?;
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|error| CommandError::internal(format!("cannot read agents: {error}")))?;
    let columns = agents
        .iter()
        .map(|agent| SkillAgentColumnView {
            id: agent.id.to_string(),
            label: agent
                .display_name
                .clone()
                .unwrap_or_else(|| agent.id.to_string()),
            presence: match agent.presence {
                at_db::agents::AgentPresence::Installed => String::from("installed"),
                at_db::agents::AgentPresence::ConfiguredOnly => String::from("configured_only"),
                _ => String::from("not_installed"),
            },
        })
        .collect::<Vec<_>>();
    let mut view_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let native = row.item_id.is_none();
        let mut cells = Vec::with_capacity(row.deployed.len());
        let deployments = match &row.item_id {
            Some(item_id) => {
                let deployments = at_db::deployments::list_deployments_for_item(
                    database.connection(),
                    item_id,
                )
                .map_err(|error| {
                    CommandError::internal(format!("cannot read deployments: {error}"))
                })?;
                Some(deployments)
            }
            None => None,
        };
        for (agent_id, deployment_state) in &row.deployed {
            let config_path = match &deployments {
                Some(deployments) => deployments
                    .iter()
                    .find(|cell| &cell.agent_id == agent_id && cell.scope == row.scope)
                    .and_then(|cell| cell.target_path.clone()),
                None => row.native_path.clone(),
            };
            let (state, enabled) = deployment_cell(*deployment_state);
            cells.push(SkillAgentCellView {
                agent: agent_id.to_string(),
                state: state.to_owned(),
                enabled,
                config_path,
            });
        }
        cells.sort_by(|left, right| left.agent.cmp(&right.agent));
        view_rows.push(SkillRowView {
            item_id: row
                .item_id
                .as_ref()
                .map_or_else(String::new, ToString::to_string),
            name: row.name,
            scope: row.scope.wire_name().to_owned(),
            state: row.state,
            shadowed_by: row.shadowed_by.map(|scope| scope.wire_name().to_owned()),
            description: row.description,
            native,
            native_path: row.native_path,
            update_available: row.update_available,
            updated_at: row.updated_at,
            agents: cells,
        });
    }
    Ok(SkillsListView {
        rows: view_rows,
        agents: columns,
        project_id: request.project_id,
        project_name: project.map(|project| project.name),
    })
}
