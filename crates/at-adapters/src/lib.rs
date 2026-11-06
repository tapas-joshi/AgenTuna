//! Agent detection, native paths, capabilities, and adapters.

pub mod adapter;
pub mod claude_mcp;
pub mod codex_mcp;
pub mod generic_mcp;
pub mod mcp_capabilities;
pub mod omp_mcp;
pub mod paths;

/// Golden-file round-trip harness for the format-preserving adapters.
///
/// Test support only: compiled for this crate's tests and included by path from
/// `tests/golden.rs`, so it is absent from production builds.
#[cfg(test)]
pub mod golden;
