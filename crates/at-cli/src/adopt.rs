//! `agentuna adopt` (TC-030): adopt one unmanaged native item into the store through the
//! shared at-migrate adopt pipeline.
//!
//! Adoption is a reviewed, undoable operation. `--dry-run` previews the exact file
//! operations without writing; the real run requires `--yes`, refuses a stale scan
//! record with `stale_plan` and never guesses an ambiguous target.

use std::path::PathBuf;

use at_migrate::adopt::{AdoptError, AdoptHandling, AdoptRequest, adopt_item, preview_adopt};
use at_store::layout::StoreLayout;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::migrate::{apply_error, authorized_roots, plan_error, run_scan};
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, resolve_paths};

/// `agentuna adopt <PATH>`: one native item becomes a canonical, owned store item.
pub struct AdoptGroup;

impl CommandGroup for AdoptGroup {
    fn name(&self) -> &'static str {
        "adopt"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("adopt")
                .about("Adopt one native item into the AgenTuna store as an owned, undoable item")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .help("Native path of the item to adopt"),
                )
                .arg(
                    Arg::new("keep")
                        .long("keep")
                        .action(ArgAction::SetTrue)
                        .help("Keep the native original in place and record it as watched"),
                )
                .arg(
                    Arg::new("handling")
                        .long("handling")
                        .value_name("LINK|COPY|WATCH")
                        .help("Original handling; overrides --keep"),
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
            .ok_or_else(|| CliError::internal("adopt dispatch without a subcommand"))?;
        if name != "adopt" {
            return Err(CliError::internal(format!(
                "adopt has no `{name}` subcommand"
            )));
        }
        adopt(globals, sub)
    }
}

/// Resolves the requested handling from `--handling`/`--keep`.
fn requested_handling(sub: &clap::ArgMatches) -> Result<AdoptHandling, CliError> {
    match sub.get_one::<String>("handling").map(String::as_str) {
        Some(value) => AdoptHandling::parse(value).map_err(|error| {
            CliError::new(
                ErrorCode::Validation,
                error.to_string(),
                "use --handling link, copy or watch",
            )
        }),
        None if sub.get_flag("keep") => Ok(AdoptHandling::Watch),
        None => Ok(AdoptHandling::Link),
    }
}

/// `agentuna adopt <PATH>`: preview with `--dry-run`, otherwise a confirmed apply.
fn adopt(globals: &GlobalArgs, sub: &clap::ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let raw_path = sub
        .get_one::<String>("path")
        .map(String::as_str)
        .unwrap_or_default();
    let target = PathBuf::from(raw_path);
    if !target.is_absolute() {
        return Err(CliError::new(
            ErrorCode::Validation,
            "adopt needs an absolute native path",
            "pass the full path printed by `agentuna migrate scan --json`",
        ));
    }
    let handling = requested_handling(sub)?;
    let scan = run_scan(&paths)?;
    let item = scan
        .items
        .iter()
        .find(|item| item.path == target)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::NotFound,
                format!("no native item was found at {}", target.display()),
                "run `agentuna migrate scan --json` and adopt one of the listed paths",
            )
        })?
        .clone();
    let (scope, store_root) = store_scope_for_item(&paths, item.scope)?;
    let authorized = authorized_roots(&paths)?;

    if globals.dry_run {
        let preview = preview_adopt(&AdoptRequest {
            scan: &scan,
            path: &item.path,
            store_root: &store_root,
            backup_root: paths.backup_dir(),
            authorized_roots: &authorized,
            database: &mut adopt_database(&paths)?,
            scope,
            handling,
        })
        .map_err(adopt_error)?;
        return Ok(CommandOutput {
            data: adopt_payload(&preview, None),
            human: format!(
                "Adopt preview for `{}`: store item `{}` ({}); nothing was written\n",
                preview.native_name,
                preview.slug,
                preview.handling.as_str()
            ),
            warnings: Vec::new(),
            warnings_in_report: false,
        });
    }
    if !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "adopt replaces the native original and needs explicit confirmation",
            "inspect `adopt --dry-run`, then rerun with --yes",
        ));
    }
    let _access = CliAccess::write(&paths)?;
    let mut database = adopt_database(&paths)?;
    let outcome = adopt_item(AdoptRequest {
        scan: &scan,
        path: &item.path,
        store_root: &store_root,
        backup_root: paths.backup_dir(),
        authorized_roots: &authorized,
        database: &mut database,
        scope,
        handling,
    })
    .map_err(adopt_error)?;
    Ok(CommandOutput {
        data: json!({
            "operation_id": outcome.operation_id,
            "slug": outcome.slug,
            "native_name": outcome.native_name,
            "store_destination": outcome.store_destination.display().to_string(),
            "handling": outcome.handling.as_str(),
            "touched_paths": outcome
                .touched_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        }),
        human: format!(
            "Adopted `{}` as `{}` ({}) — operation {}\n",
            outcome.native_name,
            outcome.slug,
            outcome.handling.as_str(),
            outcome.operation_id
        ),
        warnings: Vec::new(),
        warnings_in_report: false,
    })
}

