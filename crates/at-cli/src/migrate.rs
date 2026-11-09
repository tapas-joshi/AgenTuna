//! `agentuna migrate` (TC-030): scan, plan, apply, undo and status over the shared
//! at-migrate engine.
//!
//! Every subcommand drives the same scan/plan/apply/undo pipeline the GUI uses; the
//! CLI adds only argument parsing, the JSON envelope and exit codes. Non-interactive
//! callers refuse to guess: a content conflict without an explicit `--keep-a`,
//! `--keep-b` or `--keep-both` choice fails with `needs_resolution` and writes
//! nothing, and every write-shaped subcommand honours `--dry-run` and requires
//! `--yes` before files move.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_db::ops;
use at_model::{AgentId, Scope};
use at_store::layout::StoreLayout;
use at_store::manifest::StoreManifest;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::{CommandOutput, Warning};
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths, resolve_paths_for_project};

/// Strategy wire names accepted by `migrate plan --strategy`.
const STRATEGY_LINKS: &str = "links";
/// Strategy wire names accepted by `migrate plan --strategy`.
const STRATEGY_WATCH: &str = "watch";

/// `agentuna migrate`: the migration pipeline over native agent items.
pub struct MigrateGroup;

impl CommandGroup for MigrateGroup {
    fn name(&self) -> &'static str {
        "migrate"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("migrate")
                .about("Migrate native agent items into the AgenTuna store")
                .subcommand_required(true)
                .subcommand(
                    Command::new("scan")
                        .about("List native items with kind, scope, path and store state"),
                )
                .subcommand(
                    Command::new("plan")
                        .about("Build a reviewed migration plan from a fresh scan")
                        .arg(
                            Arg::new("strategy")
                                .long("strategy")
                                .value_name("STRATEGY")
                                .default_value(STRATEGY_LINKS)
                                .help("links replaces originals with links; watch leaves them in place"),
                        )
                        .arg(
                            Arg::new("select")
                                .long("select")
                                .value_name("PATH")
                                .action(ArgAction::Append)
                                .help("Native path to include; repeatable. Default: every unmanaged item"),
                        )
                        .arg(
                            Arg::new("keep_a")
                                .long("keep-a")
                                .value_name("CONFLICT")
                                .action(ArgAction::Append)
                                .help("Resolve CONFLICT by keeping the first sorted copy; repeatable"),
                        )
                        .arg(
                            Arg::new("keep_b")
                                .long("keep-b")
                                .value_name("CONFLICT")
                                .action(ArgAction::Append)
                                .help("Resolve CONFLICT by keeping the second sorted copy; repeatable"),
                        )
                        .arg(
                            Arg::new("keep_both")
                                .long("keep-both")
                                .value_name("CONFLICT")
                                .action(ArgAction::Append)
                                .help("Resolve CONFLICT by keeping both copies under derived names; repeatable"),
                        ),
                )
                .subcommand(
                    Command::new("apply")
                        .about("Apply a persisted plan; --dry-run lists the exact file operations")
                        .arg(
                            Arg::new("plan")
                                .long("plan")
                                .value_name("PLAN_ID")
                                .help(
                                    "Plan id printed by `migrate plan`; without it the refusal \
                                     lists the ids that are persisted",
                                ),
                        ),
                )
                .subcommand(
                    Command::new("undo")
                        .about("Undo one recorded operation from its backups")
                        .arg(
                            Arg::new("operation")
                                .value_name("OPERATION_ID")
                                .required(true)
                                .help("Operation id from `migrate status` or `migrate apply`"),
                        )
                        .arg(
                            Arg::new("force")
                                .long("force")
                                .action(ArgAction::SetTrue)
                                .help("Act as confirmation; safety refusals still apply"),
                        ),
                )
                .subcommand(
                    Command::new("status")
                        .about("Show migration counts, pending plans and recent operations"),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let (name, sub) = matches
            .subcommand()
            .ok_or_else(|| CliError::internal("migrate dispatch without a subcommand"))?;
        let writes = name == "undo" || (matches!(name, "plan" | "apply") && !globals.dry_run);
        if writes && globals.read_only {
            let access = CliAccess::for_command(globals, true)?;
            access.ensure_writable()?;
        }
        match name {
            "scan" => scan(globals),
            "plan" => plan(globals, sub),
            "apply" => apply(globals, sub),
            "undo" => undo(globals, sub),
            "status" => status(globals),
            other => Err(CliError::internal(format!(
                "migrate has no `{other}` subcommand"
            ))),
        }
    }
}

/// Store roots and canonical inventory used to classify native ownership.
///
/// Mirrors the GUI helper: every resolvable scope contributes its manifest items so the
/// scanner can mark native copies as managed, conflicting or new.
fn store_inventory(
    paths: &at_store::paths::AppPaths,
) -> Result<(Vec<PathBuf>, Vec<at_migrate::scan::StoreItem>), CliError> {
    let mut roots = Vec::new();
    let mut items = Vec::new();
    for scope in [Scope::Global, Scope::User, Scope::Project, Scope::Local] {
        let layout = match StoreLayout::for_scope(paths, scope) {
            Ok(layout) => layout,
            Err(_) if matches!(scope, Scope::Project | Scope::Local) => continue,
            Err(_) => {
                return Err(CliError::new(
                    ErrorCode::NotFound,
                    "the AgenTuna store paths are unavailable",
                    "run `agentuna doctor` to initialize the store",
                ));
            }
        };
        roots.push(layout.root().to_path_buf());
        match StoreManifest::load(&layout) {
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
                return Err(CliError::new(
                    ErrorCode::Conflict,
                    "could not read a canonical store manifest",
                    "run `agentuna doctor` to inspect the store",
                ));
            }
        }
    }
    Ok((roots, items))
}

