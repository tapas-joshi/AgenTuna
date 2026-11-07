//! `agentuna drift` (TC-031): list open drift events, then resolve one.
//!
//! `adopt` takes the on-disk edit into the store; `restore` writes the store copy over
//! the drifted file. Both write, so they need `--yes`. `--dry-run` reads the event from
//! the index without taking the write lock or calling the engine.

use at_db::drift::{DriftEvent, drift_event, open_drift_events};
use at_engine::drift::{
    DriftAdoptRequest, DriftResolutionError, DriftRestoreRequest, adopt_drift, restore_drift,
};
use at_model::Scope;
use at_store::paths::AppPaths;
use clap::{Arg, ArgMatches, Command};
use serde_json::{Value, json};

use crate::adopt::store_scope_for_item;
use crate::error::{CliError, ErrorCode};
use crate::globals::{GlobalArgs, ScopeArg};
use crate::migrate::authorized_roots;
use crate::output::{CommandOutput, Warning};
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, open_index_read_only, resolve_paths};

/// `agentuna drift list|adopt|restore`: open drift events and their resolutions.
pub struct DriftGroup;

impl CommandGroup for DriftGroup {
    fn name(&self) -> &'static str {
        "drift"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("drift")
                .about("List open drift events, or resolve one by adopting or restoring it")
                .subcommand_required(true)
                .subcommand(
                    Command::new("list")
                        .about("List open drift events, rescanning deployment targets first")
                        .arg(
                            Arg::new("refresh")
                                .long("refresh")
                                .action(clap::ArgAction::SetTrue)
                                .help(
                                    "Fail instead of listing stale events when detection cannot run",
                                ),
                        ),
                )
                .subcommand(
                    Command::new("adopt")
                        .about("Adopt the on-disk edit into the store (needs --yes)")
                        .arg(event_arg()),
                )
                .subcommand(
                    Command::new("restore")
                        .about("Restore the store copy over the drifted file (needs --yes)")
                        .arg(event_arg()),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        match matches.subcommand() {
            Some(("list", sub)) => list(globals, sub),
            Some(("adopt", sub)) => resolve(globals, sub, Resolution::Adopt),
            Some(("restore", sub)) => resolve(globals, sub, Resolution::Restore),
            _ => Err(CliError::new(
                ErrorCode::Usage,
                "drift needs a subcommand: list, adopt or restore",
                "run `agentuna drift --help`",
            )),
        }
    }
}

/// The write-side action taken on one drift event.
#[derive(Clone, Copy)]
enum Resolution {
    /// Take the on-disk edit into the store.
    Adopt,
    /// Write the store copy over the drifted file.
    Restore,
}

impl Resolution {
    const fn verb(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::Restore => "restore",
        }
    }

    const fn past(self) -> &'static str {
        match self {
            Self::Adopt => "Adopted",
            Self::Restore => "Restored",
        }
    }
}

fn event_arg() -> Arg {
    Arg::new("event")
        .value_name("EVENT_ID")
        .value_parser(clap::value_parser!(i64))
        .help(
            "Drift event id from `agentuna drift list`; without it the refusal names that \
             command and an example",
        )
}

