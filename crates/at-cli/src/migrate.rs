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
use crate::runtime::{CliAccess, resolve_paths};

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
                                .help("Resolve CONFLICT by keeping the first sorted copy"),
                        )
                        .arg(
                            Arg::new("keep_b")
                                .long("keep-b")
                                .value_name("CONFLICT")
                                .help("Resolve CONFLICT by keeping the second sorted copy"),
                        )
                        .arg(
                            Arg::new("keep_both")
                                .long("keep-both")
                                .value_name("CONFLICT")
                                .help("Resolve CONFLICT by keeping both copies under derived names"),
                        ),
                )
                .subcommand(
                    Command::new("apply")
                        .about("Apply a persisted plan; --dry-run lists the exact file operations")
                        .arg(
                            Arg::new("plan")
                                .long("plan")
                                .value_name("PLAN_ID")
                                .required(true)
                                .help("Plan id printed by `migrate plan`"),
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
    at_migrate::scan::scan(at_migrate::scan::ScanRequest {
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
    })
}

/// The store scope a migration writes to: Project with `--project`, otherwise User.
fn migration_scope(globals: &GlobalArgs, paths: &at_store::paths::AppPaths) -> Result<Scope, CliError> {
    match globals.scope() {
        Some(Scope::Project) | Some(Scope::Local) => {
            if paths.project_store().is_none() {
                return Err(CliError::new(
                    ErrorCode::Validation,
                    "--scope project needs a project: pass --project with a registered directory",
                    "pass --project, or drop --scope to migrate into the user store",
                ));
            }
            Ok(Scope::Project)
        }
        Some(scope) => Ok(scope),
        None if paths.project_store().is_some() => Ok(Scope::Project),
        None => Ok(Scope::User),
    }
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

/// `migrate scan`: read-only discovery over the injected roots.
fn scan(globals: &GlobalArgs) -> Result<CommandOutput, CliError> {
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
/// `--keep-both` derives two free names from the conflict's copies, matching the GUI's
/// derived names; an unknown conflict name is refused with `needs_resolution` because the
/// caller asked to resolve a group that does not exist.
fn parse_resolutions(
    sub: &clap::ArgMatches,
    conflicts: &at_migrate::conflicts::ConflictSet,
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
            return Err(CliError::new(
                ErrorCode::NeedsResolution,
                format!("no conflict named `{conflict}` exists in the current scan"),
                "rerun `agentuna migrate plan --dry-run` to list the conflict names",
            ));
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
            let existing = group
                .copies
                .iter()
                .map(|copy| copy.path.display().to_string())
                .collect::<BTreeSet<_>>();
            group.copies.first().map(|copy| {
                at_migrate::conflicts::keep_both_names(&group.name, &copy.agent, &existing)
            })
        });
        let Some((first, second)) = names.flatten() else {
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
    let scope = migration_scope(globals, &paths)?;
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
    let layout = StoreLayout::for_scope(&paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "the migration store is unavailable for the selected scope",
            "run `agentuna doctor` to initialize the store",
        )
    })?;
    let store_root = layout.root().to_path_buf();
    let backup_root = paths.backup_dir().join("migration");

    let mut report = run_scan(&paths)?;
    let selected = match sub.get_many::<String>("select") {
        Some(paths_arg) => {
            let wanted: BTreeSet<String> =
                paths_arg.map(|path| path.as_str().to_owned()).collect();
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
    let conflicts = at_migrate::conflicts::detect_conflicts(&report);
    let resolutions = parse_resolutions(sub, &conflicts)?;

    let plan = at_migrate::plan::build_plan(at_migrate::plan::BuildPlanRequest {
        scan: &report,
        conflicts: &conflicts,
        resolutions: &resolutions,
        selected_paths: &selected,
        store_root: store_root.clone(),
        copy_agents: &copy_agents(&paths),
        strategy,
        scope,
        backup_root: backup_root.clone(),
    })
    .map_err(plan_error)?;
    at_migrate::plan::validate_plan(&plan).map_err(plan_error)?;
    let dry_run = at_migrate::plan::dry_run(&plan);

    let persisted = if globals.dry_run {
        None
    } else {
        let _access = CliAccess::write(&paths)?;
        Some(at_migrate::plan::persist_plan(&store_root, &plan).map_err(plan_error)?)
    };

    Ok(CommandOutput {
        data: json!({
            "plan_id": plan.id,
            "plan_hash": plan.plan_hash,
            "scope": plan.scope.to_string(),
            "strategy": match plan.strategy {
                at_migrate::plan::MigrationStrategy::ReplaceWithLinks => STRATEGY_LINKS,
                at_migrate::plan::MigrationStrategy::LeaveAndWatch => STRATEGY_WATCH,
                _ => "unknown",
            },
            "persisted": persisted.is_some(),
            "plan_path": persisted.as_ref().map(|path| path.display().to_string()),
            "counts": plan_counts(&plan),
            "steps": plan
                .steps
                .iter()
                .map(|step| json!({
                    "seq": step.seq,
                    "action": step.action.clone(),
                    "item_name": step.item_name,
                    "source": step.source.as_ref().map(|path| path.display().to_string()),
                    "destination": step.destination.as_ref().map(|path| path.display().to_string()),
                    "backup_path": step.backup_path.as_ref().map(|path| path.display().to_string()),
                    "bytes": step.bytes,
                    "agents": step.agents.iter().map(|agent| agent.to_string()).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "dry_run": dry_run_rows(&dry_run),
            "files_checked": dry_run.len(),
        }),
        human: plan_human(&plan, persisted.as_deref()),
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
        _ => CliError::new(
            ErrorCode::Validation,
            error.to_string(),
            "check the selected paths, then rerun `agentuna migrate plan`",
        ),
    }
}

/// Summary counts of a built plan.
fn plan_counts(plan: &at_migrate::plan::MigrationPlan) -> Value {
    json!({
        "items": plan.counts.items,
        "files_created": plan.counts.files_created,
        "files_backed_up": plan.counts.files_backed_up,
        "links_written": plan.counts.links_written,
        "files_left_in_place": plan.counts.files_left_in_place,
    })
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

/// Human table for a built plan.
fn plan_human(plan: &at_migrate::plan::MigrationPlan, persisted: Option<&Path>) -> String {
    let mut out = String::new();
    let strategy = match plan.strategy {
        at_migrate::plan::MigrationStrategy::ReplaceWithLinks => "links",
        at_migrate::plan::MigrationStrategy::LeaveAndWatch => "watch",
        _ => "unknown",
    };
    let _ = std::fmt::Write::write_fmt(
        &mut out,
        format_args!(
            "Plan {} ({}): {} item(s), {} file(s) created, {} backup(s), {} link(s)\n",
            plan.id,
            strategy,
            plan.counts.items,
            plan.counts.files_created,
            plan.counts.files_backed_up,
            plan.counts.links_written
        ),
    );
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

/// Loads a persisted plan by id from the selected scope's store.
fn load_plan(
    paths: &at_store::paths::AppPaths,
    scope: Scope,
    plan_id: &str,
) -> Result<at_migrate::plan::MigrationPlan, CliError> {
    let safe_id = plan_id.trim();
    if safe_id.is_empty()
        || safe_id.len() > 128
        || !safe_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(CliError::new(
            ErrorCode::Validation,
            format!("`{plan_id}` is not a plan id"),
            "pass the plan_id exactly as `migrate plan` printed it",
        ));
    }
    let layout = StoreLayout::for_scope(paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "the store for this plan is unavailable",
            "run `agentuna doctor` to initialize the store",
        )
    })?;
    let path = layout
        .root()
        .join(".state")
        .join("plans")
        .join(format!("{safe_id}.json"));
    let bytes = std::fs::read(&path).map_err(|error| {
        CliError::new(
            ErrorCode::NotFound,
            format!("no persisted plan `{safe_id}`: {error}"),
            "rerun `agentuna migrate plan` to build and persist a fresh plan",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("persisted plan `{safe_id}` is unreadable: {error}"),
            "rerun `agentuna migrate plan` to rebuild the plan",
        )
    })
}

/// `migrate apply`: dry-run rows without touching anything, or a confirmed apply.
fn apply(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let plan_id = sub
        .get_one::<String>("plan")
        .map(String::as_str)
        .unwrap_or_default();
    // The plan's own scope decides which store holds the plan file.
    let scope = migration_scope(globals, &paths)?;
    let plan = load_plan(&paths, scope, plan_id)?;
    if globals.dry_run {
        at_migrate::plan::validate_plan(&plan).map_err(plan_error)?;
        let dry_run = at_migrate::plan::dry_run(&plan);
        return Ok(CommandOutput {
            data: json!({
                "plan_id": plan.id,
                "scope": plan.scope.to_string(),
                "dry_run": true,
                "counts": plan_counts(&plan),
                "operations": dry_run_rows(&dry_run),
                "files_checked": dry_run.len(),
            }),
            human: format!(
                "Dry run for plan {}: {} operation(s); nothing was written\n",
                plan.id,
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
    let layout = StoreLayout::for_scope(&paths, plan.scope).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "the store for this plan is unavailable",
            "run `agentuna doctor` to initialize the store",
        )
    })?;
    let authorized = authorized_roots(&paths)?;
    let mut context = at_migrate::apply::ApplyPlanContext {
        store_root: layout.root(),
        backup_root: &paths.backup_dir().join("migration"),
        authorized_roots: &authorized,
        database: &mut database,
    };
    let outcome = at_migrate::apply::apply_plan(&plan, &mut context).map_err(apply_error)?;
    let warnings = outcome
        .warnings
        .iter()
        .map(|warning| match warning {
            at_migrate::apply::MigrationWarning::CopyFallback { path, .. } => Warning::new(
                "copy_fallback",
                format!("{} received a manifest-bearing copy instead of a link", path.display()),
            ),
            _ => Warning::new("migration_warning", "the migration completed with a deviation"),
        })
        .collect::<Vec<_>>();
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
            plan.id,
            outcome.operation_id,
            outcome.completed
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
pub(crate) fn authorized_roots(paths: &at_store::paths::AppPaths) -> Result<at_fs::ApplyRoots, CliError> {
    let mut roots = Vec::new();
    if let Some(home) = paths.home_dir() {
        roots.push(home.to_path_buf());
    }
    if let Some(project) = paths.project_store().and_then(|store| store.parent()) {
        roots.push(project.to_path_buf());
    }
    at_fs::ApplyRoots::new(roots).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "native agent roots are unavailable",
            "set HOME or AGENTUNA_HOME to a readable directory",
        )
    })
}

/// `migrate undo <id>`: generic history-backed undo with typed refusals.
fn undo(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
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
    let scope = migration_scope(globals, &paths)?;
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

    let layout = StoreLayout::for_scope(&paths, scope).ok();
    let plans = layout
        .map(|layout| list_plans(layout.root()))
        .unwrap_or_default();

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

    Ok(CommandOutput {
        data: json!({
            "scope": scope.to_string(),
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
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{} pending plan(s)\n", plans.len()));
            for plan in &plans {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("  {}\n", plan["plan_id"]));
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
        if path.extension().is_some_and(|extension| extension == "json") {
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
