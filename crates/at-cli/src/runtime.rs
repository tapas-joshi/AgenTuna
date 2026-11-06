//! Runtime configuration and lock cooperation for the CLI (TF-028, TF-051).
//!
//! This module is supplied by the platform slice; the command *tree* stays owned by the CLI
//! slice, which registers [`ConfigGroup`] and the global `--read-only` flag.
//!
//! ## `agentuna config`
//!
//! Prints the effective feature flags with value, compiled default and source, from the same
//! [`at_store::config`] engine the desktop app uses. `--json` returns exactly the object
//! `diagnostics.json` embeds, so a bug report and the settings screen cannot disagree.
//!
//! ## Lock cooperation
//!
//! Every command that touches the store takes the same advisory lock as the app
//! (`<support>/instance.lock`, see [`at_store::instance`]):
//!
//! - [`CliAccess::read`] — shared lock, fails fast with [`ErrorCode::Locked`];
//! - [`CliAccess::write`] — exclusive lock for one transaction, waiting up to five seconds
//!   (the SQLite busy timeout) and then reporting the holder's pid and version;
//! - [`CliAccess::read_only`] — shared lock, never creates the lock file, and
//!   [`CliAccess::ensure_writable`] rejects every write.
//!
//! A CLI write while the app is idle proceeds: the idle app holds no OS lock, only its record,
//! which the CLI leaves untouched so a second app launch still sees a live instance.
//!
//! Read-only mode additionally requires the index to be opened with `PRAGMA query_only=1` and
//! without migrations; that lives in the DB layer, not here.

use std::fmt::Write as _;

use at_db::{ReadOnlyDatabase, ReadOnlyError};
use at_store::config::{CONFIG_FILE_NAME, ConfigInput, RuntimeConfig};
use at_store::instance::{
    InstanceGuard, InstanceLock, InstanceRecord, LockError, LockPolicy, Mode,
};
use at_store::paths::{AppPaths, PathEnvironment};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::{CommandOutput, Warning};
use crate::registry::CommandGroup;

/// `agentuna config`: show the effective runtime configuration.
///
/// Registered by the CLI slice in `production_registry`, because that is where the tree is
/// composed.
pub struct ConfigGroup;

impl CommandGroup for ConfigGroup {
    fn name(&self) -> &'static str {
        "config"
    }

    fn attach(&self, root: Command) -> Command {
        root.subcommand(
            Command::new("config")
                .about("Show the effective runtime configuration and feature flags")
                .long_about(
                    "Print every feature flag with its effective value, compiled default and the \
                     source it came from: command line, AGENTUNA_FLAG_<NAME>, agentuna.toml or \
                     the compiled default. Flags are read once at startup, so changing one \
                     requires a restart.",
                )
                .arg(
                    Arg::new("flag")
                        .long("flag")
                        .value_name("VENDOR:NAME=VALUE")
                        .action(ArgAction::Append)
                        .help(
                            "Override a flag for this invocation, e.g. agentuna:marketplace=true",
                        ),
                ),
        )
    }

    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError> {
        let cli_flags: Vec<String> = matches
            .get_many::<String>("flag")
            .map(|values| values.cloned().collect())
            .unwrap_or_default();
        let paths = resolve_paths(globals)?;
        let config = load_config(&paths, cli_flags, !globals.read_only)?;
        Ok(config_output(&config))
    }
}

/// Resolves the store roots for a CLI invocation.
///
/// # Errors
/// The roots cannot be resolved (no home directory, project root missing or not a directory).
pub fn resolve_paths(globals: &GlobalArgs) -> Result<AppPaths, CliError> {
    let mut environment = PathEnvironment::from_process();
    if let Some(project) = &globals.project {
        environment = environment.with_project_root(project);
    }
    AppPaths::resolve(&environment).map_err(|error| {
        CliError::new(
            ErrorCode::Validation,
            format!("cannot resolve the AgenTuna directories: {error}"),
            "pass --project with an existing directory, or set AGENTUNA_HOME",
        )
    })
}

