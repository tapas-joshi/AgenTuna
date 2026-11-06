//! Typed native migration scan, plan, apply and undo commands.
use super::{
    bridge::{JobError, JobSpec},
    commands::{CommandError, CommandErrorCode},
    state::AppState,
};
use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport, ScanRequest};
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
    /// Managed, conflict, not-in-agentuna or unknown ownership state.
    pub ownership: String,
}

/// One native folder or document that produced scan rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigrateScanRootView {
    /// Native folder or document path.
    pub path: String,
    /// Agents that read this root.
    pub agents: Vec<String>,
    /// Rows that would be moved into the store.
    pub new_items: i64,
    /// Rows that already point at the store.
    pub in_sync: i64,
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
    /// Scanned folders and documents with their item counts.
    pub roots: Vec<MigrateScanRootView>,
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
    let roots = scan_roots(&report);
    Ok(MigrateScanView {
        items: report.items.into_iter().map(item_view).collect(),
        partial: report.partial,
        skipped_roots: report
            .skipped_roots
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        roots,
    })
}

/// Groups scan rows by the native folder or document that produced them.
fn scan_roots(report: &ScanReport) -> Vec<MigrateScanRootView> {
    let mut grouped: BTreeMap<PathBuf, (std::collections::BTreeSet<String>, i64, i64)> =
        BTreeMap::new();
    for item in &report.items {
        let root = match item.kind {
            at_model::ItemKind::Skill => item
                .path
                .parent()
                .map_or_else(|| item.path.clone(), std::path::Path::to_path_buf),
            _ => item.path.clone(),
        };
        let entry = grouped.entry(root).or_default();
        for agent in &item.agents {
            entry.0.insert(agent.to_string());
        }
        if item.ownership == Ownership::Managed {
            entry.2 += 1;
        } else {
            entry.1 += 1;
        }
    }
    grouped
        .into_iter()
        .map(|(path, (agents, new_items, in_sync))| MigrateScanRootView {
            path: path.display().to_string(),
            agents: agents.into_iter().collect(),
            new_items,
            in_sync,
        })
        .collect()
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

/// A plan built from the scan and the user's conflict choices, plus its dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct MigratePlanView {
    /// Stable plan id.
    pub plan_id: String,
    /// SHA-256 binding the reviewed steps and strategy.
    pub plan_hash: String,
    /// Store scope receiving the items.
    pub scope: String,
    /// Selected strategy.
    pub strategy: String,
    /// Summary counts.
    pub counts: MigratePlanCountsView,
    /// Ordered steps.
    pub steps: Vec<MigratePlanStepView>,
    /// Files rechecked for the dry run.
    pub files_checked: i32,
}

/// Builds one reviewed plan from a scan and the user's conflict choices.
///
/// Retains only selected paths, detects conflicts among them, applies the explicit resolutions and
/// revalidates the resulting plan so the returned dry run is the exact write list.
// One argument per independent input of the reviewed plan; the two call sites build them inline.
#[allow(clippy::too_many_arguments)]
fn plan_from_scan(
    mut scan: ScanReport,
    selected_paths: &std::collections::BTreeSet<PathBuf>,
    resolutions: &[MigrateResolutionView],
    strategy: at_migrate::plan::MigrationStrategy,
    scope: at_model::Scope,
    store_root: PathBuf,
    backup_root: PathBuf,
    copy_agents: &std::collections::BTreeSet<at_model::AgentId>,
) -> Result<
    (
        at_migrate::plan::MigrationPlan,
        Vec<at_migrate::plan::DryRunRow>,
    ),
    String,
> {
    scan.items
        .retain(|item| selected_paths.contains(&item.path));
    let conflicts = at_migrate::conflicts::detect_conflicts(&scan);
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
    let plan = at_migrate::plan::build_plan(at_migrate::plan::BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &choices,
        selected_paths,
        store_root,
        copy_agents,
        strategy,
        scope,
        backup_root,
    })
    .map_err(|error| error.to_string())?;
    at_migrate::plan::validate_plan(&plan).map_err(|error| error.to_string())?;
    let dry_run = at_migrate::plan::dry_run(&plan);
    Ok((plan, dry_run))
}

