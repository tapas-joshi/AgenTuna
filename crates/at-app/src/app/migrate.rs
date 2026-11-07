//! Typed native migration scan, plan, apply and undo commands.
use super::{
    bridge::{JobError, JobSpec},
    commands::{CommandError, CommandErrorCode},
    state::AppState,
};
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::scan::{
    Ownership, ParseStatus, ScanItem, ScanReport, ScanRequest, ScanRoot, ScanRootStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};
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
    /// Authoritative ownership label: `in_agentuna`, `conflict` or `not_in_agentuna`.
    pub state: String,
    /// Managed, conflict, not-in-agentuna or unknown ownership state.
    pub ownership: String,
}

/// Status of an attempted migration discovery root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub enum MigrateScanRootStatus {
    /// The root was read successfully and contained items.
    Scanned,
    /// The root was read successfully and contained no items.
    Empty,
    /// The candidate does not exist.
    NotPresent,
    /// The candidate could not be safely read.
    Unreadable,
}

/// One attempted native folder or document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateScanRootView {
    /// Native folder or document path.
    pub path: String,
    /// Kind of native item sought in this root.
    pub kind: String,
    /// Agents that read this root.
    pub agents: Vec<String>,
    /// Number of distinct found items, including skipped items.
    #[ts(type = "number")]
    pub found_items: i64,
    /// Rows that would be moved into the store.
    #[ts(type = "number")]
    pub new_items: i64,
    /// Rows skipped due to read or parse errors.
    #[ts(type = "number")]
    pub skipped_items: i64,
    /// Rows that already point at the store.
    #[ts(type = "number")]
    pub in_sync: i64,
    /// Whether the candidate was scanned, empty, absent, or unreadable.
    pub status: MigrateScanRootStatus,
    /// Content-free reason when the root is unreadable.
    pub unreadable_reason: Option<String>,
}

/// One scanned folder or document that could not be read, with the path that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateScanWarningView {
    /// Path of the root or configuration file that could not be read.
    pub path: String,
    /// Item kind being discovered.
    pub kind: String,
    /// Agent whose root failed.
    pub agent: String,
    /// Scope of the failing path.
    pub scope: String,
    /// OS or parse error text.
    pub message: String,
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
    /// Roots and configuration files that could not be read, named by path.
    pub warnings: Vec<MigrateScanWarningView>,
    /// Scanned folders and documents with their item counts.
    pub roots: Vec<MigrateScanRootView>,
}

