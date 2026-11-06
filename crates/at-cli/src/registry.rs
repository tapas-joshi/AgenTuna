//! The extension point: owning slices attach their noun-first groups here.

use std::collections::HashSet;

use clap::{ArgMatches, Command, CommandFactory};

use crate::cli::Cli;
use crate::error::CliError;
use crate::globals::GlobalArgs;
use crate::output::CommandOutput;

/// A noun-first command group contributed by its owning slice.
///
/// `at-cli` supplies the surrounding tree, global flags and output encoding; a group
/// supplies its own subcommands, arguments and engine calls. Nested nouns (`mcp
/// secret`) belong inside the owning group's [`attach`](Self::attach), so the registry
/// only ever keys on the top-level noun.
///
/// A group must not redeclare the global flags (`--json`, `--project`, `--scope`,
/// `--yes`, `--verbose`, `--no-color`): they are propagated into every subcommand and
/// read from [`GlobalArgs`]. Their argument ids (`json`, `project`, `scope`, `yes`,
/// `verbose`, `no_color`) are reserved, so a positional such as a project target must
/// use a different field name even when its value name is `PROJECT`.
pub trait CommandGroup: Send + Sync {
    /// The top-level noun typed on the command line, for example `skills`.
    fn name(&self) -> &'static str;

    /// Adds this group's command tree to `root`.
    ///
    /// Called once, before parsing. The returned command must contain a subcommand
    /// named [`name`](Self::name).
    fn attach(&self, root: Command) -> Command;

    /// Runs an invocation whose top-level noun is [`name`](Self::name).
    ///
    /// `matches` are the group's own matches, so nested nouns are available through
    /// `matches.subcommand()`. Output is returned, not printed, so the root controls
    /// JSON mode, colour and exit codes.
    fn dispatch(
        &self,
        globals: &GlobalArgs,
        matches: &ArgMatches,
    ) -> Result<CommandOutput, CliError>;
}

/// The registered groups, in the order they appear in `--help`.
pub struct Registry {
    groups: Vec<Box<dyn CommandGroup>>,
}

/// Why a group could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Two groups claim the same top-level noun.
    #[error("command group `{name}` is registered twice")]
    Duplicate {
        /// The clashing noun.
        name: String,
    },
    /// A group claims a noun that `at-cli` already owns.
    #[error("command group `{name}` collides with a built-in command")]
    Reserved {
        /// The clashing noun.
        name: String,
    },
}

impl Registry {
    /// Builds a registry, rejecting duplicate and built-in names.
    ///
    /// Failures are composition-root bugs: the shipped binary reports them as an
    /// internal error instead of starting with an ambiguous tree.
    pub fn try_new(groups: Vec<Box<dyn CommandGroup>>) -> Result<Self, RegistryError> {
        let reserved = built_in_names();
        let mut seen = HashSet::new();
        for group in &groups {
            let name = group.name().to_owned();
            if !seen.insert(name.clone()) {
                return Err(RegistryError::Duplicate { name });
            }
            if reserved.contains(&name) {
                return Err(RegistryError::Reserved { name });
            }
        }
        Ok(Self { groups })
    }

    /// Every registered noun, in registration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.groups.iter().map(|group| group.name())
    }

    /// The group registered under `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn CommandGroup> {
        self.groups
            .iter()
            .find(|group| group.name() == name)
            .map(Box::as_ref)
    }

    /// The full command tree: built-in commands plus every registered group.
    #[must_use]
    pub fn command(&self) -> Command {
        let mut command = Cli::command();
        for group in &self.groups {
            command = group.attach(command);
        }
        command
    }
}

/// Nouns that [`Cli`] already declares.
fn built_in_names() -> HashSet<String> {
    let mut names: HashSet<String> = Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect();
    // clap creates the implicit `help` subcommand while building, not in `command()`.
    names.insert("help".to_owned());
    names
}
