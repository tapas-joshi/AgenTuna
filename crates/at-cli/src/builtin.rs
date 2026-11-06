//! Commands this slice implements itself.
//!
//! `projects`, `scan` and `doctor` exist in the tree so `--help` is honest about the
//! planned surface, but their engine work is owned by TU-027 and TU-032. Until those
//! land they return a typed not-implemented error and a distinct exit code instead of
//! pretending to succeed.

use serde_json::json;

use crate::cli::{Cli, CompletionShell, RootCommand};
use crate::error::CliError;
use crate::output::CommandOutput;
use crate::registry::Registry;

/// Executes a built-in command parsed from [`Cli`].
pub(crate) fn dispatch(cli: &Cli, registry: &Registry) -> Result<CommandOutput, CliError> {
    match &cli.command {
        RootCommand::Version => Ok(version()),
        RootCommand::Completions { shell } => Ok(completions(registry, *shell)),
        RootCommand::Projects { .. } => Err(CliError::not_implemented("projects", "TU-027")),
        RootCommand::Scan { .. } => Err(CliError::not_implemented("scan", "TU-027")),
        RootCommand::Doctor { .. } => Err(CliError::not_implemented("doctor", "TU-032")),
    }
}

fn version() -> CommandOutput {
    let version = env!("CARGO_PKG_VERSION");
    CommandOutput::new(
        json!({ "name": "agentuna", "version": version }),
        format!("agentuna {version}\n"),
    )
}

fn completions(registry: &Registry, shell: CompletionShell) -> CommandOutput {
    let mut script = Vec::new();
    let mut command = registry.command();
    clap_complete::generate(shell.generator(), &mut command, "agentuna", &mut script);
    let script = String::from_utf8_lossy(&script).into_owned();
    CommandOutput::new(
        json!({ "shell": shell.as_str(), "script": script.clone() }),
        script,
    )
}