/// The authoritative ownership label shown in the migrate table.
///
/// `In AgenTuna` means the normalized content matches the store item, `Conflict` means the item
/// exists in AgenTuna with different content, and `Not in AgenTuna` means no store item matches.
const fn ownership_state(ownership: Ownership) -> &'static str {
    match ownership {
        Ownership::Managed => "in_agentuna",
        Ownership::Conflict => "conflict",
        _ => "not_in_agentuna",
    }
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
        state: ownership_state(item.ownership).to_owned(),
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
                    scope,
                    path: layout.root().join(item.path.to_string()),
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
///
/// # Errors
/// Returns a typed command error for unknown projects or an unavailable scan worker.
#[tauri::command]
pub async fn migrate_scan(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<MigrateScanView, CommandError> {
    migrate_scan_inner(&state, project_id).await
}

/// Bridge-facing body of [`migrate_scan`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_scan`].
pub async fn migrate_scan_inner(
    app: &AppState,
    project_id: Option<String>,
) -> Result<MigrateScanView, CommandError> {
    let project = if let Some(id) = project_id {
        let database = app.database().map_err(CommandError::unavailable)?;
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
    let paths = super::paths::resolve_command_paths(app, project.as_deref())
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
    Ok(scan_view(report))
}

/// Projects a scan report into the webview view with authoritative ownership labels.
fn scan_view(report: ScanReport) -> MigrateScanView {
    MigrateScanView {
        items: report.items.into_iter().map(item_view).collect(),
        partial: report.partial,
        skipped_roots: report
            .skipped_roots
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        warnings: report
            .warnings
            .into_iter()
            .map(|warning| MigrateScanWarningView {
                path: warning.path.display().to_string(),
                kind: warning.kind.to_string(),
                agent: warning.agent.to_string(),
                scope: warning.scope.to_string(),
                message: warning.message,
            })
            .collect(),
        roots: report.roots.into_iter().map(root_view).collect(),
    }
}

/// Completed scan views keyed by bridge job id, bounded to the newest eight.
static SCAN_RESULTS: LazyLock<Mutex<BTreeMap<String, MigrateScanView>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn record_scan_result(job_id: String, view: MigrateScanView) {
    let mut results = SCAN_RESULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.insert(job_id, view);
    while results.len() > 8 {
        let oldest = results.keys().next().cloned();
        if let Some(oldest) = oldest {
            results.remove(&oldest);
        }
    }
}

/// Forwards discovery steps to the bridge and stops a cancelled scan.
struct ScanJobObserver {
    progress: super::bridge::ProgressSender,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl at_migrate::scan::ScanObserver for ScanJobObserver {
    fn step(&mut self, step: &at_migrate::scan::ScanProgress) -> bool {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let done = step.completed + 1;
        let fraction = if step.total == 0 {
            0.0
        } else {
            done as f64 / step.total as f64
        };
        self.progress.fraction(
            fraction,
            format!(
                "Reading {} {} {} ({done}/{})",
                step.agent, step.scope, step.kind, step.total
            ),
        );
        true
    }
}

/// Starts the native scan as a cancellable bridge job and returns its id.
///
/// Results stream as bridge progress events, and the finished view is fetched with
/// [`migrate_scan_result`]. Cancelling the job stops the scan at the next discovery step.
///
/// # Errors
/// [`CommandError`] for unknown projects or an unavailable bridge.
#[tauri::command]
pub async fn migrate_scan_start(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<super::bridge::JobId, CommandError> {
    migrate_scan_start_inner(&state, project_id).await
}

/// Bridge-facing body of [`migrate_scan_start`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_scan_start`].
pub async fn migrate_scan_start_inner(
    app: &AppState,
    project_id: Option<String>,
) -> Result<super::bridge::JobId, CommandError> {
    let inputs = migration_inputs(app, project_id.as_deref())?;
    let job = JobSpec::new_cancellable("Migration scan", move |progress, cancel| async move {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let worker_progress = progress.clone();
        let cancelled = cancel.cancellation_flag();
        let worker = tokio::task::spawn_blocking(move || {
            let environment = AgentEnvironment {
                home: inputs.home.clone(),
                project: inputs.project.clone(),
                variables: BTreeMap::new(),
            };
            let registry = AgentAdapterRegistry::register();
            let mut observer = ScanJobObserver {
                progress: worker_progress,
                cancelled,
            };
            match at_migrate::scan::scan_with(
                ScanRequest {
                    environment: &environment,
                    adapters: &registry,
                    store_roots: &inputs.roots,
                    store_items: &inputs.store_items,
                },
                &mut observer,
            ) {
                Ok(report) => Ok(Some(scan_view(report))),
                Err(at_migrate::scan::ScanError::Cancelled) => Ok(None),
                Err(_) => Err(JobError::Failed {
                    message: "the agent folders could not be scanned".to_owned(),
                }),
            }
        })
        .await
        .map_err(|join| JobError::Failed {
            message: format!("the scan worker stopped: {join}"),
        })?;
        let view = match worker {
            Ok(Some(view)) => view,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error),
        };
        if cancel.is_cancelled() {
            return Ok(());
        }
        record_scan_result(progress.id().as_str().to_owned(), view);
        progress.fraction(1.0, "Scan complete");
        Ok(())
    });
    let handle = app.bridge().spawn_job(job).map_err(CommandError::from)?;
    Ok(handle.id().clone())
}

/// Returns the recorded result of one scan job, or `None` while it is still running.
///
/// # Errors
/// [`CommandErrorCode::NotFound`] when the job finished without recording a result, for example
/// after cancellation.
#[tauri::command]
pub fn migrate_scan_result(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Option<MigrateScanView>, CommandError> {
    migrate_scan_result_inner(&state, &job_id)
}

/// Bridge-facing body of [`migrate_scan_result`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_scan_result`].
pub fn migrate_scan_result_inner(
    app: &AppState,
    job_id: &str,
) -> Result<Option<MigrateScanView>, CommandError> {
    {
        let results = SCAN_RESULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = results.get(job_id) {
            return Ok(Some(found.clone()));
        }
    }
    if app
        .bridge()
        .jobs()
        .iter()
        .any(|snapshot| snapshot.id.as_str() == job_id)
    {
        return Ok(None);
    }
    Err(CommandError::new(
        CommandErrorCode::NotFound,
        "No completed scan was recorded for this job.",
    ))
}

/// Projects the scanner's actual candidate outcomes without inferring roots from items.
fn root_view(root: ScanRoot) -> MigrateScanRootView {
    MigrateScanRootView {
        path: root.path.display().to_string(),
        kind: root.kind.to_string(),
        agents: root
            .agents
            .into_iter()
            .map(|agent| agent.to_string())
            .collect(),
        found_items: root.found_items,
        new_items: root.new_items,
        skipped_items: root.skipped_items,
        in_sync: root.in_sync,
        status: match root.status {
            ScanRootStatus::Scanned => MigrateScanRootStatus::Scanned,
            ScanRootStatus::Empty => MigrateScanRootStatus::Empty,
            ScanRootStatus::NotPresent => MigrateScanRootStatus::NotPresent,
            _ => MigrateScanRootStatus::Unreadable,
        },
        unreadable_reason: root.unreadable_reason,
    }
}
/// One comparable copy in a migration conflict.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateConflictCopyView {
    /// Agent identifier, or `store` for the canonical store copy.
    pub agent: String,
    /// `native` for an agent copy, `store` for the canonical store copy.
    pub origin: String,
    /// Native path, or the canonical store path for a store copy.
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
///
/// # Errors
/// Returns a typed command error for unknown projects or an unavailable scan worker.
#[tauri::command]
pub async fn migrate_conflicts(
    state: State<'_, AppState>,
    project_id: Option<String>,
    selected_paths: Vec<String>,
) -> Result<MigrateConflictsView, CommandError> {
    migrate_conflicts_inner(&state, project_id, selected_paths).await
}

/// Bridge-facing body of [`migrate_conflicts`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_conflicts`].
pub async fn migrate_conflicts_inner(
    app: &AppState,
    project_id: Option<String>,
    selected_paths: Vec<String>,
) -> Result<MigrateConflictsView, CommandError> {
    let project = if let Some(id) = project_id {
        let database = app.database().map_err(CommandError::unavailable)?;
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
    let paths = super::paths::resolve_command_paths(app, project.as_deref())
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
                        agent: copy
                            .agent
                            .as_ref()
                            .map_or_else(|| "store".to_owned(), ToString::to_string),
                        origin: match copy.origin {
                            at_migrate::conflicts::CopyOrigin::Native => "native",
                            at_migrate::conflicts::CopyOrigin::Store => "store",
                            _ => "unknown",
                        }
                        .to_owned(),
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

/// One explicit conflict choice carried into the migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateResolutionView {
    /// Normalized conflict name (`kind:name` is not used; the name alone keys the choice).
    pub name: String,
    /// `keep_a`, `keep_b` or `keep_both`.
    pub choice: String,
    /// Two names for `keep_both`, in A/B copy order.
    pub names: Vec<String>,
}

/// Inputs for one reviewed migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanRequest {
    /// Registered project providing Project scope paths.
    pub project_id: Option<String>,
    /// Native paths selected in the scan.
    pub selected_paths: Vec<String>,
    /// Explicit choices for every conflict group.
    pub resolutions: Vec<MigrateResolutionView>,
    /// `replace_with_links` or `leave_and_watch`.
    pub strategy: String,
}

/// One ordered step of a reviewed migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanStepView {
    /// Step sequence, starting at one.
    pub seq: usize,
    /// Action name.
    pub action: String,
    /// Store scope that receives this step's item.
    pub scope: String,
    /// Selected canonical item name.
    pub item_name: Option<String>,
    /// Source path.
    pub source: Option<String>,
    /// Destination path.
    pub destination: Option<String>,
    /// Planned backup destination.
    pub backup_path: Option<String>,
    /// Bytes read or written.
    pub bytes: u64,
    /// Agents that can read the native path.
    pub agents: Vec<String>,
}

/// Summary counts of a reviewed migration plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanCountsView {
    /// Unique item copies selected.
    pub items: i32,
    /// Destinations written.
    pub files_created: i32,
    /// Originals backed up.
    pub files_backed_up: i32,
    /// Links created.
    pub links_written: i32,
    /// Source copies left in place.
    pub files_left_in_place: i32,
}

/// One receiving scope of a reviewed migration plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanScopeView {
    /// Store scope receiving these items (`user`, `project` or `local`).
    pub scope: String,
    /// Resolved store root these items are copied into.
    pub store_path: String,
    /// What happens in this store.
    pub counts: MigratePlanCountsView,
    /// Ordered steps written into this store.
    pub steps: Vec<MigratePlanStepView>,
}

/// A plan built from the scan and the user's conflict choices, plus its dry run.
///
/// A selection that spans native scopes produces one plan per receiving scope; `scopes` lists
/// them in precedence order with the store root each one writes into, and `plan_hash` binds all
/// of them to one reviewed apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanView {
    /// SHA-256 binding every reviewed scope, step and the strategy.
    pub plan_hash: String,
    /// Resolved directory that receives the originals' backups.
    pub backup_dir: String,
    /// Selected strategy.
    pub strategy: String,
    /// Summary counts across every receiving scope.
    pub counts: MigratePlanCountsView,
    /// One entry per receiving scope, each carrying its store path.
    pub scopes: Vec<MigratePlanScopeView>,
    /// Files rechecked for the dry run.
    pub files_checked: i32,
}