/// Builds the migration plan from a fresh scan and the reviewed conflict choices.
///
/// The scan is repeated so the plan binds to current native bytes; the returned dry run is the
/// exact ordered write list the preview step shows. Nothing is written.
///
/// # Errors
/// Returns a typed command error for unknown projects, an unknown strategy or a refused plan.
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
    let built = tokio::task::spawn_blocking(move || {
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
        .map_err(|_| "migration scan failed".to_owned())?;
        plan_from_scan(
            scan,
            &selected_paths,
            &resolutions,
            strategy,
            inputs.scope,
            inputs.store_root,
            inputs.backup_root,
            &inputs.copy_agents,
        )
    })
    .await
    .map_err(|_| CommandError::internal("migration plan worker stopped unexpectedly"))?
    .map_err(|error| CommandError::invalid_argument("selected_paths", &error))?;
    let (plan, dry_run) = built;
    Ok(MigratePlanView {
        plan_id: plan.id,
        plan_hash: plan.plan_hash,
        scope: plan.scope.to_string(),
        strategy: match plan.strategy {
            at_migrate::plan::MigrationStrategy::ReplaceWithLinks => {
                String::from("replace_with_links")
            }
            at_migrate::plan::MigrationStrategy::LeaveAndWatch => String::from("leave_and_watch"),
            _ => String::from("unknown"),
        },
        counts: MigratePlanCountsView {
            items: i32::try_from(plan.counts.items).unwrap_or(i32::MAX),
            files_created: i32::try_from(plan.counts.files_created).unwrap_or(i32::MAX),
            files_backed_up: i32::try_from(plan.counts.files_backed_up).unwrap_or(i32::MAX),
            links_written: i32::try_from(plan.counts.links_written).unwrap_or(i32::MAX),
            files_left_in_place: i32::try_from(plan.counts.files_left_in_place).unwrap_or(i32::MAX),
        },
        files_checked: i32::try_from(dry_run.len()).unwrap_or(i32::MAX),
        steps: plan
            .steps
            .into_iter()
            .map(|step| MigratePlanStepView {
                seq: step.seq,
                action: format!("{:?}", step.action).to_lowercase(),
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
            })
            .collect(),
    })
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

/// Result of a completed migration apply job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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
    /// Store scope that received the items.
    pub scope: String,
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
    scope: at_model::Scope,
    store_root: PathBuf,
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
    let scope = if project.is_some() {
        at_model::Scope::Project
    } else {
        at_model::Scope::User
    };
    let paths = super::paths::resolve_command_paths(app, project.as_deref())
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    let home = paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let layout = at_store::layout::StoreLayout::for_scope(&paths, scope)
        .map_err(|_| CommandError::unavailable("the migration store is unavailable"))?;
    let store_root = layout.root().to_path_buf();
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
        scope,
        store_root,
        backup_root,
        roots,
        store_items,
        copy_agents,
    })
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
                plan_from_scan(
                    scan,
                    &selected_paths,
                    &resolutions,
                    strategy,
                    inputs.scope,
                    inputs.store_root.clone(),
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
            let (plan, _dry_run) = match built {
                Ok(built) => built,
                Err(failure) => return Err(failure),
            };
            if plan.plan_hash != reviewed_hash {
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
            worker_progress.fraction(0.6, "Backing up originals and writing reviewed changes");
            let apply = {
                let mut database = database.lock().map_err(|_| {
                    MigrationApplyFailure::internal("the index database is unavailable")
                })?;
                let mut context = at_migrate::apply::ApplyPlanContext {
                    store_root: &inputs.store_root,
                    backup_root: &inputs.backup_root,
                    authorized_roots: &authorized,
                    database: &mut database,
                };
                at_migrate::apply::apply_plan(&plan, &mut context).map_err(apply_failure)
            };
            let outcome = match apply {
                Ok(outcome) => outcome,
                Err(failure) => return Err(failure),
            };
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
                scope: plan.scope.to_string(),
                strategy: strategy_text.clone(),
                plan_hash: plan.plan_hash,
                counts: MigratePlanCountsView {
                    items: i32::try_from(plan.counts.items).unwrap_or(i32::MAX),
                    files_created: i32::try_from(plan.counts.files_created).unwrap_or(i32::MAX),
                    files_backed_up: i32::try_from(plan.counts.files_backed_up).unwrap_or(i32::MAX),
                    links_written: i32::try_from(plan.counts.links_written).unwrap_or(i32::MAX),
                    files_left_in_place: i32::try_from(plan.counts.files_left_in_place)
                        .unwrap_or(i32::MAX),
                },
                steps: plan
                    .steps
                    .iter()
                    .map(|step| MigratePlanStepView {
                        seq: step.seq,
                        action: format!("{:?}", step.action).to_lowercase(),
                        item_name: step.item_name.clone(),
                        source: step.source.as_ref().map(|path| path.display().to_string()),
                        destination: step
                            .destination
                            .as_ref()
                            .map(|path| path.display().to_string()),
                        backup_path: step
                            .backup_path
                            .as_ref()
                            .map(|path| path.display().to_string()),
                        bytes: step.bytes,
                        agents: step.agents.iter().map(|agent| agent.to_string()).collect(),
                    })
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

    use super::{MigrateResolutionView, plan_from_scan};

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
            Scope::User,
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
        };
        let selected = BTreeSet::from([first, second]);
        let unresolved = plan_from_scan(
            scan.clone(),
            &selected,
            &[],
            at_migrate::plan::MigrationStrategy::LeaveAndWatch,
            Scope::User,
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
            Scope::User,
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
            items: vec![item(
                "solo",
                &first.display().to_string(),
                AgentId::ClaudeCode,
                'a',
            )],
            partial: false,
            skipped_roots: Vec::new(),
        };
        let selected = BTreeSet::from([first]);
        let (plan, _) = plan_from_scan(
            scan,
            &selected,
            &[],
            at_migrate::plan::MigrationStrategy::ReplaceWithLinks,
            Scope::User,
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
}