/// Runs one native scan against the injected roots.
pub(crate) fn run_scan(
    paths: &at_store::paths::AppPaths,
) -> Result<at_migrate::scan::ScanReport, CliError> {
    run_scan_with_inventory(paths).map(|(report, _)| report)
}

/// Runs one native scan and returns the store inventory the scan compared against.
///
/// Conflict analysis and keep-both naming need the same inventory, so callers planning a
/// migration take both values from one call.
pub(crate) fn run_scan_with_inventory(
    paths: &at_store::paths::AppPaths,
) -> Result<
    (
        at_migrate::scan::ScanReport,
        Vec<at_migrate::scan::StoreItem>,
    ),
    CliError,
> {
    let home = paths.home_dir().ok_or_else(|| {
        CliError::new(
            ErrorCode::NotFound,
            "no home directory is available",
            "set HOME or AGENTUNA_HOME to a readable directory",
        )
    })?;
    let project = paths
        .project_store()
        .and_then(|store| store.parent())
        .map(Path::to_path_buf);
    let (roots, store_items) = store_inventory(paths)?;
    let environment = AgentEnvironment {
        home: home.to_path_buf(),
        project,
        variables: BTreeMap::new(),
    };
    let adapters = AgentAdapterRegistry::register();
    let report = at_migrate::scan::scan(at_migrate::scan::ScanRequest {
        environment: &environment,
        adapters: &adapters,
        store_roots: &roots,
        store_items: &store_items,
    })
    .map_err(|error| {
        CliError::new(
            ErrorCode::Internal,
            format!("native scan failed: {error}"),
            "check the agent folders are readable, then rerun the command",
        )
    })?;
    Ok((report, store_items))
}

/// Explicit target scope for every item, from `--scope`.
///
/// Without `--scope` each item is migrated into the store of the scope it was discovered in.
/// `--scope` is the explicit override that sends the whole selection into one store.
fn target_scope(
    globals: &GlobalArgs,
    paths: &at_store::paths::AppPaths,
) -> Result<Option<Scope>, CliError> {
    match globals.scope() {
        Some(Scope::Project) | Some(Scope::Local) => {
            if paths.project_store().is_none() {
                return Err(CliError::new(
                    ErrorCode::Validation,
                    "--scope project needs a project: pass --project with a registered directory",
                    "pass --project, or drop --scope to migrate each item into its own scope",
                ));
            }
            Ok(Some(Scope::Project))
        }
        Some(scope) => Ok(Some(scope)),
        None => Ok(None),
    }
}

/// Registered project id for the selected project root, when the index knows it.
///
/// A Project or Local plan requires it: without a registered project the items have no project
/// identity to record, so planning refuses instead of writing them into the User store.
fn registered_project_id(paths: &at_store::paths::AppPaths) -> Option<String> {
    let database = at_db::ReadOnlyDatabase::open(paths.db_path()).ok()?;
    let root = paths.project_store()?.parent()?;
    at_db::projects::find_project_by_path(database.connection(), root)
        .ok()
        .flatten()
        .map(|project| project.id)
}

/// Store root per scope that native discovery can report.
///
/// Project and Local need a selected project; without one they stay out of the map, so planning
/// an item discovered there fails with a message naming the missing project.
fn migration_stores(
    paths: &at_store::paths::AppPaths,
) -> Result<at_migrate::plan::ScopeStores, CliError> {
    let mut stores = at_migrate::plan::ScopeStores::new();
    for scope in [Scope::User, Scope::Project, Scope::Local] {
        match StoreLayout::for_scope(paths, scope) {
            Ok(layout) => stores.insert(scope, layout.root().to_path_buf()),
            Err(_) if matches!(scope, Scope::Project | Scope::Local) => {}
            Err(_) => {
                return Err(CliError::new(
                    ErrorCode::NotFound,
                    "the AgenTuna store paths are unavailable",
                    "run `agentuna doctor` to initialize the store",
                ));
            }
        }
    }
    Ok(stores)
}

/// JSON record for one scanned native item.
fn scan_item_record(item: &at_migrate::scan::ScanItem) -> Value {
    json!({
        "kind": item.kind.to_string(),
        "name": item.name,
        "agents": item.agents.iter().map(|agent| agent.to_string()).collect::<Vec<_>>(),
        "path": item.path.display().to_string(),
        "scope": item.scope.to_string(),
        "state": match item.ownership {
            at_migrate::scan::Ownership::Managed => "managed",
            at_migrate::scan::Ownership::NotInAgenTuna => "not_in_agentuna",
            at_migrate::scan::Ownership::Conflict => "conflict",
            _ => "unknown",
        },
        "content_hash": item.content_hash,
        "link_target": item.link_target.as_ref().map(|target| target.display().to_string()),
        "resolved": item.resolved.as_ref().map(|target| target.display().to_string()),
        "discovery": item.discovery.as_str(),
        "store_link": item.store_link,
        "parse_error": match &item.parse_status {
            at_migrate::scan::ParseStatus::Parsed => None,
            at_migrate::scan::ParseStatus::Error(message) => Some(message.clone()),
            _ => Some("unrecognized scan status".to_owned()),
        },
    })
}

/// Renders the scanned items as the human table.
fn scan_human(report: &at_migrate::scan::ScanReport) -> String {
    let mut out = String::new();
    let managed = report
        .items
        .iter()
        .filter(|item| item.ownership == at_migrate::scan::Ownership::Managed)
        .count();
    let conflicting = report
        .items
        .iter()
        .filter(|item| item.ownership == at_migrate::scan::Ownership::Conflict)
        .count();
    let fresh = report.items.len() - managed - conflicting;
    let _ = std::fmt::Write::write_fmt(
        &mut out,
        format_args!(
            "{} native item(s): {} managed, {} conflicting, {} new\n",
            report.items.len(),
            managed,
            conflicting,
            fresh
        ),
    );
    for item in &report.items {
        let state = match item.ownership {
            at_migrate::scan::Ownership::Managed => "managed",
            at_migrate::scan::Ownership::NotInAgenTuna => "new",
            at_migrate::scan::Ownership::Conflict => "conflict",
            _ => "unknown",
        };
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "  {:<9} {:<6} {:<8} {}\n",
                item.kind.to_string(),
                item.scope.to_string(),
                state,
                item.path.display()
            ),
        );
    }
    out
}