/// `agentuna drift list`: rescan the indexed deployments, then list the open events.
///
/// The scan hashes every recorded deployment path against its baseline and records a
/// `drift_events` row per difference; it never writes an agent file. Detection needs the
/// write lock because it records those rows, so a running app (which owns the watcher) makes
/// the CLI list the events already recorded and say so in a warning. `--refresh` turns that
/// fallback into an error for a script that must not read stale state, and is refused
/// together with `--read-only` because detection cannot write there.
fn list(globals: &GlobalArgs, sub: &ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    // The read-only opener owns the not-found message for a machine without an index.
    drop(open_index_read_only(&paths)?);
    let strict = sub.get_flag("refresh");
    let mut warnings = Vec::new();
    let mut detected = None;
    if globals.read_only {
        if strict {
            return Err(CliError::new(
                ErrorCode::Usage,
                "`drift list --refresh` records detection rows and cannot run in read-only mode",
                "drop --read-only, or list the events the index already records",
            ));
        }
    } else {
        match CliAccess::write(&paths) {
            Ok(access) => {
                let database = open_index(&paths)?;
                let notices = at_engine::drift::reconcile_indexed(&database).map_err(db_error)?;
                drop(database);
                access.release();
                detected = Some(notices.len());
            }
            Err(error) if error.code() == ErrorCode::Locked && !strict => {
                warnings.push(Warning::new(
                    "drift_refresh",
                    "another AgenTuna process holds the store lock; listing the events already recorded",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    let index = open_index_read_only(&paths)?;
    let events =
        open_drift_events(index.connection(), globals.scope.map(Scope::from)).map_err(db_error)?;
    let human = if events.is_empty() {
        "No open drift events\n".to_owned()
    } else {
        events
            .iter()
            .map(|event| {
                format!(
                    "{}  {}  {}  {}\n",
                    event.id,
                    ScopeArg::from(event.scope).as_str(),
                    event.agent_id,
                    event.path
                )
            })
            .collect()
    };
    let data = json!({
        "events": events.iter().map(event_json).collect::<Vec<_>>(),
        "refreshed": detected.is_some(),
        "detected": detected.unwrap_or(0),
    });
    Ok(CommandOutput::new(data, human).with_warnings(warnings))
}

/// `agentuna drift adopt|restore <EVENT_ID>`: preview with `--dry-run`, otherwise a
/// confirmed write through the engine.
fn resolve(
    globals: &GlobalArgs,
    sub: &ArgMatches,
    action: Resolution,
) -> Result<CommandOutput, CliError> {
    let verb = action.verb();
    let Some(event_id) = sub.get_one::<i64>("event").copied() else {
        return Err(missing_event(verb));
    };
    let paths = resolve_paths(globals)?;

    if globals.dry_run {
        let index = open_index_read_only(&paths)?;
        let event = drift_event(index.connection(), event_id)
            .map_err(db_error)?
            .ok_or_else(|| event_not_found(event_id))?;
        if !event.is_open() {
            return Err(already_resolved(event_id));
        }
        return Ok(CommandOutput::new(
            json!({ "dry_run": true, "action": verb, "event": event_json(&event) }),
            format!(
                "Would {verb} drift event {event_id} for `{}`; nothing was written\n",
                event.path
            ),
        ));
    }
    if globals.read_only {
        // A typed refusal must come before the event lookup: in read-only mode the command can
        // never write, so a missing event id must not be what the caller hears about.
        return Err(CliError::new(
            ErrorCode::Locked,
            format!("drift {verb} writes files and cannot run in read-only mode"),
            format!(
                "drop --read-only, or preview with `agentuna drift {verb} {event_id} --dry-run`"
            ),
        ));
    }
    if !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            format!("drift {verb} writes files and needs --yes"),
            format!(
                "preview with `agentuna drift {verb} {event_id} --dry-run`, then rerun with --yes"
            ),
        ));
    }

    let _access = CliAccess::write(&paths)?;
    let database = open_index(&paths)?;
    let event = drift_event(database.connection(), event_id)
        .map_err(db_error)?
        .ok_or_else(|| event_not_found(event_id))?;
    let (_, store_root) = store_scope_for_item(&paths, event.scope)?;
    let authorized = authorized_roots(&paths)?;
    let backup_root = paths.backup_dir();
    let connection = database.connection();
    let outcome = match action {
        Resolution::Adopt => adopt_drift(DriftAdoptRequest {
            event_id,
            store_root: &store_root,
            backup_root,
            authorized_roots: &authorized,
            connection,
            actor: at_db::ops::OperationActor::Cli,
        }),
        Resolution::Restore => restore_drift(DriftRestoreRequest {
            event_id,
            store_root: &store_root,
            backup_root,
            authorized_roots: &authorized,
            connection,
            // The MCP projection reads the agent's capability declaration from its home.
            home: paths.home_dir().unwrap_or_else(|| paths.user_store()),
            actor: at_db::ops::OperationActor::Cli,
        }),
    }
    .map_err(|error| drift_error(&error))?;

    let touched: Vec<String> = outcome
        .touched_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    Ok(CommandOutput::new(
        json!({
            "action": verb,
            "event_id": outcome.event_id,
            "operation_id": outcome.operation_id,
            "touched_paths": touched,
        }),
        format!(
            "{} drift event {} — operation {}\n",
            action.past(),
            outcome.event_id,
            outcome.operation_id
        ),
    ))
}

/// Maps an engine refusal to a stable CLI error code.
fn drift_error(error: &DriftResolutionError) -> CliError {
    let message = error.to_string();
    match error {
        DriftResolutionError::EventNotFound(id) => event_not_found(*id),
        DriftResolutionError::AlreadyResolved(id) => already_resolved(*id),
        DriftResolutionError::DriftChangedAgain { .. } => CliError::new(
            ErrorCode::DriftChangedAgain,
            message,
            "the file changed again on disk; run `agentuna drift list` and retry",
        ),
        DriftResolutionError::OrphanedDrift | DriftResolutionError::Apply(_) => CliError::new(
            ErrorCode::Conflict,
            message,
            "the item or store changed; run `agentuna drift list` and retry",
        ),
        _ => CliError::new(
            ErrorCode::Internal,
            message,
            "run `agentuna doctor` and retry",
        ),
    }
}

/// The usage refusal for `drift adopt|restore` without an event id.
///
/// TC-031 takes the event id as a positional (`drift restore <EVENT_ID>`), so it stays
/// required; this refusal adds what clap cannot: where the ids come from and the exact
/// command to run next, including the dry run that changes nothing.
fn missing_event(verb: &str) -> CliError {
    CliError::new(
        ErrorCode::Usage,
        format!("drift {verb} needs an event id"),
        format!(
            "list the open events with `agentuna drift list`, then run \
             `agentuna drift {verb} 1 --dry-run` (replace 1 with the id you want)"
        ),
    )
}

fn event_not_found(id: i64) -> CliError {
    CliError::new(
        ErrorCode::NotFound,
        format!("drift event {id} was not found"),
        "list open events with `agentuna drift list`",
    )
}

fn already_resolved(id: i64) -> CliError {
    CliError::new(
        ErrorCode::Conflict,
        format!("drift event {id} is already resolved"),
        "list open events with `agentuna drift list`",
    )
}

/// Opens the writable index that an engine call needs.
pub(crate) fn open_index(paths: &AppPaths) -> Result<at_db::Database, CliError> {
    at_db::Database::open_file(paths.db_path()).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            format!("cannot open the AgenTuna index: {error}"),
            "run `agentuna doctor` to create or repair the index",
        )
    })
}

/// Maps an index read failure to an internal error with a repair hint.
pub(crate) fn db_error(error: at_db::DbError) -> CliError {
    CliError::new(
        ErrorCode::Internal,
        format!("cannot read the AgenTuna index: {error}"),
        "run `agentuna doctor` to check the index",
    )
}

fn event_json(event: &DriftEvent) -> Value {
    json!({
        "id": event.id,
        "item_id": event.item_id,
        "agent_id": event.agent_id.to_string(),
        "scope": ScopeArg::from(event.scope).as_str(),
        "path": event.path,
        "expected_hash": event.expected_hash,
        "actual_hash": event.actual_hash,
        "detected_at": event.detected_at,
        "resolved_at": event.resolved_at,
        "backup_id": event.backup_id,
        "open": event.is_open(),
    })
}
