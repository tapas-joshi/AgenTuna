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

/// One file touched by a recent operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverviewChangeFileView {
    /// Path recorded in the operation journal.
    pub path: String,
    /// Scope recorded for the operation.
    pub scope: Option<String>,
}
/// One recent operation and its touched-file count and paths.
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
    /// Touched files loaded with the Overview query.
    pub files: Vec<OverviewChangeFileView>,
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
    /// Resolved user store directory for copy and path display.
    pub user_path: String,
    /// Resolved machine-wide Global store directory.
    pub global_path: String,
    /// Whether this process can write to the Global store.
    pub global_writable: bool,
    /// Why Global writes are unavailable, when applicable.
    pub global_reason: Option<String>,
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
    let mut attention = payload
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
        .collect::<Vec<_>>();
    let locks = at_db::locks::locks_for_scope(database.connection(), at_model::Scope::Global)
        .map_err(|_| CommandError::internal("could not query Global locks"))?;
    let lock_ids = locks.iter().map(|lock| lock.item_id).collect::<Vec<_>>();
    let indexed_ids = at_db::shared::items_by_ids(database.connection(), &lock_ids)
        .map_err(|_| CommandError::internal("could not query locked items"))?
        .into_iter()
        .map(|item| item.id)
        .collect::<std::collections::HashSet<_>>();
    for lock in locks {
        if !indexed_ids.contains(&lock.item_id) {
            let item_id = lock.item_id.to_string();
            attention.push(OverviewAttentionView {
                item_id: Some(item_id.clone()),
                title: "Locked item not found".to_owned(),
                subtitle: lock.reason,
                kind: "locked_item_not_found".to_owned(),
                screen: Some("effective".to_owned()),
                row_id: Some(item_id),
            });
        }
    }
    let changes = payload
        .recent_changes
        .into_iter()
        .map(|row| {
            let detail = at_db::ops::operation_detail(database.connection(), &row.operation.id)
                .map_err(|_| CommandError::internal("could not query recent change files"))?;
            let scope = row.operation.scope.map(|scope| scope.to_string());
            let files = detail
                .into_iter()
                .flat_map(|detail| detail.files)
                .filter_map(|file| file.path)
                .map(|path| OverviewChangeFileView {
                    path,
                    scope: scope.clone(),
                })
                .collect();
            Ok(OverviewChangeView {
                id: row.operation.id,
                kind: row
                    .operation
                    .kind
                    .map(|kind| format!("{kind:?}").to_lowercase())
                    .unwrap_or_else(|| "change".to_owned()),
                summary: row.operation.summary,
                created_at: row.operation.created_at,
                file_count: row.file_count,
                scope,
                files,
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load path settings"))?;
    let mut paths = super::paths::resolve_process_paths(None)
        .map_err(|_| CommandError::internal("could not resolve application paths"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    let global_status = at_fs::privilege::GlobalStoreStatus::probe(paths.global_store());
    Ok(OverviewView {
        scopes,
        attention,
        attention_total: payload.needs_attention_total,
        changes,
        user_path: paths.user_store().display().to_string(),
        global_path: paths.global_store().display().to_string(),
        global_writable: global_status.is_available(),
        global_reason: global_status.reason().map(str::to_owned),
    })
}

/// Explicitly prepares the Global store after the user requests write access.
///
/// # Errors
/// Returns a typed unavailable error if authorization is denied or the store remains read-only.
#[tauri::command]
pub fn overview_enable_global_writes(state: State<'_, AppState>) -> Result<(), CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load path settings"))?;
    let mut paths = super::paths::resolve_process_paths(None)
        .map_err(|_| CommandError::internal("could not resolve application paths"))?;
    paths.apply_settings_global_store(settings.global_store_path());
    at_fs::privilege::enable_global_writes(
        paths.global_store(),
        at_fs::privilege::StoreOwner::current(),
        at_fs::privilege::platform_elevator().as_ref(),
    )
    .map_err(|_| CommandError::unavailable("Global store authorization failed"))?;
    if !at_fs::privilege::GlobalStoreStatus::probe(paths.global_store()).is_available() {
        return Err(CommandError::unavailable(
            "Global store is still not writable by this user",
        ));
    }
    Ok(())
}