/// `migrate scan`: discovery over the injected roots.
///
/// Native files are only read; the derived agent registry is refreshed first so the rest of
/// the migration flow can rely on it.
fn scan(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    crate::runtime::refresh_agent_registry_if_needed(globals)?;
    let paths = resolve_paths(globals)?;
    let report = run_scan(&paths)?;
    Ok(CommandOutput {
        data: json!({
            "items": report.items.iter().map(scan_item_record).collect::<Vec<_>>(),
            "partial": report.partial,
            "skipped_roots": report
                .skipped_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        }),
        human: scan_human(&report),
        warnings: Vec::new(),
        warnings_in_report: false,
    })
}

/// Parses the keep flags into resolutions keyed by normalized conflict name.
///
/// `--keep-both` derives two free names from the store and every agent folder the scan saw,
/// matching the GUI's derived names; an unknown conflict name is refused with
/// `needs_resolution` because the caller asked to resolve a group that does not exist.
fn parse_resolutions(
    sub: &clap::ArgMatches,
    conflicts: &at_migrate::conflicts::ConflictSet,
    scan: &at_migrate::scan::ScanReport,
    store_items: &[at_migrate::scan::StoreItem],
) -> Result<BTreeMap<String, at_migrate::plan::ConflictResolution>, CliError> {
    let keep_a = sub
        .get_many::<String>("keep_a")
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let keep_b = sub
        .get_many::<String>("keep_b")
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let keep_both = sub
        .get_many::<String>("keep_both")
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut resolutions = BTreeMap::new();
    let mut insert = |conflict: &str,
                      resolution: at_migrate::plan::ConflictResolution|
     -> Result<(), CliError> {
        let normalized = conflict.trim().to_ascii_lowercase();
        if !conflicts
            .conflicts
            .iter()
            .any(|group| group.name == normalized)
        {
            let available = conflicts
                .conflicts
                .iter()
                .map(|group| group.name.clone())
                .collect::<Vec<_>>();
            let names = if available.is_empty() {
                "the current scan reported no conflicts".to_owned()
            } else {
                format!("available conflicts: {}", available.join(", "))
            };
            return Err(CliError::new(
                ErrorCode::NeedsResolution,
                format!("no conflict named `{conflict}` exists in the current scan"),
                format!(
                    "{names}; rerun `agentuna migrate plan --keep-both <name>` with one of them"
                ),
            )
            .with_details(json!({
                "conflict": conflict.trim(),
                "available": available,
            })));
        }
        resolutions.insert(normalized, resolution);
        Ok(())
    };
    for conflict in keep_a {
        insert(conflict, at_migrate::plan::ConflictResolution::KeepA)?;
    }
    for conflict in keep_b {
        insert(conflict, at_migrate::plan::ConflictResolution::KeepB)?;
    }
    for conflict in keep_both {
        let group = conflicts
            .conflicts
            .iter()
            .find(|candidate| candidate.name == conflict.trim().to_ascii_lowercase());
        let names = group.and_then(|group| {
            let agent = group
                .copies
                .iter()
                .find_map(|copy| copy.agent.as_ref())
                .cloned()?;
            let occupied = at_migrate::conflicts::occupied_names(scan, store_items, group.kind);
            at_migrate::conflicts::keep_both_names(&group.name, &agent, &occupied)
        });
        let Some((first, second)) = names else {
            return Err(CliError::new(
                ErrorCode::NeedsResolution,
                format!("cannot derive two free names for conflict `{conflict}`"),
                "rename one of the copies first, then rerun the plan",
            ));
        };
        insert(
            conflict,
            at_migrate::plan::ConflictResolution::KeepBoth {
                names: vec![first, second],
            },
        )?;
    }
    Ok(resolutions)
}

/// Agents whose deployments are manifest-bearing copies, from the stored link modes.
///
/// Defaults to links when the index is absent so planning never needs one.
fn copy_agents(paths: &at_store::paths::AppPaths) -> BTreeSet<AgentId> {
    let Ok(database) = at_db::ReadOnlyDatabase::open(paths.db_path()) else {
        return BTreeSet::new();
    };
    let settings = match at_db::settings::load(database.connection()) {
        Ok(settings) => settings,
        Err(_) => return BTreeSet::new(),
    };
    let agents = match at_db::agents::list_agents_on_mac(database.connection()) {
        Ok(agents) => agents,
        Err(_) => return BTreeSet::new(),
    };
    agents
        .into_iter()
        .filter(|agent| settings.link_mode(&agent.id) == at_model::LinkMode::Copy)
        .map(|agent| agent.id)
        .collect()
}

