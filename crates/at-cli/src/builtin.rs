//! Commands this slice implements itself.
//!
//! `doctor` (TU-032), `projects` and `scan` (TU-027) are implemented here; the
//! rest of the tree is contributed by domain groups through [`Registry`].

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
        RootCommand::Projects { command } => {
            crate::projects::dispatch_projects(&cli.global, command)
        }
        RootCommand::Scan { depth } => crate::projects::dispatch_scan(&cli.global, *depth),
        RootCommand::Doctor { fix } => crate::doctor::dispatch(&cli.global, *fix),
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