/// Serializes one reviewed step for the webview.
fn step_view(step: at_migrate::plan::PlanStep) -> MigratePlanStepView {
    MigratePlanStepView {
        seq: step.seq,
        action: format!("{:?}", step.action).to_lowercase(),
        scope: step.scope.to_string(),
        item_name: step.item_name,
        source: step.source.map(|path| path.display().to_string()),
        destination: step.destination.map(|path| path.display().to_string()),
        backup_path: step.backup_path.map(|path| path.display().to_string()),
        bytes: step.bytes,
        agents: step
            .agents
            .into_iter()
            .map(|agent| agent.to_string())
            .collect(),
    }
}

/// Serializes one plan's counts for the webview.
fn counts_view(counts: at_migrate::plan::PlanCounts) -> MigratePlanCountsView {
    MigratePlanCountsView {
        items: i32::try_from(counts.items).unwrap_or(i32::MAX),
        files_created: i32::try_from(counts.files_created).unwrap_or(i32::MAX),
        files_backed_up: i32::try_from(counts.files_backed_up).unwrap_or(i32::MAX),
        links_written: i32::try_from(counts.links_written).unwrap_or(i32::MAX),
        files_left_in_place: i32::try_from(counts.files_left_in_place).unwrap_or(i32::MAX),
    }
}

/// Adds every scope's counts into the single total the plan footer shows.
fn summed_counts(plans: &[at_migrate::plan::MigrationPlan]) -> MigratePlanCountsView {
    let mut total = at_migrate::plan::PlanCounts {
        items: 0,
        files_created: 0,
        files_backed_up: 0,
        links_written: 0,
        files_left_in_place: 0,
    };
    for plan in plans {
        total.items += plan.counts.items;
        total.files_created += plan.counts.files_created;
        total.files_backed_up += plan.counts.files_backed_up;
        total.links_written += plan.counts.links_written;
        total.files_left_in_place += plan.counts.files_left_in_place;
    }
    counts_view(total)
}

/// Wire name of the reviewed strategy.
const fn strategy_name(strategy: at_migrate::plan::MigrationStrategy) -> &'static str {
    match strategy {
        at_migrate::plan::MigrationStrategy::ReplaceWithLinks => "replace_with_links",
        at_migrate::plan::MigrationStrategy::LeaveAndWatch => "leave_and_watch",
        _ => "unknown",
    }
}

/// Builds one reviewed plan per receiving scope from a scan and the user's conflict choices.
///
/// Each selected item is planned into the store of the scope it was discovered in; conflicts are
/// detected inside each scope and the explicit resolutions are applied there, so the returned
/// plans are the exact write list the preview step shows.
// One argument per independent input of the reviewed selection; both call sites build them inline.
#[allow(clippy::too_many_arguments)]
fn plans_from_scan(
    scan: ScanReport,
    selected_paths: &std::collections::BTreeSet<PathBuf>,
    resolutions: &[MigrateResolutionView],
    strategy: at_migrate::plan::MigrationStrategy,
    stores: &at_migrate::plan::ScopeStores,
    project_id: Option<&str>,
    backup_root: PathBuf,
    copy_agents: &std::collections::BTreeSet<at_model::AgentId>,
) -> Result<Vec<at_migrate::plan::MigrationPlan>, String> {
    let mut choices = BTreeMap::new();
    for resolution in resolutions {
        let choice = match resolution.choice.as_str() {
            "keep_a" => at_migrate::plan::ConflictResolution::KeepA,
            "keep_b" => at_migrate::plan::ConflictResolution::KeepB,
            "keep_both" => at_migrate::plan::ConflictResolution::KeepBoth {
                names: resolution.names.clone(),
            },
            other => return Err(format!("unknown conflict choice `{other}`")),
        };
        choices.insert(resolution.name.clone(), choice);
    }
    at_migrate::plan::build_scope_plans(at_migrate::plan::ScopePlansRequest {
        scan: &scan,
        resolutions: &choices,
        selected_paths,
        stores,
        target_scope: None,
        project_id,
        copy_agents,
        strategy,
        backup_root,
    })
    .map_err(|error| plan_error_text(&error))
}

/// Projects the built plans into the webview view, grouped by receiving scope.
fn plan_view(
    plans: Vec<at_migrate::plan::MigrationPlan>,
    stores: &at_migrate::plan::ScopeStores,
    backup_dir: String,
) -> Result<MigratePlanView, String> {
    let Some(first) = plans.first() else {
        return Err(plan_error_text(
            &at_migrate::plan::PlanError::EmptySelection,
        ));
    };
    let strategy = strategy_name(first.strategy);
    let plan_hash =
        at_migrate::plan::plans_hash(&plans).map_err(|error| plan_error_text(&error))?;
    let counts = summed_counts(&plans);
    let mut scopes = Vec::with_capacity(plans.len());
    let mut files_checked = 0usize;
    for plan in &plans {
        let missing = at_migrate::plan::PlanError::MissingStore { scope: plan.scope };
        let store_path = stores
            .root(plan.scope)
            .ok_or_else(|| plan_error_text(&missing))?
            .display()
            .to_string();
        files_checked += at_migrate::plan::dry_run(plan).len();
        scopes.push(MigratePlanScopeView {
            scope: plan.scope.to_string(),
            store_path,
            counts: counts_view(plan.counts),
            steps: plan.steps.iter().cloned().map(step_view).collect(),
        });
    }
    Ok(MigratePlanView {
        plan_hash,
        backup_dir,
        strategy: strategy.to_owned(),
        counts,
        scopes,
        files_checked: i32::try_from(files_checked).unwrap_or(i32::MAX),
    })
}

/// Explains a plan refusal in the terms the plan screen shows.
///
/// A selection that contains Project or Local native files needs a selected project, because
/// those items belong in a project store; the message says so instead of naming a missing store.
fn plan_error_text(error: &at_migrate::plan::PlanError) -> String {
    match error {
        at_migrate::plan::PlanError::MissingStore { scope }
        | at_migrate::plan::PlanError::MissingProject { scope } => format!(
            "The selected items include {scope}-scope native files, which belong in the {scope} \
             store. Select a project and rebuild the plan."
        ),
        _ => error.to_string(),
    }
}

/// Builds the migration plan from a fresh scan and the reviewed conflict choices.
///
/// The scan is repeated so the plan binds to current native bytes; the returned dry run is the
/// exact ordered write list the preview step shows. Each item is planned into the store of the
/// scope it was discovered in. Nothing is written.
///
/// # Errors
/// Returns a typed command error for unknown projects, an unknown strategy, a selection that
/// needs a project while none is selected, or a refused plan.
#[tauri::command]
pub async fn migrate_plan(
    state: State<'_, AppState>,
    request: MigratePlanRequest,
) -> Result<MigratePlanView, CommandError> {
    migrate_plan_inner(&state, request).await
}