/// `migrate plan`: build (and persist, unless `--dry-run`) a reviewed plan.
fn plan(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let target = target_scope(globals, &paths)?;
    let strategy = match sub
        .get_one::<String>("strategy")
        .map(String::as_str)
        .unwrap_or(STRATEGY_LINKS)
    {
        STRATEGY_LINKS => at_migrate::plan::MigrationStrategy::ReplaceWithLinks,
        STRATEGY_WATCH => at_migrate::plan::MigrationStrategy::LeaveAndWatch,
        other => {
            return Err(CliError::new(
                ErrorCode::Validation,
                format!("unknown strategy `{other}`"),
                "use --strategy links or --strategy watch",
            ));
        }
    };
    let stores = migration_stores(&paths)?;
    let project_id = registered_project_id(&paths);
    let backup_root = paths.backup_dir().join("migration");

    let (mut report, store_items) = run_scan_with_inventory(&paths)?;
    let selected = match sub.get_many::<String>("select") {
        Some(paths_arg) => {
            let wanted: BTreeSet<String> = paths_arg.map(|path| path.as_str().to_owned()).collect();
            report
                .items
                .retain(|item| wanted.contains(&item.path.display().to_string()));
            report
                .items
                .iter()
                .map(|item| item.path.clone())
                .collect::<BTreeSet<_>>()
        }
        None => report
            .items
            .iter()
            .filter(|item| item.ownership != at_migrate::scan::Ownership::Managed)
            .map(|item| item.path.clone())
            .collect::<BTreeSet<_>>(),
    };
    if selected.is_empty() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "no native items are selected for migration",
            "run `agentuna migrate scan` first, or pass --select with a scanned path",
        ));
    }
    let conflicts = at_migrate::conflicts::detect_conflicts_with_store(&report, &store_items);
    let resolutions = parse_resolutions(sub, &conflicts, &report, &store_items)?;

    let plans = match at_migrate::plan::build_scope_plans(at_migrate::plan::ScopePlansRequest {
        scan: &report,
        resolutions: &resolutions,
        selected_paths: &selected,
        stores: &stores,
        target_scope: target,
        project_id: project_id.as_deref(),
        copy_agents: &copy_agents(&paths),
        strategy,
        backup_root,
    }) {
        Ok(plans) => plans,
        Err(error) => return Err(plan_error_with_context(error, sub, globals, &conflicts)),
    };
    let document = at_migrate::plan::plans_document(&plans).map_err(plan_error)?;
    let persisted = if globals.dry_run {
        None
    } else {
        let _access = CliAccess::write(&paths)?;
        let primary = plans
            .first()
            .and_then(|plan| stores.root(plan.scope))
            .ok_or_else(|| CliError::internal("the reviewed plan names no store"))?;
        Some(at_migrate::plan::persist_plans(primary, &document).map_err(plan_error)?)
    };
    let dry_run = plans
        .iter()
        .flat_map(at_migrate::plan::dry_run)
        .collect::<Vec<_>>();

    Ok(CommandOutput {
        data: json!({
            "plan_id": document.id,
            "plan_hash": document.plan_hash,
            "strategy": match strategy {
                at_migrate::plan::MigrationStrategy::ReplaceWithLinks => STRATEGY_LINKS,
                at_migrate::plan::MigrationStrategy::LeaveAndWatch => STRATEGY_WATCH,
                _ => "unknown",
            },
            "persisted": persisted.is_some(),
            "plan_path": persisted.as_ref().map(|path| path.display().to_string()),
            "counts": plan_counts(&plans),
            "scopes": scope_records(&plans, &stores),
            "steps": step_records(&plans),
            "dry_run": dry_run_rows(&dry_run),
            "files_checked": dry_run.len(),
        }),
        human: plan_human(&plans, persisted.as_deref()),
        warnings: Vec::new(),
        warnings_in_report: false,
    })
}

/// Maps a plan failure onto the typed CLI codes.
pub(crate) fn plan_error(error: at_migrate::plan::PlanError) -> CliError {
    match &error {
        at_migrate::plan::PlanError::UnresolvedConflict { name } => CliError::new(
            ErrorCode::NeedsResolution,
            format!("conflict `{name}` requires an explicit resolution"),
            "rerun with --keep-a, --keep-b or --keep-both naming this conflict",
        )
        .with_details(json!({ "conflict": name })),
        at_migrate::plan::PlanError::StalePlan => CliError::new(
            ErrorCode::StalePlan,
            "the migration plan is stale: a source changed after review",
            "rerun `agentuna migrate plan` to rebuild the plan from current bytes",
        ),
        at_migrate::plan::PlanError::EmptySelection => CliError::new(
            ErrorCode::Validation,
            "no native items are selected for migration",
            "run `agentuna migrate scan` first, or pass --select with a scanned path",
        ),
        at_migrate::plan::PlanError::MissingProject { scope } => CliError::new(
            ErrorCode::Validation,
            format!("{scope}-scope items need a selected project"),
            "pass --project with a registered project directory, or select only user-scope items",
        ),
        _ => CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "check the selected paths, then rerun `agentuna migrate plan`",
        ),
    }
}