/// Loads the runtime configuration through the shared engine.
///
/// # Errors
/// Only a failure to create the file on first run is fatal; an unwritable file with `create`
/// set to `false` (read-only mode) falls back to the compiled defaults.
pub fn load_config(
    paths: &AppPaths,
    cli_flags: Vec<String>,
    create: bool,
) -> Result<RuntimeConfig, CliError> {
    let path = paths.support_dir().join(CONFIG_FILE_NAME);
    let input = ConfigInput {
        create,
        ..ConfigInput::production(cli_flags)
    };
    RuntimeConfig::load(path, input).map_err(|error| {
        CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "fix the permissions of the support directory, or set AGENTUNA_HOME",
        )
    })
}

/// Opens `<support>/agentuna.db` read-only for a `--read-only` command.
///
/// The handle sets `PRAGMA query_only=1` and never migrates, so a read-only invocation cannot
/// change the index; [`ReadOnlyDatabase::execute`] reports a rejected write as
/// [`ReadOnlyError::ReadOnly`].
///
/// # Errors
/// [`ErrorCode::NotFound`] when the index does not exist yet (read-only mode never creates it),
/// or a conflict for any other SQLite failure.
pub fn open_index_read_only(paths: &AppPaths) -> Result<ReadOnlyDatabase, CliError> {
    ReadOnlyDatabase::open(paths.db_path()).map_err(|error| match error {
        ReadOnlyError::Open { .. } => CliError::new(
            ErrorCode::NotFound,
            error.to_string(),
            "run a normal command first so the index exists, or drop --read-only",
        ),
        other => CliError::new(
            ErrorCode::Conflict,
            other.to_string(),
            "check the index file and retry",
        ),
    })
}

/// Renders a loaded configuration into the JSON payload and the human table.
fn config_output(config: &RuntimeConfig) -> CommandOutput {
    let warnings = config
        .report()
        .warnings
        .iter()
        .map(|warning| {
            let mut message = warning.message.clone();
            if let Some(suggestion) = &warning.suggestion {
                let _ = write!(message, " (did you mean `{suggestion}`?)");
            }
            Warning::new("config", message)
        })
        .collect();
    CommandOutput::new(config.diagnostics_section(), render_human(config)).with_warnings(warnings)
}

/// Human rendering: one aligned row per flag and advanced value, then the warnings.
fn render_human(config: &RuntimeConfig) -> String {
    let report = config.report();
    let mut out = String::new();
    let _ = writeln!(out, "config: {}", report.config_path);
    let _ = writeln!(
        out,
        "restart required: {} \u{2014} {}",
        if report.requires_restart { "yes" } else { "no" },
        report.restart_note
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "{:<24} {:<6} {:<8} SOURCE", "FLAG", "VALUE", "DEFAULT");
    for (key, entry) in &report.flags {
        let _ = writeln!(
            out,
            "{key:<24} {:<6} {:<8} {}",
            entry.value,
            entry.default,
            entry.source.as_str()
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<30} {:<8} {:<8} SOURCE",
        "ADVANCED", "VALUE", "DEFAULT"
    );
    for (key, entry) in &report.advanced {
        let _ = writeln!(
            out,
            "{key:<30} {:<8} {:<8} {}",
            render_value(&entry.value),
            render_value(&entry.default),
            entry.source.as_str()
        );
    }
    if !report.warnings.is_empty() {
        let _ = writeln!(out);
        for warning in &report.warnings {
            let _ = match &warning.suggestion {
                Some(suggestion) => writeln!(
                    out,
                    "warning: {} (did you mean `{suggestion}`?) \u{2014} {}",
                    warning.key, warning.message
                ),
                None => writeln!(out, "warning: {} \u{2014} {}", warning.key, warning.message),
            };
        }
    }
    out
}

/// A JSON scalar as it appears in the human table.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "-".to_owned(),
        other => other.to_string(),
    }
}

/// What a CLI command was given while holding the lock.
#[derive(Debug)]
pub struct CliAccess {
    paths: AppPaths,
    guard: Option<InstanceGuard>,
}

impl CliAccess {
    /// Shared lock for a read; fails fast when a writer holds the lock.
    ///
    /// # Errors
    /// `ErrorCode::Locked` when the lock is busy, or a conflict for a filesystem failure.
    pub fn read(paths: &AppPaths) -> Result<Self, CliError> {
        Self::acquire(paths, Mode::CliRead, LockPolicy::NONBLOCKING)
    }

