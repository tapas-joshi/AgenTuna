//! AgenTuna CLI entry point.
//!
//! Everything lives in the `at_cli` library so integration tests and future tooling
//! drive the exact same tree and dispatch path as the shipped binary.

use std::process::ExitCode;

fn main() -> ExitCode {
    at_cli::run_from_env()
}