/// A plan failure with the context only the dispatch site has.
///
/// An unresolved conflict is the one plan failure a caller can fix from the prompt, so this
/// adds what the engine error cannot carry: every copy with its agent, scope and path, and a
/// ready-to-run command for each resolution flag. Every other failure passes through.
fn plan_error_with_context(
    error: at_migrate::plan::PlanError,
    sub: &clap::ArgMatches,
    globals: &GlobalArgs,
    conflicts: &at_migrate::conflicts::ConflictSet,
) -> CliError {
    let unresolved = match &error {
        at_migrate::plan::PlanError::UnresolvedConflict { name } => Some(name.clone()),
        _ => None,
    };
    let Some(name) = unresolved else {
        return plan_error(error);
    };
    let Some(group) = conflicts.conflicts.iter().find(|group| group.name == name) else {
        return plan_error(error);
    };
    let copies = group
        .copies
        .iter()
        .map(|copy| {
            json!({
                "agent": copy.agent.as_ref().map(ToString::to_string),
                "origin": match copy.origin {
                    at_migrate::conflicts::CopyOrigin::Native => "native",
                    at_migrate::conflicts::CopyOrigin::Store => "store",
                    _ => "other",
                },
                "scope": copy.scope.to_string(),
                "path": copy.path.display().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let keep_a = plan_command(sub, globals, "--keep-a", &name);
    let keep_b = plan_command(sub, globals, "--keep-b", &name);
    let keep_both = plan_command(sub, globals, "--keep-both", &name);
    let locations = group
        .copies
        .iter()
        .map(|copy| match &copy.agent {
            Some(agent) => format!("{} ({agent})", copy.path.display()),
            None => format!("{} (store)", copy.path.display()),
        })
        .collect::<Vec<_>>()
        .join(", ");
    CliError::new(
        ErrorCode::NeedsResolution,
        format!("conflict `{name}` requires an explicit resolution"),
        format!(
            "rerun `{keep_both}` to keep both copies, or pick one with --keep-a/--keep-b; \
             `{name}` appears at {locations}"
        ),
    )
    .with_details(json!({
        "conflict": name,
        "kind": group.kind.to_string(),
        "copies": copies,
        "keep_a": keep_a,
        "keep_b": keep_b,
        "keep_both": keep_both,
    }))
}

/// The exact `migrate plan` command that resolves `name` with `flag`.
///
/// It repeats the strategy, selections and project this invocation used, so the command can
/// be pasted as printed and plan the same selection.
fn plan_command(sub: &clap::ArgMatches, globals: &GlobalArgs, flag: &str, name: &str) -> String {
    let strategy = sub
        .get_one::<String>("strategy")
        .map(String::as_str)
        .unwrap_or(STRATEGY_LINKS);
    let mut command = format!("agentuna migrate plan --strategy {strategy} {flag} {name}");
    if let Some(selects) = sub.get_many::<String>("select") {
        for select in selects {
            command.push_str(" --select ");
            command.push_str(&shell_argument(select));
        }
    }
    if let Some(project) = &globals.project {
        command.push_str(" --project ");
        command.push_str(&shell_argument(&project.display().to_string()));
    }
    command
}

/// Quotes a value for a ready-to-copy shell command when it needs quoting.
fn shell_argument(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || "-_./:@%=+,~".contains(character)
        });
    if plain {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Summary counts of a reviewed selection, summed across its receiving scopes.
fn plan_counts(plans: &[at_migrate::plan::MigrationPlan]) -> Value {
    let mut items = 0;
    let mut files_created = 0;
    let mut files_backed_up = 0;
    let mut links_written = 0;
    let mut files_left_in_place = 0;
    for plan in plans {
        items += plan.counts.items;
        files_created += plan.counts.files_created;
        files_backed_up += plan.counts.files_backed_up;
        links_written += plan.counts.links_written;
        files_left_in_place += plan.counts.files_left_in_place;
    }
    json!({
        "items": items,
        "files_created": files_created,
        "files_backed_up": files_backed_up,
        "links_written": links_written,
        "files_left_in_place": files_left_in_place,
    })
}

/// One JSON record per receiving scope, naming the store root it writes into.
fn scope_records(
    plans: &[at_migrate::plan::MigrationPlan],
    stores: &at_migrate::plan::ScopeStores,
) -> Vec<Value> {
    plans
        .iter()
        .map(|plan| {
            json!({
                "scope": plan.scope.to_string(),
                "store_path": stores
                    .root(plan.scope)
                    .map(|root| root.display().to_string()),
                "counts": plan_counts(std::slice::from_ref(plan)),
                "steps": step_records(std::slice::from_ref(plan)),
            })
        })
        .collect()
}

/// Every reviewed step, in scope order.
fn step_records(plans: &[at_migrate::plan::MigrationPlan]) -> Vec<Value> {
    plans
        .iter()
        .flat_map(|plan| plan.steps.iter())
        .map(|step| {
            json!({
                "seq": step.seq,
                "action": step.action.clone(),
                "scope": step.scope.to_string(),
                "item_name": step.item_name,
                "source": step.source.as_ref().map(|path| path.display().to_string()),
                "destination": step.destination.as_ref().map(|path| path.display().to_string()),
                "backup_path": step.backup_path.as_ref().map(|path| path.display().to_string()),
                "bytes": step.bytes,
                "agents": step.agents.iter().map(|agent| agent.to_string()).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// Dry-run rows as JSON.
fn dry_run_rows(rows: &[at_migrate::plan::DryRunRow]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            json!({
                "seq": row.seq,
                "action": row.action.clone(),
                "source": row.source.as_ref().map(|path| path.display().to_string()),
                "destination": row.destination.as_ref().map(|path| path.display().to_string()),
                "bytes": row.bytes,
                "backup_path": row.backup_path.as_ref().map(|path| path.display().to_string()),
            })
        })
        .collect()
}

/// Human table for a reviewed selection.
fn plan_human(plans: &[at_migrate::plan::MigrationPlan], persisted: Option<&Path>) -> String {
    let mut out = String::new();
    let strategy = match plans.first().map(|plan| plan.strategy) {
        Some(at_migrate::plan::MigrationStrategy::ReplaceWithLinks) => "links",
        Some(at_migrate::plan::MigrationStrategy::LeaveAndWatch) => "watch",
        _ => "unknown",
    };
    let counts = plan_counts(plans);
    let _ = std::fmt::Write::write_fmt(
        &mut out,
        format_args!(
            "Plan {strategy}: {} item(s), {} file(s) created, {} backup(s), {} link(s)\n",
            counts["items"],
            counts["files_created"],
            counts["files_backed_up"],
            counts["links_written"]
        ),
    );
    for plan in plans {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!("  {} store: {} item(s)\n", plan.scope, plan.counts.items),
        );
    }
    match persisted {
        Some(path) => {
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("Persisted: {}\n", path.display()),
            );
        }
        None => out.push_str("Dry run: nothing was written\n"),
    }
    out
}

/// The refusal `migrate apply` gives when `--plan` is missing.
///
/// TC-030 names the plan on the command line (`migrate apply --plan <id> --json`), so the
/// flag stays required. Clap cannot name the ids a caller may pick, so this refusal does:
/// every persisted plan id, the newest first, and the exact command that reviews it.
fn missing_plan(paths: &at_store::paths::AppPaths) -> CliError {
    let Ok(stores) = migration_stores(paths) else {
        return CliError::new(
            ErrorCode::Usage,
            "migrate apply needs --plan <PLAN_ID>",
            "build a plan with `agentuna migrate plan`, then review it with `agentuna migrate apply --plan <PLAN_ID> --dry-run`",
        );
    };
    let mut plans = Vec::new();
    for root in stores.scopes().filter_map(|scope| stores.root(scope)) {
        plans.extend(list_plans(root));
    }
    // Plan ids are UUIDv7, so they sort chronologically and the last entry is the newest.
    plans.sort_by_key(|plan| plan["plan_id"].as_str().unwrap_or_default().to_owned());
    let Some(latest) = plans
        .last()
        .and_then(|plan| plan["plan_id"].as_str())
        .map(str::to_owned)
    else {
        return CliError::new(
            ErrorCode::Usage,
            "migrate apply needs --plan <PLAN_ID>; no plan is persisted yet",
            "build one with `agentuna migrate plan`, then review it with `agentuna migrate apply --plan <PLAN_ID> --dry-run`",
        );
    };
    CliError::new(
        ErrorCode::Usage,
        format!(
            "migrate apply needs --plan <PLAN_ID>; {} persisted plan(s) are available",
            plans.len()
        ),
        format!(
            "review the newest with `agentuna migrate apply --plan {latest} --dry-run`; \
             every id is listed by `agentuna migrate status`"
        ),
    )
    .with_details(json!({
        "plans": plans,
        "latest": latest,
        "next_command": format!("agentuna migrate apply --plan {latest} --dry-run"),
    }))
}

/// Loads a persisted reviewed selection by id from any receiving scope's store.
///
/// A selection can span scopes, so its document lives in the store of the first receiving scope,
/// which is not necessarily the store the current `--scope` names: every resolvable store is
/// searched instead of guessing one.
fn load_plans(
    paths: &at_store::paths::AppPaths,
    plan_id: &str,
) -> Result<at_migrate::plan::PersistedPlans, CliError> {
    let safe_id = plan_id.trim();
    if safe_id.is_empty()
        || safe_id.len() > 128
        || !safe_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err(CliError::new(
            ErrorCode::Validation,
            format!("`{plan_id}` is not a plan id"),
            "pass the plan_id exactly as `migrate plan` printed it",
        ));
    }
    let file_name = format!("{safe_id}.json");
    let stores = migration_stores(paths)?;
    for root in stores.scopes().filter_map(|scope| stores.root(scope)) {
        let path = root.join(".state").join("plans").join(&file_name);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(CliError::new(
                    ErrorCode::Conflict,
                    format!("persisted plan `{safe_id}` is unreadable: {error}"),
                    "rerun `agentuna migrate plan` to rebuild the plan",
                ));
            }
        };
        return serde_json::from_slice(&bytes).map_err(|error| {
            CliError::new(
                ErrorCode::Conflict,
                format!("persisted plan `{safe_id}` is unreadable: {error}"),
                "rerun `agentuna migrate plan` to rebuild the plan",
            )
        });
    }
    Err(CliError::new(
        ErrorCode::NotFound,
        format!("no persisted plan `{safe_id}` in any AgenTuna store"),
        "rerun `agentuna migrate plan` to build and persist a fresh plan",
    ))
}

/// `migrate apply`: dry-run rows without touching anything, or a confirmed apply.
fn apply(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let plan_id = sub
        .get_one::<String>("plan")
        .map(|value| value.trim())
        .unwrap_or_default();
    if plan_id.is_empty() {
        return Err(missing_plan(&paths));
    }
    let document = load_plans(&paths, plan_id)?;
    let plans = document.plans;
    if globals.dry_run {
        for plan in &plans {
            at_migrate::plan::validate_plan(plan).map_err(plan_error)?;
        }
        let dry_run = plans
            .iter()
            .flat_map(at_migrate::plan::dry_run)
            .collect::<Vec<_>>();
        return Ok(CommandOutput {
            data: json!({
                "plan_id": document.id,
                "dry_run": true,
                "counts": plan_counts(&plans),
                "operations": dry_run_rows(&dry_run),
                "files_checked": dry_run.len(),
            }),
            human: format!(
                "Dry run for plan {}: {} operation(s); nothing was written\n",
                document.id,
                dry_run.len()
            ),
            warnings: Vec::new(),
            warnings_in_report: false,
        });
    }
    if !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "migrate apply moves files and needs explicit confirmation",
            "review the plan with `migrate apply --dry-run`, then rerun with --yes",
        ));
    }
    let _access = CliAccess::write(&paths)?;
    let mut database = open_index(&paths)?;
    let stores = migration_stores(&paths)?;
    let authorized = authorized_roots(&paths)?;
    let project_id = registered_project_id(&paths);
    let outcome = {
        let mut context = at_migrate::apply::ApplyPlansContext {
            stores: &stores,
            backup_root: &paths.backup_dir().join("migration"),
            authorized_roots: &authorized,
            database: &mut database,
            project_id: project_id.as_deref(),
        };
        at_migrate::apply::apply_plans_with_actor(
            &plans,
            &mut context,
            at_db::ops::OperationActor::Cli,
        )
        .map_err(apply_error)?
    };
    let mut warnings = outcome
        .warnings
        .iter()
        .map(|warning| match warning {
            at_migrate::apply::MigrationWarning::CopyFallback { path, .. } => Warning::new(
                "copy_fallback",
                format!(
                    "{} received a manifest-bearing copy instead of a link",
                    path.display()
                ),
            ),
            _ => Warning::new(
                "migration_warning",
                "the migration completed with a deviation",
            ),
        })
        .collect::<Vec<_>>();
    if let Err(error) = at_engine::refresh::refresh_agents(database.connection(), &paths) {
        warnings.push(Warning::new(
            "agent_registry_refresh",
            format!("agent detection was not refreshed: {error}"),
        ));
    }
    Ok(CommandOutput {
        data: json!({
            "operation_id": outcome.operation_id,
            "completed": outcome.completed,
            "touched_paths": outcome
                .touched_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        }),
        human: format!(
            "Applied plan {}: {} operation row(s), {} file operation(s)\n",
            document.id, outcome.operation_id, outcome.completed
        ),
        warnings,
        warnings_in_report: false,
    })
}

