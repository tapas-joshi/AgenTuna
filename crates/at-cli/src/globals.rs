//! Flags every command accepts, before or after its own arguments.

use std::path::PathBuf;

use at_model::Scope;
use clap::{ArgAction, Args, ValueEnum};

/// A `--scope` value on the command line.
///
/// Mirrors [`Scope`] as a [`ValueEnum`] so the parser, `--help` and shell completions
/// all use the same wire names without giving `at-model` a dependency on clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum ScopeArg {
    /// Machine or organisation level; may lock items.
    #[value(name = "global")]
    Global,
    /// The current user, all repositories.
    #[value(name = "user")]
    User,
    /// One repository, committed to Git.
    #[value(name = "project")]
    Project,
    /// The current user in one repository; uncommitted.
    #[value(name = "local")]
    Local,
}

impl ScopeArg {
    /// The lowercase wire name shared by the CLI, JSON and SQLite.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ScopeArg::Global => "global",
            ScopeArg::User => "user",
            ScopeArg::Project => "project",
            ScopeArg::Local => "local",
        }
    }
}

impl From<ScopeArg> for Scope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::Global => Scope::Global,
            ScopeArg::User => Scope::User,
            ScopeArg::Project => Scope::Project,
            ScopeArg::Local => Scope::Local,
        }
    }
}

impl From<Scope> for ScopeArg {
    fn from(value: Scope) -> Self {
        match value {
            Scope::Global => ScopeArg::Global,
            Scope::User => ScopeArg::User,
            Scope::Project => ScopeArg::Project,
            Scope::Local => ScopeArg::Local,
        }
    }
}

/// Flags accepted by every command, whether written before or after the subcommand.
///
/// A [`CommandGroup`](crate::registry::CommandGroup) must not redeclare these; clap
/// propagates them into every subcommand, and the group reads them from this struct.
#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Emit one machine-readable JSON object on stdout; diagnostics stay on stderr.
    #[arg(long, global = true)]
    pub json: bool,

    /// Project directory to operate on instead of the one above the working directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Scope to read or write: global, user, project or local.
    #[arg(long, global = true, value_name = "SCOPE")]
    pub scope: Option<ScopeArg>,

    /// Assume yes for confirmations that would otherwise be asked.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Report what a command would change without writing anything.
    ///
    /// Every writing command must honour it; `doctor --fix --dry-run` lists the
    /// repairs it would perform and touches nothing.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Open the store read-only: SQLite `query_only`, no migrations, writes refused.
    #[arg(long, global = true)]
    pub read_only: bool,

    /// Increase diagnostic output; repeat for more detail.
    #[arg(long, short = 'v', global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Never write ANSI colour, even to a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,
}

impl GlobalArgs {
    /// The selected scope, if the user asked for one.
    #[must_use]
    pub fn scope(&self) -> Option<Scope> {
        self.scope.map(Scope::from)
    }
}