/// Bridge-facing body of [`migrate_plan`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_plan`].
pub async fn migrate_plan_inner(
    app: &AppState,
    request: MigratePlanRequest,
) -> Result<MigratePlanView, CommandError> {
    let strategy = strategy_from_wire(&request.strategy)?;
    let inputs = migration_inputs(app, request.project_id.as_deref())?;
    let selected_paths = request
        .selected_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<std::collections::BTreeSet<_>>();
    let resolutions = request.resolutions;
    let project_id = request.project_id.clone();
    let backup_dir = inputs.backup_root.display().to_string();
    let view = tokio::task::spawn_blocking(move || {
        let environment = AgentEnvironment {
            home: inputs.home.clone(),
            project: inputs.project.clone(),
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        let scan = at_migrate::scan::scan(ScanRequest {
            environment: &environment,
            adapters: &registry,
            store_roots: &inputs.roots,
            store_items: &inputs.store_items,
        })
        .map_err(|_| {
            CommandError::new(
                CommandErrorCode::Unavailable,
                "The selected agent folders could not be read.",
            )
        })?;
        let plans = plans_from_scan(
            scan,
            &selected_paths,
            &resolutions,
            strategy,
            &inputs.stores,
            project_id.as_deref(),
            inputs.backup_root.clone(),
            &inputs.copy_agents,
        )
        .map_err(|error| CommandError::new(CommandErrorCode::InvalidArgument, error))?;
        plan_view(plans, &inputs.stores, backup_dir)
            .map_err(|error| CommandError::new(CommandErrorCode::InvalidArgument, error))
    })
    .await
    .map_err(|_| CommandError::internal("migration plan worker stopped unexpectedly"))??;
    Ok(view)
}

// ---------------------------------------------------------------------------
// Reviewed apply (TC-023) and undo readiness (TC-024)
// ---------------------------------------------------------------------------

/// Stable failure class of a migration apply, for branching in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum MigrationApplyFailureCode {
    /// The reviewed plan no longer matches the current native bytes.
    StalePlan,
    /// The plan could not be built or revalidated from the current scan.
    PlanInvalid,
    /// An agent folder refused writes.
    PermissionDenied,
    /// macOS protects an agent folder with TCC.
    FullDiskAccess,
    /// A native source could not be read or hashed.
    SourceRead,
    /// The selected item kind cannot be migrated.
    Unsupported,
    /// The canonical store rejected the change.
    Store,
    /// The reviewed ChangeSet failed and was rolled back.
    ApplyFailed,
    /// Everything else.
    Internal,
}

/// Typed migration failure delivered through the job-finished event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrationApplyFailure {
    /// Stable failure class.
    pub code: MigrationApplyFailureCode,
    /// Human-readable, secret-free explanation.
    pub message: String,
    /// Offending native path when the failure is about one file or folder.
    pub path: Option<String>,
    /// What the user can do next.
    pub remedy: String,
}

impl MigrationApplyFailure {
    /// Builds a failure with a path and remedy.
    fn new(
        code: MigrationApplyFailureCode,
        message: impl Into<String>,
        path: Option<&std::path::Path>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            path: path.map(|path| path.display().to_string()),
            remedy: remedy.into(),
        }
    }

    /// Internal fallback with the retry remedy.
    fn internal(message: impl Into<String>) -> Self {
        Self::new(
            MigrationApplyFailureCode::Internal,
            message,
            None,
            "Retry the apply; if it fails again, export diagnostics from the logs screen.",
        )
    }
}

/// Maps an apply failure onto the typed job-failure contract.
fn apply_failure(error: at_migrate::apply::ApplyPlanError) -> MigrationApplyFailure {
    use at_migrate::apply::ApplyPlanError;
    match error {
        ApplyPlanError::PermissionDenied { path, required } => {
            let code = if required.contains("Full Disk Access") {
                MigrationApplyFailureCode::FullDiskAccess
            } else {
                MigrationApplyFailureCode::PermissionDenied
            };
            MigrationApplyFailure::new(
                code,
                format!("cannot write `{}`: {required}", path.display()),
                Some(&path),
                required,
            )
        }
        ApplyPlanError::SourceRead { path, source } => MigrationApplyFailure::new(
            MigrationApplyFailureCode::SourceRead,
            format!("cannot read `{}`: {source}", path.display()),
            Some(&path),
            "The native item changed or became unreadable; rescan and review the plan again.",
        ),
        ApplyPlanError::Unsupported(path) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::Unsupported,
            format!("`{}` cannot be migrated", path.display()),
            Some(&path),
            "Leave this item in place for now; its kind cannot be migrated safely yet.",
        ),
        ApplyPlanError::OutsideStore(path) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::Store,
            format!("`{}` is outside the AgenTuna store", path.display()),
            Some(&path),
            "Rebuild the plan; the reviewed destination escaped the store.",
        ),
        ApplyPlanError::Store(message) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::Store,
            format!("the AgenTuna store rejected the migration: {message}"),
            None,
            "Check the store on the Overview screen, then retry the apply.",
        ),
        ApplyPlanError::Plan(_) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::StalePlan,
            "the reviewed plan no longer matches the files on disk",
            None,
            "Go back to the scan, rebuild the plan and review it again.",
        ),
        ApplyPlanError::Apply(_) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::ApplyFailed,
            "the reviewed changes failed and were rolled back",
            None,
            "Nothing is left half-written; check the backups list, then retry the apply.",
        ),
        ApplyPlanError::MissingProjectId(slug) => MigrationApplyFailure::new(
            MigrationApplyFailureCode::PlanInvalid,
            format!("the project-scope item `{slug}` requires an active project"),
            None,
            "Select a project before migrating project-scope items.",
        ),
        ApplyPlanError::Metadata(_) | ApplyPlanError::OperationKind(_) => {
            MigrationApplyFailure::internal("the migration metadata could not be recorded")
        }
        ApplyPlanError::Clock => {
            MigrationApplyFailure::internal("the system clock is before the Unix epoch")
        }
        _ => MigrationApplyFailure::internal("the migration could not be applied"),
    }
}

/// One deviation from the reviewed plan that completed instead of failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrationApplyWarningView {
    /// Warning kind (`copy_fallback`).
    pub kind: String,
    /// Agent path that received the fallback.
    pub path: String,
    /// Deployment mode actually written at that path.
    pub observed_mode: String,
}

/// Inputs for one confirmed migration apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateApplyRequest {
    /// Registered project providing Project scope paths.
    pub project_id: Option<String>,
    /// Native paths selected in the scan.
    pub selected_paths: Vec<String>,
    /// Explicit choices for every conflict group.
    pub resolutions: Vec<MigrateResolutionView>,
    /// `replace_with_links` or `leave_and_watch`.
    pub strategy: String,
    /// `plan_hash` from the reviewed plan; a mismatch refuses the apply.
    pub plan_hash: String,
}

/// What one receiving scope holds after a completed apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateApplyScopeView {
    /// Store scope that received these items.
    pub scope: String,
    /// Resolved store root that received them.
    pub store_path: String,
    /// What landed in this store.
    pub counts: MigratePlanCountsView,
}