/// Maps an apply failure onto the typed CLI codes.
pub(crate) fn apply_error(error: at_migrate::apply::ApplyPlanError) -> CliError {
    match error {
        at_migrate::apply::ApplyPlanError::Plan(plan) => plan_error(plan),
        at_migrate::apply::ApplyPlanError::Apply(apply) => CliError::new(
            ErrorCode::Conflict,
            format!("the migration was refused or rolled back: {apply}"),
            "inspect the reported path, resolve the problem, then apply again",
        ),
        at_migrate::apply::ApplyPlanError::PermissionDenied { path, required } => CliError::new(
            ErrorCode::Conflict,
            format!("{} refused access: {required}", path.display()),
            "grant the named permission, then rerun the apply",
        ),
        _ => CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "run `agentuna doctor`, then rebuild the plan with `migrate plan`",
        ),
    }
}

/// Opens the write index, mapping a missing index to the doctor hint.
fn open_index(paths: &at_store::paths::AppPaths) -> Result<at_db::Database, CliError> {
    at_db::Database::open_file(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot open the AgenTuna index: {error}"),
            "run `agentuna doctor` to create or repair the index",
        )
    })
}

/// Home and project roots an apply may touch.
///
/// A selected project that has no `.agentuna` store yet is a normal first migration: only
/// roots that exist are authorized, so a project directory alone does not make the apply fail.
pub(crate) fn authorized_roots(
    paths: &at_store::paths::AppPaths,
) -> Result<at_fs::ApplyRoots, CliError> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    roots.push(paths.user_store().to_path_buf());
    if let Some(project) = paths.project_store() {
        roots.push(project.to_path_buf());
        if let Some(parent) = project.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    // Backups and the Global store are AgenTuna-owned roots outside HOME when
    // `AGENTUNA_HOME` is redirected; without them a first apply refuses its own backups.
    if at_fs::ensure_dir(paths.backup_dir()).is_ok() {
        roots.push(paths.backup_dir().to_path_buf());
    }
    if paths.global_store().is_dir() {
        roots.push(paths.global_store().to_path_buf());
    }
    roots.retain(|path| path.is_dir());
    at_fs::ApplyRoots::new(roots).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "native agent roots are unavailable",
            "set HOME or AGENTUNA_HOME to a readable directory",
        )
    })
}