    /// Exclusive lock for one write transaction, waiting up to the SQLite busy timeout.
    ///
    /// # Errors
    /// `ErrorCode::Locked` with the holder's pid and version after the wait expires.
    pub fn write(paths: &AppPaths) -> Result<Self, CliError> {
        Self::acquire(paths, Mode::CliWrite, LockPolicy::CLI_TRANSACTION)
    }

    /// Read-only access: no lock file creation, no record, and writes rejected.
    ///
    /// # Errors
    /// `ErrorCode::Locked` when an exclusive holder blocks the shared lock.
    pub fn read_only(paths: &AppPaths) -> Result<Self, CliError> {
        Self::acquire(paths, Mode::ReadOnly, LockPolicy::NONBLOCKING)
    }

    /// Chooses the mode from `--read-only` and whether the command writes.
    ///
    /// # Errors
    /// Same as the constructor for the chosen mode, and when the roots cannot be resolved.
    pub fn for_command(globals: &GlobalArgs, writes: bool) -> Result<Self, CliError> {
        let paths = resolve_paths(globals)?;
        if globals.read_only {
            Self::read_only(&paths)
        } else if writes {
            Self::write(&paths)
        } else {
            Self::read(&paths)
        }
    }

    fn acquire(paths: &AppPaths, mode: Mode, policy: LockPolicy) -> Result<Self, CliError> {
        let guard = InstanceLock::new(paths.support_dir())
            .acquire(mode, policy)
            .map_err(lock_error_to_cli)?;
        Ok(Self {
            paths: paths.clone(),
            guard: Some(guard),
        })
    }

    /// The record the guard read or wrote.
    ///
    /// # Errors
    /// Only when the guard was already released; callers do not normally hit this.
    pub fn record(&self) -> Result<&InstanceRecord, CliError> {
        self.guard
            .as_ref()
            .map(InstanceGuard::record)
            .ok_or_else(|| CliError::internal("the CLI access guard was already released"))
    }

    /// Rejects a write in read-only (and read) mode.
    ///
    /// # Errors
    /// `ErrorCode::Locked` with the read-only message.
    pub fn ensure_writable(&self) -> Result<(), CliError> {
        match &self.guard {
            Some(guard) => guard.ensure_writable().map_err(lock_error_to_cli),
            None => Err(lock_error_to_cli(LockError::ReadOnly)),
        }
    }

    /// Roots resolved for this command.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Releases the lock now instead of at the end of the command.
    pub fn release(mut self) {
        if let Some(guard) = self.guard.take() {
            guard.release();
        }
    }
}

/// Maps a lock failure to the CLI error table.
///
/// Busy and read-only failures use [`ErrorCode::Locked`]: the invocation is valid, another
/// process or the selected mode blocked it. The holder's pid and version are in the message so
/// a script or an agent can report exactly who to wait for.
#[must_use]
pub fn lock_error_to_cli(error: LockError) -> CliError {
    match &error {
        LockError::Busy {
            holder: Some(holder),
        } => CliError::new(
            ErrorCode::Locked,
            format!(
                "the store lock is held by pid {} (version {}, mode {})",
                holder.pid, holder.version, holder.mode
            ),
            "wait for the running operation to finish, then retry",
        )
        .with_details(json!({
            "holder": {
                "pid": holder.pid,
                "version": holder.version,
                "started_at": holder.started_at,
                "mode": holder.mode.as_str(),
            }
        })),
        LockError::Busy { holder: None } => CliError::new(
            ErrorCode::Locked,
            "the store lock is held by another AgenTuna process",
            "wait for the running operation to finish, then retry",
        ),
        LockError::AlreadyRunning { holder } => CliError::new(
            ErrorCode::Locked,
            format!(
                "AgenTuna is running (pid {}, version {}); the command needs the app to be idle",
                holder.pid, holder.version
            ),
            "quit the app or retry after the running operation finishes",
        ),
        LockError::ReadOnly => CliError::new(
            ErrorCode::Locked,
            "read-only mode: writes are disabled",
            "rerun without --read-only to write",
        ),
        LockError::Io { .. } => CliError::new(
            ErrorCode::Conflict,
            error.to_string(),
            "check the permissions of the support directory, or set AGENTUNA_HOME",
        ),
        _ => CliError::new(
            ErrorCode::Internal,
            error.to_string(),
            "this is a bug in agentuna; please report the command you ran",
        ),
    }
}

#[cfg(test)]
mod tests;
