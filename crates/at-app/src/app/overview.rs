//! Typed read commands for the Overview screen.

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::CommandError;
use super::state::AppState;

/// One scope card in the Overview screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewScopeView {
    /// Scope represented by this card.
    pub scope: String,
    /// Skill count.
    pub skills: i64,
    /// MCP server count.
    pub mcp: i64,
    /// Instruction/rule count.
    pub rules: i64,
    /// Lock count, including orphaned locks.
    pub locks: i64,
    /// Timestamp of the most recent change, if any.
    pub last_edited: Option<String>,
    /// Number of disabled items.
    pub disabled: u64,
}

/// Needs-attention row and its safe route target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewAttentionView {
    /// Stable item identity when the issue concerns an item.
    pub item_id: Option<String>,
    /// One-line row title.
    pub title: String,
    /// Supporting detail.
    pub subtitle: Option<String>,
    /// Problem class in snake case.
    pub kind: String,
    /// Screen to navigate to.
    pub screen: Option<String>,
    /// Exact row filter, when available.
    pub row_id: Option<String>,
}

/// One recent operation and its touched-file count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewChangeView {
    /// Operation identifier.
    pub id: String,
    /// Operation kind.
    pub kind: String,
    /// Summary shown in the timeline.
    pub summary: Option<String>,
    /// When the operation occurred.
    pub created_at: String,
    /// Touched-file count.
    pub file_count: i64,
    /// Scope where it occurred.
    pub scope: Option<String>,
}

/// Complete database-backed Overview query result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewView {
    /// Scope cards including empty scopes.
    pub scopes: Vec<OverviewScopeView>,
    /// Needs-attention rows.
    pub attention: Vec<OverviewAttentionView>,
    /// Total attention count before display limit.
    pub attention_total: usize,
    /// Recent operations, newest first.
    pub changes: Vec<OverviewChangeView>,
}

/// Loads Overview projections through the engine and shared database.
#[tauri::command]
pub fn overview_load(
    state: State<'_, AppState>,
    active_scope: String,
) -> Result<OverviewView, CommandError> {
    let scope = active_scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("active_scope", "unknown scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let payload = at_engine::overview::build(database.connection(), scope)
        .map_err(|_| CommandError::internal("could not load Overview data"))?;
    let scopes = payload
        .scopes
        .into_iter()
        .map(|card| OverviewScopeView {
            scope: card.scope.to_string(),
            skills: card.counts.skills,
            mcp: card.counts.mcp,
            rules: card.counts.rules,
            locks: card.counts.locks,
            last_edited: card.last_edited,
            disabled: card.disabled_count,
        })
        .collect();
    let attention = payload
        .needs_attention
        .into_iter()
        .map(|row| OverviewAttentionView {
            item_id: row.item_id.map(|id| id.to_string()),
            title: row.title,
            subtitle: row.subtitle,
            kind: format!("{:?}", row.kind).to_lowercase(),
            screen: row.navigation.as_ref().map(|target| target.screen.clone()),
            row_id: row.navigation.and_then(|target| target.row_id),
        })
        .collect();
    let changes = payload
        .recent_changes
        .into_iter()
        .map(|row| OverviewChangeView {
            id: row.operation.id,
            kind: row
                .operation
                .kind
                .map(|kind| format!("{:?}", kind).to_lowercase())
                .unwrap_or_else(|| "change".to_owned()),
            summary: row.operation.summary,
            created_at: row.operation.created_at,
            file_count: row.file_count,
            scope: row.operation.scope.map(|scope| scope.to_string()),
        })
        .collect();
    Ok(OverviewView {
        scopes,
        attention,
        attention_total: payload.needs_attention_total,
        changes,
    })
}