/// Resolves the project root one operation belongs to from what the index stored.
///
/// Prefers the project stamped on the operation's own deployment rows (a Project-scope apply
/// copies it onto every row it writes); otherwise takes the deepest registered project that
/// contains a file the operation touched. `None` when nothing in the index ties the operation
/// to a registered project, which is the only case that needs an explicit `--project`.
fn operation_project_root(
    database: &at_db::Database,
    detail: &ops::OperationDetail,
) -> Result<Option<PathBuf>, CliError> {
    let connection = database.connection();
    let history_error = |error: at_db::DbError| {
        CliError::new(
            ErrorCode::Conflict,
            format!("operation history is unavailable: {error}"),
            "run `agentuna doctor` to inspect the index",
        )
    };
    for project_id in at_db::deployments::operation_project_ids(connection, &detail.operation.id)
        .map_err(history_error)?
    {
        if let Some(project) =
            at_db::projects::find_project_by_id(connection, &project_id).map_err(history_error)?
        {
            return Ok(Some(PathBuf::from(project.path)));
        }
    }
    let mut best: Option<(usize, PathBuf)> = None;
    for project in at_db::projects::list_projects(connection).map_err(history_error)? {
        let root = PathBuf::from(&project.path);
        let depth = root.components().count();
        if best.as_ref().is_some_and(|(found, _)| *found >= depth) {
            continue;
        }
        let touched = detail
            .files
            .iter()
            .filter_map(|file| file.path.as_deref())
            .any(|path| Path::new(path).starts_with(&root));
        if touched {
            best = Some((depth, root));
        }
    }
    Ok(best.map(|(_, root)| root))
}

