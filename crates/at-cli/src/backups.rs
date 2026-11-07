//! `agentuna backups` (TC-031): browse the backup catalogue and restore one entry over
//! its original path.
//!
//! `restore` replaces the current file, so it needs `--yes`. When the target changed
//! since the backup, the engine issues a confirmation token first; the CLI obtains that
//! token and confirms it inside the same locked call, so nothing is written without the
//! engine's stale-target check. `--dry-run` only reads the catalogue row.

use at_db::backups::{Backup, BackupQuery, backup_by_id, list_backups};
use at_db::ops::OperationActor;
use at_fs::{RestoreError, RestoreOutcome, RestoreRequest, restore_backup};
use at_model::Scope;
use clap::{Arg, ArgMatches, Command};
use serde_json::{Value, json};

use crate::adopt::store_scope_for_item;
use crate::drift::{db_error, open_index};
use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;
use crate::registry::CommandGroup;
use crate::runtime::{CliAccess, open_index_read_only, resolve_paths};

/// `agentuna backups list|restore`: the backup catalogue and single-entry restore.
pub struct BackupsGroup;

impl CommandGroup for BackupsGroup {
    fn name(&self) -> &'static str {
        "backups"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("backups")
                .about("Browse the backup catalogue and restore one backup")
                .subcommand_required(true)
                .subcommand(
                    Command::new("list")
                        .about("List backups, newest first")
                        .arg(
                            Arg::new("path")
                                .long("path")
                                .value_name("PREFIX")
                                .help("Only backups whose original path starts with PREFIX"),
                        ),
                )
                .subcommand(
                    Command::new("restore")
                        .about("Restore one backup over its original path (needs --yes)")
                        .arg(
                            Arg::new("backup")
                                .value_name("BACKUP_ID")
                                .required(true)
                                .help("Backup id from `agentuna backups list`"),
                        ),
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
            Some(("restore", sub)) => restore(globals, sub),
            _ => Err(CliError::new(
                ErrorCode::Usage,
                "backups needs a subcommand: list or restore",
                "run `agentuna backups --help`",
            )),
        }
    }
}

/// `agentuna backups list`: catalogue rows, optionally narrowed by `--path`.
fn list(globals: &GlobalArgs, sub: &ArgMatches) -> Result<CommandOutput, CliError> {
    let paths = resolve_paths(globals)?;
    let index = open_index_read_only(&paths)?;
    let query = BackupQuery {
        path_prefix: sub.get_one::<String>("path").map(String::as_str),
        ..BackupQuery::default()
    };
    let backups = list_backups(index.connection(), query).map_err(db_error)?;
    let human = if backups.is_empty() {
        "No backups\n".to_owned()
    } else {
        backups
            .iter()
            .map(|backup| {
                format!(
                    "{}  {}  {}\n",
                    backup.id,
                    backup.created_at,
                    backup.original_path.as_deref().unwrap_or("-")
                )
            })
            .collect()
    };
    let data = json!({ "backups": backups.iter().map(backup_json).collect::<Vec<_>>() });
    Ok(CommandOutput::new(data, human))
}

/// `agentuna backups restore <BACKUP_ID>`: preview with `--dry-run`, otherwise a
/// confirmed restore through the engine.
fn restore(globals: &GlobalArgs, sub: &ArgMatches) -> Result<CommandOutput, CliError> {
    let backup_id = sub
        .get_one::<String>("backup")
        .map(String::as_str)
        .unwrap_or_default();
    let access = CliAccess::for_command(globals, !globals.dry_run)?;
    if !globals.dry_run {
        access.ensure_writable()?;
    }
    let paths = access.paths().clone();

    if globals.dry_run {
        let index = open_index_read_only(&paths)?;
        let backup = backup_by_id(index.connection(), backup_id)
            .map_err(db_error)?
            .ok_or_else(|| backup_not_found(backup_id))?;
        return Ok(CommandOutput::new(
            json!({ "dry_run": true, "action": "restore", "backup": backup_json(&backup) }),
            format!(
                "Would restore backup {} over {}; nothing was written\n",
                backup.id,
                backup.original_path.as_deref().unwrap_or("-")
            ),
        ));
    }
    if !globals.yes {
        return Err(CliError::new(
            ErrorCode::ConfirmationRequired,
            "backups restore overwrites the current file and needs --yes",
            format!(
                "preview with `agentuna backups restore {backup_id} --dry-run`, then rerun with --yes"
            ),
        ));
    }

    let scope = globals.scope.map_or(Scope::User, Scope::from);
    let database = open_index(&paths)?;
    let (_, store_root) = store_scope_for_item(&paths, scope)?;
    // The change-set writer records canonical paths, and `restore_backup` bounds the target
    // lexically, so hand it the same canonical root or a valid restore looks like an escape.
    let store_root = store_root.canonicalize().unwrap_or(store_root);
    let connection = database.connection();
    let first = restore_backup(RestoreRequest {
        connection,
        store_root: &store_root,
        backup_id,
        confirmation: None,
        actor: OperationActor::Cli,
    })
    .map_err(|error| restore_error(&error))?;
    let outcome = match first {
        RestoreOutcome::ConfirmationRequired { token, .. } => restore_backup(RestoreRequest {
            connection,
            store_root: &store_root,
            backup_id,
            confirmation: Some(&token),
            actor: OperationActor::Cli,
        })
        .map_err(|error| restore_error(&error))?,
        other => other,
    };

    match outcome {
        RestoreOutcome::Restored {
            operation_id,
            target,
        } => Ok(CommandOutput::new(
            json!({
                "backup_id": backup_id,
                "operation_id": operation_id,
                "target": target.display().to_string(),
            }),
            format!(
                "Restored backup {backup_id} to {} — operation {operation_id}\n",
                target.display()
            ),
        )),
        _ => Err(CliError::new(
            ErrorCode::Conflict,
            "restore did not complete; the target was not confirmed",
            "run `agentuna backups list` and retry",
        )),
    }
}

/// Maps a restore refusal to a stable CLI error code.
fn restore_error(error: &RestoreError) -> CliError {
    let message = error.to_string();
    match error {
        RestoreError::NotFound(id) => backup_not_found(id),
        RestoreError::StaleConfirmation => CliError::new(
            ErrorCode::TargetChanged,
            message,
            "the target changed after the backup; run the restore again so it is confirmed",
        ),
        RestoreError::Fs(at_fs::FsError::ConcurrentModification { .. }) => CliError::new(
            ErrorCode::TargetChanged,
            message,
            "the target changed during the restore; review it and retry",
        ),
        RestoreError::OutsideRoot => CliError::new(
            ErrorCode::Validation,
            message,
            "pass --scope matching the store the backup was taken from",
        ),
        _ => CliError::new(
            ErrorCode::Internal,
            message,
            "run `agentuna doctor` and retry",
        ),
    }
}

fn backup_not_found(id: &str) -> CliError {
    CliError::new(
        ErrorCode::NotFound,
        format!("backup {id} was not found"),
        "list backups with `agentuna backups list`",
    )
}

fn backup_json(backup: &Backup) -> Value {
    json!({
        "id": backup.id,
        "operation_id": backup.operation_id,
        "original_path": backup.original_path,
        "backup_path": backup.backup_path,
        "content_hash": backup.content_hash,
        "size": backup.size,
        "created_at": backup.created_at,
        "pinned": backup.pinned,
        "expires_at": backup.expires_at,
        "restore_count": backup.restore_count,
    })
}
