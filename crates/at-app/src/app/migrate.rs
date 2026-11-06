//! Typed native migration scan command.
use super::{commands::CommandError, paths::resolve_process_paths, state::AppState};
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport, ScanRequest};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use tauri::State;
use ts_rs::TS;

/// One safely discovered native definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateScanItemView {
    /// Discovered definition kind.
    pub kind: String,
    /// Native item name.
    pub name: String,
    /// Agents which can observe this physical entry.
    pub agents: Vec<String>,
    /// Absolute native path of the entry.
    pub path: String,
    /// Scope inferred by native discovery.
    pub scope: String,
    /// Content digest when readable.
    pub content_hash: Option<String>,
    /// Parsing or read error for this item.
    pub parse_error: Option<String>,
    /// Whether the native entry points into an AgenTuna store.
    pub store_link: bool,
    /// Managed, conflict, not-in-agentuna or unknown ownership state.
    pub ownership: String,
}

/// Complete migration scan result, including partial-scan status and skipped roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateScanView {
    /// Discovered definitions and per-item errors.
    pub items: Vec<MigrateScanItemView>,
    /// Whether one or more discovery roots failed.
    pub partial: bool,
    /// Missing candidate roots skipped during discovery.
    pub skipped_roots: Vec<String>,
}

fn item_view(item: ScanItem) -> MigrateScanItemView {
    MigrateScanItemView {
        kind: item.kind.to_string(),
        name: item.name,
        agents: item
            .agents
            .into_iter()
            .map(|agent| agent.to_string())
            .collect(),
        path: item.path.display().to_string(),
        scope: item.scope.to_string(),
        content_hash: item.content_hash,
        parse_error: match item.parse_status {
            ParseStatus::Parsed => None,
            ParseStatus::Error(message) => Some(message),
            _ => Some("unrecognized scan status".to_owned()),
        },
        store_link: item.store_link,
        ownership: match item.ownership {
            Ownership::Managed => "managed",
            Ownership::NotInAgenTuna => "not_in_agentuna",
            Ownership::Conflict => "conflict",
            _ => "unknown",
        }
        .to_owned(),
    }
}

fn store_inventory(
    paths: &at_store::paths::AppPaths,
) -> Result<(Vec<PathBuf>, Vec<at_migrate::scan::StoreItem>), CommandError> {
    let mut roots = Vec::new();
    let mut items = Vec::new();
    for scope in [
        at_model::Scope::Global,
        at_model::Scope::User,
        at_model::Scope::Project,
        at_model::Scope::Local,
    ] {
        let layout = match at_store::layout::StoreLayout::for_scope(paths, scope) {
            Ok(layout) => layout,
            Err(_) if matches!(scope, at_model::Scope::Project | at_model::Scope::Local) => {
                continue;
            }
            Err(_) => return Err(CommandError::unavailable("store paths are unavailable")),
        };
        roots.push(layout.root().to_path_buf());
        match at_store::manifest::StoreManifest::load(&layout) {
            Ok(loaded) => items.extend(loaded.manifest.items().iter().map(|item| {
                at_migrate::scan::StoreItem {
                    kind: item.kind,
                    name: item.name.clone().unwrap_or_else(|| item.slug.to_string()),
                    content_hash: item.hash.to_string(),
                }
            })),
            Err(error) if error.is_missing() => {}
            Err(_) => {
                return Err(CommandError::internal(
                    "could not read a canonical store manifest",
                ));
            }
        }
    }
    Ok((roots, items))
}