/// Result of a completed migration apply job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateApplyResultView {
    /// Bridge job that produced this result.
    pub job_id: String,
    /// Durable operation id accepted by the undo command.
    pub operation_id: String,
    /// Number of filesystem actions completed.
    pub completed: i64,
    /// Absolute paths changed by the operation.
    pub touched_paths: Vec<String>,
    /// Deviations that completed instead of failing.
    pub warnings: Vec<MigrationApplyWarningView>,
    /// Where the operation backups were written.
    pub backup_dir: String,
    /// One entry per receiving scope, each with its store path and what landed there.
    pub scopes: Vec<MigrateApplyScopeView>,
    /// Strategy that was applied.
    pub strategy: String,
    /// Plan content hash that was confirmed before writing.
    pub plan_hash: String,
    /// Reviewed summary counts, matching the applied operation.
    pub counts: MigratePlanCountsView,
    /// Reviewed steps, for per-step results on the done screen.
    pub steps: Vec<MigratePlanStepView>,
}

/// Results of completed migration apply jobs, newest last, bounded.
static APPLY_RESULTS: LazyLock<Mutex<Vec<MigrateApplyResultView>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

fn record_apply_result(result: MigrateApplyResultView) {
    let mut results = APPLY_RESULTS.lock().expect("apply result registry");
    results.push(result);
    let excess = results.len().saturating_sub(8);
    results.drain(0..excess);
}

/// Shared, validated inputs for building one reviewed migration plan.
struct MigrationInputs {
    project: Option<PathBuf>,
    home: PathBuf,
    /// Store root per scope native discovery can report.
    stores: at_migrate::plan::ScopeStores,
    backup_root: PathBuf,
    roots: Vec<PathBuf>,
    store_items: Vec<at_migrate::scan::StoreItem>,
    copy_agents: std::collections::BTreeSet<at_model::AgentId>,
}

fn strategy_from_wire(value: &str) -> Result<at_migrate::plan::MigrationStrategy, CommandError> {
    match value {
        "replace_with_links" => Ok(at_migrate::plan::MigrationStrategy::ReplaceWithLinks),
        "leave_and_watch" => Ok(at_migrate::plan::MigrationStrategy::LeaveAndWatch),
        _ => Err(CommandError::invalid_argument(
            "strategy",
            "expected replace_with_links or leave_and_watch",
        )),
    }
}