/// Opens the index an adopt needs for its operation rows.
fn adopt_database(paths: &at_store::paths::AppPaths) -> Result<at_db::Database, CliError> {
    at_db::Database::open_file(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot open the AgenTuna index: {error}"),
            "run `agentuna doctor` to create or repair the index",
        )
    })
}

/// JSON payload shared by preview and apply.
fn adopt_payload(preview: &at_migrate::adopt::AdoptPreview, operation: Option<&str>) -> Value {
    json!({
        "operation_id": operation,
        "source": preview.source.display().to_string(),
        "store_root": preview.store_root.display().to_string(),
        "store_destination": preview.store_destination.display().to_string(),
        "slug": preview.slug,
        "native_name": preview.native_name,
        "kind": preview.kind.to_string(),
        "agents": preview.agents.iter().map(|agent| agent.to_string()).collect::<Vec<_>>(),
        "handling": preview.handling.as_str(),
        "renamed": preview.renamed,
        "current": match &preview.current {
            at_migrate::adopt::PathState::Present(hash) => json!({ "state": "present", "hash": hash }),
            at_migrate::adopt::PathState::Missing => json!({ "state": "missing" }),
            _ => json!({ "state": "unknown" }),
        },
        "dry_run": preview
            .rows
            .iter()
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
            .collect::<Vec<_>>(),
    })
}

/// Maps an adopt failure onto the typed CLI codes.
fn adopt_error(error: AdoptError) -> CliError {
    match error {
        AdoptError::ItemNotFound(path) => CliError::new(
            ErrorCode::NotFound,
            format!("no native item was found at {}", path.display()),
            "run `agentuna migrate scan --json` and adopt one of the listed paths",
        ),
        AdoptError::StaleScanRecord { path, current } => CliError::new(
            ErrorCode::StalePlan,
            format!(
                "{} changed since the scan ({current:?}); rescan before adopting",
                path.display()
            ),
            "rerun `agentuna migrate scan --json`, then adopt the fresh path",
        ),
        AdoptError::AlreadyManaged(path) => CliError::new(
            ErrorCode::Conflict,
            format!("{} is already managed by AgenTuna", path.display()),
            "nothing to adopt; inspect the item with `agentuna effective` instead",
        ),
        AdoptError::InvalidHandling(value) => CliError::new(
            ErrorCode::Validation,
            format!("unknown adopt handling `{value}`"),
            "use link, copy or watch",
        ),
        AdoptError::Plan(plan) => plan_error(plan),
        AdoptError::Apply(apply) => apply_error(apply),
        _ => CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "resolve the reported problem, then rerun the adopt",
        ),
    }
}

/// Store scope and root receiving an adopted item, mirroring the GUI rule.
///
/// Project and Local items adopt into the project store when one is selected; everything
/// else adopts into the user store.
fn store_scope_for_item(
    paths: &at_store::paths::AppPaths,
    item_scope: at_model::Scope,
) -> Result<(at_model::Scope, PathBuf), CliError> {
    let scope = match item_scope {
        at_model::Scope::Project | at_model::Scope::Local
            if paths.project_store().is_some() =>
        {
            at_model::Scope::Project
        }
        _ => at_model::Scope::User,
    };
    let layout = StoreLayout::for_scope(paths, scope).map_err(|_| {
        CliError::new(
            ErrorCode::NotFound,
            "the adoption store is unavailable for the selected scope",
            "run `agentuna doctor` to initialize the store",
        )
    })?;
    Ok((scope, layout.root().to_path_buf()))
}