/// `migrate undo <id>`: generic history-backed undo with typed refusals.
fn undo(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let mut paths = resolve_paths(globals)?;
    let operation_id = sub
        .get_one::<String>("operation")
        .map(String::as_str)
        .unwrap_or_default();
    if !globals.yes && !sub.get_flag("force") {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "undo restores earlier file contents and needs explicit confirmation",
            "inspect `migrate status`, then rerun with --yes or --force",
        ));
    }
    let _access = CliAccess::write(&paths)?;
    let database = open_index(&paths)?;
    let detail = ops::operation_detail(database.connection(), operation_id).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("operation history is unavailable: {error}"),
            "run `agentuna doctor` to inspect the index",
        )
    })?;
    let Some(detail) = detail else {
        return Err(CliError::new(
            ErrorCode::NotFound,
            format!("operation {operation_id} was not found"),
            "list operations with `agentuna migrate status --json`",
        ));
    };
    let scope = detail.operation.scope.unwrap_or(Scope::User);
    // A Project-scope operation lives in that project's own store. An explicit --project still
    // wins; otherwise the operation's own rows name its project. That also covers a selection
    // that spans scopes and is filed under its first (User) scope while still writing the project
    // store, so the undo never demands a flag the index can already answer.
    if globals.project.is_none() {
        match operation_project_root(&database, &detail)? {
            Some(project) => paths = resolve_paths_for_project(&project)?,
            None if matches!(scope, Scope::Project | Scope::Local) => {
                return Err(CliError::new(
                    ErrorCode::Validation,
                    format!(
                        "operation {operation_id} is a Project-scope operation and the index does not name its project"
                    ),
                    "pass --project <path> pointing at the project the operation migrated",
                ));
            }
            None => {}
        }
    }
    let layout = StoreLayout::for_scope(&paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "the store for this operation is unavailable",
            "run `agentuna doctor` to initialize the store",
        )
    })?;
    let authorized = authorized_roots(&paths)?;
    let outcome = at_migrate::undo::undo_operation(
        at_migrate::undo::UndoOperationRequest {
            connection: database.connection(),
            store_root: layout.root(),
            backup_root: paths.backup_dir(),
            authorized_roots: &authorized,
            actor: ops::OperationActor::Cli,
            force: sub.get_flag("force"),
        },
        operation_id,
    )
    .map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("the operation could not be undone: {error}"),
            "inspect the reported backup or path, then retry",
        )
    })?;
    match outcome {
        at_migrate::undo::UndoOutcome::Restored { undo_operation_id } => Ok(CommandOutput {
            data: json!({
                "undo_operation_id": undo_operation_id,
                "undone": operation_id,
            }),
            human: format!("Operation {operation_id} was undone ({undo_operation_id}).\n"),
            warnings: Vec::new(),
            warnings_in_report: false,
        }),
        at_migrate::undo::UndoOutcome::Expired => Err(CliError::new(
            ErrorCode::UndoExpired,
            format!("operation {operation_id} is outside the 30-day undo window"),
            "restore the affected files from `agentuna backups list` instead",
        )),
        at_migrate::undo::UndoOutcome::AlreadyUndone => Err(CliError::new(
            ErrorCode::Conflict,
            format!("operation {operation_id} was already undone"),
            "the history keeps the Undo operation; nothing else to do",
        )),
        at_migrate::undo::UndoOutcome::NotUndoable => Err(CliError::new(
            ErrorCode::Conflict,
            format!("operation {operation_id} did not succeed, so it has nothing to undo"),
            "the operation rolled its files back; inspect the history for the failure",
        )),
        at_migrate::undo::UndoOutcome::Conflict { paths: conflicts } => Err(CliError::new(
            ErrorCode::Conflict,
            format!(
                "{} path(s) changed after the operation; the undo was refused",
                conflicts.len()
            ),
            "the newer on-disk content is preserved; adopt it or restore a backup explicitly",
        )
        .with_details(json!({
            "paths": conflicts.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        }))),
        at_migrate::undo::UndoOutcome::Partial { paths: incomplete } => Err(CliError::new(
            ErrorCode::PartialFailure,
            format!(
                "{} path(s) could not be represented by the undo pipeline",
                incomplete.len()
            ),
            "inspect the listed paths and restore them from `agentuna backups list`",
        )
        .with_details(json!({
            "paths": incomplete.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        }))),
        _ => Err(CliError::internal(
            "the undo pipeline returned an outcome this build does not know",
        )),
    }
}

/// `migrate status`: fresh counts, pending plans and recent migration operations.
fn status(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let report = run_scan(&paths)?;
    let managed = report
        .items
        .iter()
        .filter(|item| item.ownership == at_migrate::scan::Ownership::Managed)
        .count();
    let conflicting = report
        .items
        .iter()
        .filter(|item| item.ownership == at_migrate::scan::Ownership::Conflict)
        .count();

    // A pending selection lives in the store of the first scope it receives items into, which is
    // not necessarily the store `--scope` names, so every resolvable store is listed.
    let stores = migration_stores(&paths)?;
    let mut plans = Vec::new();
    for root in stores.scopes().filter_map(|scope| stores.root(scope)) {
        plans.extend(list_plans(root));
    }
    plans.sort_by_key(|plan| plan["plan_id"].as_str().unwrap_or_default().to_owned());

    let mut recent = Vec::new();
    if paths.db_path().exists()
        && let Ok(database) = at_db::ReadOnlyDatabase::open(paths.db_path())
    {
        let query = ops::RecentChangesQuery {
            limit: 5,
            kind: Some(ops::OperationKind::Migrate),
            ..ops::RecentChangesQuery::default()
        };
        if let Ok(rows) = ops::recent_changes(database.connection(), &query) {
            for row in rows {
                recent.push(json!({
                    "operation_id": row.id,
                    "kind": row.kind.map(|kind| kind.as_str()),
                    "summary": row.summary,
                    "started_at": row.started_at,
                    "status": row.status.map(|status| status.as_str()),
                    "file_count": row.file_count,
                }));
            }
        }
    }

    let target = target_scope(globals, &paths)?;
    Ok(CommandOutput {
        data: json!({
            "scope": target.map(|scope| scope.to_string()),
            "items": {
                "total": report.items.len(),
                "managed": managed,
                "conflicting": conflicting,
                "new": report.items.len() - managed - conflicting,
            },
            "partial": report.partial,
            "pending_plans": plans,
            "recent_operations": recent,
        }),
        human: {
            let mut out = format!(
                "{} native item(s): {} managed, {} conflicting, {} new\n",
                report.items.len(),
                managed,
                conflicting,
                report.items.len() - managed - conflicting
            );
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("{} pending plan(s)\n", plans.len()),
            );
            for plan in &plans {
                let _ =
                    std::fmt::Write::write_fmt(&mut out, format_args!("  {}\n", plan["plan_id"]));
            }
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("{} recent operation(s)\n", recent.len()),
            );
            out
        },
        warnings: Vec::new(),
        warnings_in_report: false,
    })
}

/// Pending plan ids under `<store>/.state/plans`.
fn list_plans(store_root: &Path) -> Vec<Value> {
    let directory = store_root.join(".state").join("plans");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut plans = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let plan_id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            plans.push(json!({
                "plan_id": plan_id,
                "path": path.display().to_string(),
            }));
        }
    }
    plans.sort_by_key(|plan| plan["plan_id"].as_str().unwrap_or_default().to_owned());
    plans
}