/// Resolves project, store and agent inputs shared by plan and apply.
fn migration_inputs(
    app: &AppState,
    project_id: Option<&str>,
) -> Result<MigrationInputs, CommandError> {
    let (project, settings, agents) = {
        let database = app.database().map_err(CommandError::unavailable)?;
        let project = match project_id {
            Some(id) => Some(PathBuf::from(
                at_db::projects::find_project_by_id(database.connection(), id)
                    .map_err(|_| CommandError::internal("could not query registered projects"))?
                    .ok_or_else(|| {
                        CommandError::invalid_argument("project_id", "project is not registered")
                    })?
                    .path,
            )),
            None => None,
        };
        let settings = at_db::settings::load(database.connection())
            .map_err(|_| CommandError::internal("could not read deployment settings"))?;
        let agents = at_db::agents::list_agents_on_mac(database.connection())
            .map_err(|_| CommandError::internal("could not read detected agents"))?;
        (project, settings, agents)
    };
    let paths = super::paths::resolve_command_paths(app, project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let stores = migration_stores(&paths)?;
    let backup_root = paths.backup_dir().join("migration");
    let (roots, store_items) = store_inventory(&paths)?;
    let copy_agents = agents
        .iter()
        .filter(|agent| settings.link_mode(&agent.id) == at_model::LinkMode::Copy)
        .map(|agent| agent.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    Ok(MigrationInputs {
        project,
        home,
        stores,
        backup_root,
        roots,
        store_items,
        copy_agents,
    })
}

/// Store root per scope that native discovery can report.
///
/// The User store always resolves. Project and Local need a selected project, so without one they
/// stay out of the map: planning an item discovered there then refuses with a message naming the
/// missing project instead of writing it into the wrong store.
fn migration_stores(
    paths: &at_store::paths::AppPaths,
) -> Result<at_migrate::plan::ScopeStores, CommandError> {
    let mut stores = at_migrate::plan::ScopeStores::new();
    for scope in [
        at_model::Scope::User,
        at_model::Scope::Project,
        at_model::Scope::Local,
    ] {
        match at_store::layout::StoreLayout::for_scope(paths, scope) {
            Ok(layout) => stores.insert(scope, layout.root().to_path_buf()),
            Err(_) if matches!(scope, at_model::Scope::Project | at_model::Scope::Local) => {}
            Err(_) => {
                return Err(CommandError::unavailable(
                    "the migration store is unavailable",
                ));
            }
        }
    }
    Ok(stores)
}

/// Authorized roots for the native paths a migration may replace.
fn migration_authorized_roots(
    paths: &at_store::paths::AppPaths,
) -> Result<at_fs::ApplyRoots, CommandError> {
    super::overwrite::adopt_authorized_roots(paths)
}

/// Starts the confirmed migration as a cancellable bridge job and returns its id.
///
/// The job re-scans, rebuilds the plan, verifies it still hashes to the reviewed
/// `plan_hash`, then submits one reversible ChangeSet through
/// [`at_migrate::apply::apply_plan`]. Cancellation stops the job only before the write
/// begins; once submitted, the migration either completes or rolls back as one operation.
///
/// # Errors
/// [`CommandError`] for unknown projects, an unknown strategy or an unavailable bridge.
#[tauri::command]
pub async fn migrate_apply(
    state: State<'_, AppState>,
    request: MigrateApplyRequest,
) -> Result<super::bridge::JobId, CommandError> {
    migrate_apply_inner(&state, request).await
}

/// Bridge-facing body of [`migrate_apply`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_apply`].
pub async fn migrate_apply_inner(
    app: &AppState,
    request: MigrateApplyRequest,
) -> Result<super::bridge::JobId, CommandError> {
    let strategy = strategy_from_wire(&request.strategy)?;
    let inputs = migration_inputs(app, request.project_id.as_deref())?;
    let authorized = migration_authorized_roots(
        &super::paths::resolve_command_paths(app, inputs.project.as_deref())
            .map_err(|_| CommandError::unavailable("application paths are unavailable"))?,
    )?;
    let selected_paths = request
        .selected_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<std::collections::BTreeSet<_>>();
    let resolutions = request.resolutions;
    let reviewed_hash = request.plan_hash;
    let strategy_text = request.strategy.clone();
    let database = app.database_handle().map_err(CommandError::unavailable)?;
    let refresh_paths = app.paths().cloned();
    let job = JobSpec::new_cancellable("Migration apply", move |progress, cancel| async move {
        if cancel.is_cancelled() {
            return Ok(());
        }
        progress.fraction(0.05, "Re-reading the selected agent folders");
        let cancellation = cancel.cancellation_flag();
        let worker_progress = progress.clone();
        let worker = tokio::task::spawn_blocking(move || {
            worker_progress.fraction(0.2, "Rebuilding the reviewed plan");
            let environment = AgentEnvironment {
                home: inputs.home.clone(),
                project: inputs.project.clone(),
                variables: BTreeMap::new(),
            };
            let registry = AgentAdapterRegistry::register();
            let built = at_migrate::scan::scan(ScanRequest {
                environment: &environment,
                adapters: &registry,
                store_roots: &inputs.roots,
                store_items: &inputs.store_items,
            })
            .map_err(|_| {
                MigrationApplyFailure::new(
                    MigrationApplyFailureCode::PlanInvalid,
                    "the selected agent folders could not be re-read",
                    None::<&std::path::Path>,
                    "Rescan from the first migration step, then review the plan again.",
                )
            })
            .and_then(|scan| {
                plans_from_scan(
                    scan,
                    &selected_paths,
                    &resolutions,
                    strategy,
                    &inputs.stores,
                    request.project_id.as_deref(),
                    inputs.backup_root.clone(),
                    &inputs.copy_agents,
                )
                .map_err(|error| {
                    MigrationApplyFailure::new(
                        MigrationApplyFailureCode::PlanInvalid,
                        format!("the reviewed plan could not be rebuilt: {error}"),
                        None::<&std::path::Path>,
                        "Go back to the scan, rebuild the plan and review it again.",
                    )
                })
            });
            let plans = match built {
                Ok(built) => built,
                Err(failure) => return Err(failure),
            };
            let rebuilt_hash = at_migrate::plan::plans_hash(&plans).map_err(|error| {
                MigrationApplyFailure::new(
                    MigrationApplyFailureCode::PlanInvalid,
                    format!("the reviewed plan could not be rebuilt: {error}"),
                    None::<&std::path::Path>,
                    "Go back to the scan, rebuild the plan and review it again.",
                )
            })?;
            if rebuilt_hash != reviewed_hash {
                return Err(MigrationApplyFailure::new(
                    MigrationApplyFailureCode::StalePlan,
                    "the files changed since the plan was reviewed",
                    None::<&std::path::Path>,
                    "Go back to the scan, rebuild the plan and review it again.",
                ));
            }
            if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(None);
            }
            let mut scopes = Vec::with_capacity(plans.len());
            for plan in &plans {
                let Some(store_root) = inputs.stores.root(plan.scope) else {
                    return Err(MigrationApplyFailure::new(
                        MigrationApplyFailureCode::PlanInvalid,
                        format!("the {} store is unavailable for these items", plan.scope),
                        None::<&std::path::Path>,
                        "Select the project these items belong to, then review the plan again.",
                    ));
                };
                scopes.push(MigrateApplyScopeView {
                    scope: plan.scope.to_string(),
                    store_path: store_root.display().to_string(),
                    counts: counts_view(plan.counts),
                });
            }
            worker_progress.fraction(0.6, "Backing up originals and writing reviewed changes");
            let apply = {
                let mut database = database.lock().map_err(|_| {
                    MigrationApplyFailure::internal("the index database is unavailable")
                })?;
                let mut context = at_migrate::apply::ApplyPlansContext {
                    stores: &inputs.stores,
                    backup_root: &inputs.backup_root,
                    authorized_roots: &authorized,
                    database: &mut database,
                    project_id: request.project_id.as_deref(),
                };
                at_migrate::apply::apply_plans(&plans, &mut context).map_err(apply_failure)
            };
            let outcome = match apply {
                Ok(outcome) => outcome,
                Err(failure) => return Err(failure),
            };
            // The plan replaced agent files, so the findings derived from them are stale before
            // the result is even shown: rebuild them from the disk the apply just wrote.
            if let Some(refresh_paths) = refresh_paths.as_ref()
                && let Ok(database) = database.lock()
            {
                super::native_scan::refresh_native_findings(refresh_paths, database.connection());
            }
            worker_progress.fraction(0.95, "Recording the migration");
            let view = MigrateApplyResultView {
                job_id: worker_progress.id().as_str().to_owned(),
                operation_id: outcome.operation_id,
                completed: i64::try_from(outcome.completed).unwrap_or(i64::MAX),
                touched_paths: outcome
                    .touched_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                warnings: outcome
                    .warnings
                    .iter()
                    .map(|warning| match warning {
                        at_migrate::apply::MigrationWarning::CopyFallback {
                            path,
                            observed_mode,
                        } => MigrationApplyWarningView {
                            kind: "copy_fallback".to_owned(),
                            path: path.display().to_string(),
                            observed_mode: (*observed_mode).to_owned(),
                        },
                        _ => MigrationApplyWarningView {
                            kind: "unknown".to_owned(),
                            path: String::new(),
                            observed_mode: String::new(),
                        },
                    })
                    .collect(),
                backup_dir: inputs.backup_root.display().to_string(),
                scopes,
                strategy: strategy_text.clone(),
                plan_hash: rebuilt_hash,
                counts: summed_counts(&plans),
                steps: plans
                    .iter()
                    .flat_map(|plan| plan.steps.iter().cloned())
                    .map(step_view)
                    .collect(),
            };
            Ok(Some(view))
        })
        .await
        .map_err(|join| JobError::Migration {
            failure: MigrationApplyFailure::internal(format!("migration worker stopped: {join}")),
        })?;
        let view = match worker {
            Ok(Some(view)) => view,
            Ok(None) => return Ok(()),
            Err(failure) => return Err(JobError::Migration { failure }),
        };
        record_apply_result(view);
        progress.fraction(1.0, "Migration applied");
        Ok(())
    });
    let handle = app.bridge().spawn_job(job).map_err(CommandError::from)?;
    Ok(handle.id().clone())
}

/// Returns the recorded result of one completed migration apply job.
///
/// # Errors
/// [`CommandErrorCode::Conflict`] while the job is still running, [`CommandErrorCode::NotFound`]
/// when no result was recorded for the id.
#[tauri::command]
pub fn migrate_apply_result(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<MigrateApplyResultView, CommandError> {
    migrate_apply_result_inner(&state, &job_id)
}

/// Bridge-facing body of [`migrate_apply_result`], callable without a Tauri app.
///
/// # Errors
/// See [`migrate_apply_result`].
pub fn migrate_apply_result_inner(
    app: &AppState,
    job_id: &str,
) -> Result<MigrateApplyResultView, CommandError> {
    {
        let results = APPLY_RESULTS.lock().expect("apply result registry");
        if let Some(found) = results.iter().find(|result| result.job_id == job_id) {
            return Ok(found.clone());
        }
    }
    if app
        .bridge()
        .jobs()
        .iter()
        .any(|snapshot| snapshot.id.as_str() == job_id)
    {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "The migration apply job is still running.",
        ));
    }
    Err(CommandError::new(
        CommandErrorCode::NotFound,
        "No completed migration apply was recorded for this job.",
    ))
}