/// Scans agent folders in a blocking worker; scanning never executes MCP servers.
#[tauri::command]
pub async fn migrate_scan(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<MigrateScanView, CommandError> {
    let project = if let Some(id) = project_id {
        let database = state.database().map_err(CommandError::unavailable)?;
        Some(PathBuf::from(
            at_db::projects::find_project_by_id(database.connection(), &id)
                .map_err(|_| CommandError::internal("could not query registered projects"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?
                .path,
        ))
    } else {
        None
    };
    let paths = resolve_process_paths(project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let (roots, store_items) = store_inventory(&paths)?;
    let report: ScanReport = tokio::task::spawn_blocking(move || {
        let env = AgentEnvironment {
            home,
            project,
            variables: BTreeMap::new(),
        };
        let adapters = AgentAdapterRegistry::register();
        at_migrate::scan::scan(ScanRequest {
            environment: &env,
            adapters: &adapters,
            store_roots: &roots,
            store_items: &store_items,
        })
        .map_err(|_| ())
    })
    .await
    .map_err(|_| CommandError::internal("native scan worker stopped unexpectedly"))?
    .map_err(|_| CommandError::internal("native scan failed"))?;
    Ok(MigrateScanView {
        items: report.items.into_iter().map(item_view).collect(),
        partial: report.partial,
        skipped_roots: report
            .skipped_roots
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}
/// One comparable native copy in a migration conflict.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateConflictCopyView {
    /// Agent identifier.
    pub agent: String,
    /// Native path.
    pub path: String,
    /// Discovered scope.
    pub scope: String,
    /// Raw content digest.
    pub raw_hash: String,
    /// Normalized content digest.
    pub normalized_hash: String,
}

/// One group of same-name native copies that needs review.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateConflictView {
    /// Item kind.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Copies ordered by path.
    pub copies: Vec<MigrateConflictCopyView>,
    /// Whether only formatting differs.
    pub formatting_only: bool,
    /// Safe textual diff; omitted for MCP credentials.
    pub unified_diff: String,
    /// Default choice when applicable.
    pub proposed_resolution: String,
}

/// Conflict groups for one fresh native scan.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateConflictsView {
    /// Comparable conflict groups.
    pub conflicts: Vec<MigrateConflictView>,
}

/// Scans the selected project and returns conflicts without running any native server.
#[tauri::command]
pub async fn migrate_conflicts(
    state: State<'_, AppState>,
    project_id: Option<String>,
    selected_paths: Vec<String>,
) -> Result<MigrateConflictsView, CommandError> {
    let project = if let Some(id) = project_id {
        let database = state.database().map_err(CommandError::unavailable)?;
        Some(PathBuf::from(
            at_db::projects::find_project_by_id(database.connection(), &id)
                .map_err(|_| CommandError::internal("could not query registered projects"))?
                .ok_or_else(|| {
                    CommandError::invalid_argument("project_id", "project is not registered")
                })?
                .path,
        ))
    } else {
        None
    };
    let paths = resolve_process_paths(project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let (roots, store_items) = store_inventory(&paths)?;
    let scan = tokio::task::spawn_blocking(move || {
        let environment = AgentEnvironment {
            home,
            project,
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        at_migrate::scan::scan(ScanRequest {
            environment: &environment,
            adapters: &registry,
            store_roots: &roots,
            store_items: &store_items,
        })
        .map_err(|_| ())
    })
    .await
    .map_err(|_| CommandError::internal("migration scan worker stopped unexpectedly"))?
    .map_err(|_| CommandError::internal("migration scan failed"))?;
    let selected: std::collections::BTreeSet<PathBuf> =
        selected_paths.into_iter().map(PathBuf::from).collect();
    let mut scan = scan;
    scan.items.retain(|item| selected.contains(&item.path));
    let conflicts = at_migrate::conflicts::detect_conflicts(&scan);
    Ok(MigrateConflictsView {
        conflicts: conflicts
            .conflicts
            .into_iter()
            .map(|conflict| MigrateConflictView {
                kind: format!("{:?}", conflict.kind).to_lowercase(),
                name: conflict.name,
                copies: conflict
                    .copies
                    .into_iter()
                    .map(|copy| MigrateConflictCopyView {
                        agent: copy.agent.to_string(),
                        path: copy.path.display().to_string(),
                        scope: copy.scope.to_string(),
                        raw_hash: copy.raw_hash,
                        normalized_hash: copy.normalized_hash,
                    })
                    .collect(),
                formatting_only: conflict.formatting_only,
                unified_diff: conflict.unified_diff,
                proposed_resolution: format!("{:?}", conflict.proposed_resolution).to_lowercase(),
            })
            .collect(),
    })
}
