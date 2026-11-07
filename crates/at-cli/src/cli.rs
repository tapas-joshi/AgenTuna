//! The root command line: global flags and the commands this slice owns.
//!
//! Domain groups are not declared here; they attach through
//! [`CommandGroup`](crate::registry::CommandGroup) so the tree lists exactly what is
//! implemented.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::globals::GlobalArgs;

/// Root command line for the `agentuna` binary.
#[derive(Debug, Parser)]
#[command(
    name = "agentuna",
    version,
    about = "Manage AI coding agent configuration across scopes",
    long_about = "Manage skills, MCP servers and rule files for coding agents across the Global, \
                  User, Project and Local scopes. Without --project, commands use the nearest Git \
                  work tree or registered project above the working directory. JSON output is \
                  stable; diagnostics go to stderr.",
    arg_required_else_help = true,
    max_term_width = 100
)]
pub struct Cli {
    /// Flags every command accepts.
    #[command(flatten)]
    pub global: GlobalArgs,

    /// The command to run.
    #[command(subcommand)]
    pub command: RootCommand,
}

/// Commands owned by this slice. Domain groups are attached separately.
#[derive(Debug, Subcommand)]
pub enum RootCommand {
    /// Manage the projects registered with AgenTuna
    #[command(subcommand_required = true)]
    Projects {
        /// What to do with the project registry.
        #[command(subcommand)]
        command: ProjectsCommand,
    },

    /// Discover configuration below the configured roots
    Scan {
        /// Directory levels to search below each root.
        #[arg(long, value_name = "LEVELS")]
        depth: Option<u32>,
    },

    /// Check the local environment and report findings
    Doctor {
        /// Apply the safe repairs that need no confirmation.
        #[arg(long)]
        fix: bool,
    },

    /// Print a completion script for the named shell
    Completions {
        /// Shell to generate a script for.
        #[arg(value_enum, value_name = "SHELL")]
        shell: CompletionShell,
    },

    /// Print version information
    Version,
}

/// Project registry subcommands.
#[derive(Debug, Subcommand)]
pub enum ProjectsCommand {
    /// List the registered projects
    List,

    /// Register a folder as a project
    Add {
        /// Folder to register.
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Stop listing a project without deleting its files or history
    Remove {
        /// Registered project path or identifier.
        #[arg(value_name = "PROJECT")]
        target: String,
    },

    /// Make a project the active one
    Use {
        /// Registered project path or identifier.
        #[arg(value_name = "PROJECT")]
        target: String,
    },
}

/// Shells `completions` can emit a script for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// Bash.
    Bash,
    /// Z shell.
    Zsh,
    /// Fish.
    Fish,
    /// Elvish.
    Elvish,
    /// PowerShell.
    #[value(name = "powershell")]
    PowerShell,
}

impl CompletionShell {
    /// The clap_complete generator for this shell.
    #[must_use]
    pub const fn generator(self) -> clap_complete::Shell {
        match self {
            CompletionShell::Bash => clap_complete::Shell::Bash,
            CompletionShell::Zsh => clap_complete::Shell::Zsh,
            CompletionShell::Fish => clap_complete::Shell::Fish,
            CompletionShell::Elvish => clap_complete::Shell::Elvish,
            CompletionShell::PowerShell => clap_complete::Shell::PowerShell,
        }
    }

    /// The lowercase shell name used in JSON payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CompletionShell::Bash => "bash",
            CompletionShell::Zsh => "zsh",
            CompletionShell::Fish => "fish",
            CompletionShell::Elvish => "elvish",
            CompletionShell::PowerShell => "powershell",
        }
    }
}