/// One undo readiness answer for the done screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OperationUndoCheckView {
    /// Operation the answer describes.
    pub operation_id: String,
    /// The operation row exists.
    pub known: bool,
    /// The operation already has an Undo child.
    pub already_undone: bool,
    /// The operation started before the undo window.
    pub expired: bool,
    /// Fixed undo window in days.
    pub window_days: i32,
    /// Unix epoch milliseconds when the window closes, when the operation exists.
    pub expires_at_unix_ms: Option<i64>,
    /// Paths that changed after the operation and refuse undo unless forced.
    pub conflicted_paths: Vec<String>,
}

/// Reads undo readiness for one recorded operation without writing anything.
///
/// # Errors
/// [`CommandErrorCode::Conflict`] when history cannot be read.
#[tauri::command]
pub fn operation_undo_check(
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<OperationUndoCheckView, CommandError> {
    operation_undo_check_inner(&state, &operation_id)
}

/// Bridge-facing body of [`operation_undo_check`], callable without a Tauri app.
///
/// # Errors
/// See [`operation_undo_check`].
pub fn operation_undo_check_inner(
    app: &AppState,
    operation_id: &str,
) -> Result<OperationUndoCheckView, CommandError> {
    let database = app.database().map_err(CommandError::unavailable)?;
    let readiness =
        at_migrate::undo::undo_readiness(database.connection(), operation_id).map_err(|error| {
            CommandError::new(
                CommandErrorCode::Conflict,
                format!("the operation history could not be read: {error}"),
            )
        })?;
    Ok(OperationUndoCheckView {
        operation_id: operation_id.to_owned(),
        known: readiness.known,
        already_undone: readiness.already_undone,
        expired: readiness.expired,
        window_days: i32::try_from(readiness.window_days).unwrap_or(i32::MAX),
        expires_at_unix_ms: readiness
            .expires_at_unix_seconds
            .and_then(|seconds| seconds.checked_mul(1000)),
        conflicted_paths: readiness
            .conflicted_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    })
}

#[cfg(test)]
mod plan_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
    use at_model::{AgentId, ItemKind, Scope};

    use super::MigrateResolutionView;

    /// Builds the single reviewed User-scope plan the unit tests inspect.
    ///
    /// The command path splits a selection by receiving scope; these tests exercise one scope, so
    /// they assert on the one plan it produces.
    fn plan_from_scan(
        scan: ScanReport,
        selected: &BTreeSet<PathBuf>,
        resolutions: &[MigrateResolutionView],
        strategy: at_migrate::plan::MigrationStrategy,
        store_root: PathBuf,
        backup_root: PathBuf,
        copy_agents: &BTreeSet<AgentId>,
    ) -> Result<
        (
            at_migrate::plan::MigrationPlan,
            Vec<at_migrate::plan::DryRunRow>,
        ),
        String,
    > {
        let stores = at_migrate::plan::ScopeStores::single(Scope::User, store_root);
        let plans = super::plans_from_scan(
            scan,
            selected,
            resolutions,
            strategy,
            &stores,
            None,
            backup_root,
            copy_agents,
        )?;
        let plan = plans
            .into_iter()
            .next()
            .ok_or_else(|| "the selection produced no plan".to_owned())?;
        let dry_run = at_migrate::plan::dry_run(&plan);
        Ok((plan, dry_run))
    }

    fn item(name: &str, path: &str, agent: AgentId, hash: char) -> ScanItem {
        ScanItem {
            kind: ItemKind::Skill,
            name: name.to_owned(),
            agents: BTreeSet::from([agent]),
            path: PathBuf::from(path),
            scope: Scope::User,
            content_hash: Some(hash.to_string().repeat(64)),
            normalized_hash: Some(hash.to_string().repeat(64)),
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
        }
    }

    fn temp_store() -> tempfile::TempDir {
        let store = tempfile::tempdir().expect("temporary store");
        at_store::layout::init_store(Scope::User, store.path().join("store"))
            .expect("initialise store");
        store
    }

    #[test]
    fn reviewed_choices_reach_the_plan_and_drop_the_unselected_copy() {
        let store = temp_store();
        let first = store.path().join("skills/a/SKILL.md");
        let second = store.path().join("skills/b/SKILL.md");
        std::fs::create_dir_all(first.parent().expect("dir")).expect("mkdir a");
        std::fs::create_dir_all(second.parent().expect("dir")).expect("mkdir b");
        std::fs::write(&first, b"alpha").expect("write a");
        std::fs::write(&second, b"beta").expect("write b");
        let scan = ScanReport {
            roots: Vec::new(),
            items: vec![
                item(
                    "review",
                    &first.display().to_string(),
                    AgentId::ClaudeCode,
                    'a',
                ),
                item("review", &second.display().to_string(), AgentId::Codex, 'b'),
            ],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let selected = BTreeSet::from([first.clone(), second.clone()]);
        let resolutions = vec![MigrateResolutionView {
            name: "review".to_owned(),
            choice: "keep_a".to_owned(),
            names: Vec::new(),
        }];
        let (plan, dry_run) = plan_from_scan(
            scan,
            &selected,
            &resolutions,
            at_migrate::plan::MigrationStrategy::LeaveAndWatch,
            store.path().join("store"),
            store.path().join("backups"),
            &BTreeSet::new(),
        )
        .expect("plan");
        assert_eq!(plan.counts.items, 1, "Keep A selects one copy");
        assert!(
            plan.steps
                .iter()
                .filter_map(|step| step.source.as_ref())
                .all(|path| path != &second),
            "the dropped copy must not appear in the plan"
        );
        assert_eq!(dry_run.len(), plan.steps.len());
    }

    #[test]
    fn missing_resolution_and_invalid_keep_both_names_are_refused() {
        let store = temp_store();
        let first = store.path().join("skills/a/SKILL.md");
        let second = store.path().join("skills/b/SKILL.md");
        std::fs::create_dir_all(first.parent().expect("dir")).expect("mkdir a");
        std::fs::create_dir_all(second.parent().expect("dir")).expect("mkdir b");
        std::fs::write(&first, b"alpha").expect("write a");
        std::fs::write(&second, b"beta").expect("write b");
        let scan = ScanReport {
            roots: Vec::new(),
            items: vec![
                item(
                    "review",
                    &first.display().to_string(),
                    AgentId::ClaudeCode,
                    'a',
                ),
                item("review", &second.display().to_string(), AgentId::Codex, 'b'),
            ],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let selected = BTreeSet::from([first, second]);
        let unresolved = plan_from_scan(
            scan.clone(),
            &selected,
            &[],
            at_migrate::plan::MigrationStrategy::LeaveAndWatch,
            store.path().join("store"),
            store.path().join("backups"),
            &BTreeSet::new(),
        )
        .expect_err("an unresolved conflict blocks the plan");
        assert!(unresolved.contains("requires an explicit resolution"));

        let invalid = plan_from_scan(
            scan,
            &selected,
            &[MigrateResolutionView {
                name: "review".to_owned(),
                choice: "keep_both".to_owned(),
                names: vec!["same name".to_owned(), "same name".to_owned()],
            }],
            at_migrate::plan::MigrationStrategy::LeaveAndWatch,
            store.path().join("store"),
            store.path().join("backups"),
            &BTreeSet::new(),
        )
        .expect_err("invalid keep-both names block the plan");
        assert!(invalid.contains("KeepBoth"));
    }

    #[test]
    fn copy_agents_produce_copy_steps_instead_of_links() {
        let store = temp_store();
        let first = store.path().join("skills/a/SKILL.md");
        std::fs::create_dir_all(first.parent().expect("dir")).expect("mkdir a");
        std::fs::write(&first, b"alpha").expect("write a");
        let scan = ScanReport {
            roots: Vec::new(),
            items: vec![item(
                "solo",
                &first.display().to_string(),
                AgentId::ClaudeCode,
                'a',
            )],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let selected = BTreeSet::from([first]);
        let (plan, _) = plan_from_scan(
            scan,
            &selected,
            &[],
            at_migrate::plan::MigrationStrategy::ReplaceWithLinks,
            store.path().join("store"),
            store.path().join("backups"),
            &BTreeSet::from([AgentId::ClaudeCode]),
        )
        .expect("plan");
        assert_eq!(plan.counts.links_written, 0);
        assert!(
            plan.steps.iter().any(|step| {
                matches!(step.action, at_migrate::plan::MigrationAction::WriteCopy)
            })
        );
        let _ = BTreeMap::<String, String>::new();
    }

    #[test]
    fn project_scope_items_without_a_selected_project_are_refused() {
        let store = temp_store();
        let source = store.path().join("skills/a/SKILL.md");
        std::fs::create_dir_all(source.parent().expect("dir")).expect("mkdir a");
        std::fs::write(&source, b"alpha").expect("write a");
        let mut project_item = item(
            "review",
            &source.display().to_string(),
            AgentId::ClaudeCode,
            'a',
        );
        project_item.scope = Scope::Project;
        let scan = ScanReport {
            roots: Vec::new(),
            items: vec![project_item],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let selected = BTreeSet::from([source]);
        let mut stores =
            at_migrate::plan::ScopeStores::single(Scope::User, store.path().join("store"));
        stores.insert(Scope::Project, store.path().join("project-store"));
        let error = super::plans_from_scan(
            scan,
            &selected,
            &[],
            at_migrate::plan::MigrationStrategy::ReplaceWithLinks,
            &stores,
            None,
            store.path().join("backups"),
            &BTreeSet::new(),
        )
        .expect_err("a project-scope item without a selected project must be refused");
        assert!(
            error.contains("Select a project and rebuild the plan"),
            "the refusal tells the user what to do: {error}"
        );
        assert!(
            !store.path().join("project-store").exists(),
            "a refused plan writes nothing"
        );
    }
}

#[cfg(test)]
mod scan_view_tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use at_migrate::scan::{
        Ownership, ParseStatus, ScanItem, ScanReport, ScanRoot, ScanRootStatus, ScanWarning,
    };
    use at_model::{AgentId, ItemKind, Scope};

    use super::{MigrateScanRootStatus, ownership_state, scan_view};

    fn item(name: &str, ownership: Ownership) -> ScanItem {
        ScanItem {
            kind: ItemKind::Rule,
            name: name.to_owned(),
            agents: BTreeSet::from([AgentId::ClaudeCode]),
            path: PathBuf::from(format!("/home/{name}.md")),
            scope: Scope::User,
            content_hash: Some("a".repeat(64)),
            normalized_hash: Some("a".repeat(64)),
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership,
        }
    }

    #[test]
    fn every_ownership_has_one_authoritative_state_label() {
        assert_eq!(ownership_state(Ownership::Managed), "in_agentuna");
        assert_eq!(ownership_state(Ownership::Conflict), "conflict");
        assert_eq!(ownership_state(Ownership::NotInAgenTuna), "not_in_agentuna");

        let report = ScanReport {
            roots: Vec::new(),
            items: vec![
                item("managed", Ownership::Managed),
                item("conflict", Ownership::Conflict),
                item("fresh", Ownership::NotInAgenTuna),
            ],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let view = scan_view(report);
        let states = view
            .items
            .iter()
            .map(|row| (row.name.as_str(), row.state.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(states.get("managed"), Some(&"in_agentuna"));
        assert_eq!(states.get("conflict"), Some(&"conflict"));
        assert_eq!(states.get("fresh"), Some(&"not_in_agentuna"));
    }

    #[test]
    fn root_outcomes_are_projected_even_when_they_have_no_items() {
        let root = |path: &str, status, reason: Option<&str>| ScanRoot {
            path: PathBuf::from(path),
            kind: ItemKind::Skill,
            agents: BTreeSet::from([AgentId::Codex, AgentId::Omp]),
            status,
            unreadable_reason: reason.map(str::to_owned),
            found_items: 0,
            new_items: 0,
            skipped_items: 0,
            in_sync: 0,
        };
        let view = scan_view(ScanReport {
            roots: vec![
                root("/empty", ScanRootStatus::Empty, None),
                root("/missing", ScanRootStatus::NotPresent, None),
                root(
                    "/denied",
                    ScanRootStatus::Unreadable,
                    Some("permission denied"),
                ),
            ],
            items: vec![item("kept", Ownership::NotInAgenTuna)],
            partial: true,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        });
        assert_eq!(view.roots.len(), 3);
        assert_eq!(view.roots[0].status, MigrateScanRootStatus::Empty);
        assert_eq!(view.roots[0].new_items, 0);
        assert_eq!(view.roots[0].agents, vec!["codex", "omp"]);
        assert_eq!(view.roots[1].status, MigrateScanRootStatus::NotPresent);
        assert_eq!(view.roots[2].status, MigrateScanRootStatus::Unreadable);
        assert_eq!(
            view.roots[2].unreadable_reason.as_deref(),
            Some("permission denied")
        );
        assert_eq!(view.items.len(), 1);
    }

    #[test]
    fn a_warning_keeps_the_path_that_failed() {
        let path = PathBuf::from("/home/.claude.json");
        let report = ScanReport {
            roots: Vec::new(),
            items: vec![item("kept", Ownership::NotInAgenTuna)],
            partial: true,
            skipped_roots: Vec::new(),
            warnings: vec![ScanWarning {
                path: path.clone(),
                kind: ItemKind::McpServer,
                agent: AgentId::ClaudeCode,
                scope: Scope::User,
                message: "permission denied".to_owned(),
            }],
        };
        let view = scan_view(report);
        assert!(view.partial);
        assert_eq!(view.items.len(), 1, "found rows stay usable");
        assert_eq!(view.warnings.len(), 1);
        assert_eq!(view.warnings[0].path, path.display().to_string());
        assert_eq!(view.warnings[0].kind, "mcp_server");
        assert_eq!(view.warnings[0].agent, "claude_code");
    }
}
