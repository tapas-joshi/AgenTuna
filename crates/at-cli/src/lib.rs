//! The `agentuna` command line: tree, global flags, output envelope and exit codes.
//!
//! The binary is a thin front end. `at-cli` owns the tree, argument parsing, the
//! versioned JSON envelope and the exit-code mapping; every noun-first domain group
//! (`skills`, `mcp`, `migrate`, ...) is contributed by its owning slice through
//! [`CommandGroup`] and carries the engine calls.
//!
//! Design rules this crate enforces:
//!
//! - `--json` writes exactly one object to stdout; diagnostics, usage text and
//!   progress go to stderr.
//! - A command that exists in the tree without an implementation fails with a typed
//!   [`CliError`] and exit code 11; it never reports success it did not produce.
//! - Only implemented groups are registered, so `--help` lists exactly what works
//!   plus the commands this slice owns.

mod builtin;
pub mod cli;
pub mod error;
pub mod globals;
pub mod output;
pub mod registry;
mod run;

pub use cli::{Cli, CompletionShell, ProjectsCommand, RootCommand};
pub use error::{CliError, ErrorCode, exit_code};
pub use globals::{GlobalArgs, ScopeArg};
pub use output::{Color, CommandOutput, SCHEMA_VERSION, Stdio, Warning};
pub use registry::{CommandGroup, Registry, RegistryError};
pub use run::{production_registry, run, run_from_env};
