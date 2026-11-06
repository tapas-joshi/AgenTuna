//! Agent detection, native paths, capabilities, and adapters.

pub mod claude_mcp;
pub mod codex_mcp;
pub mod omp_mcp;

/// Golden-file round-trip harness for the format-preserving adapters.
///
/// Test support only: compiled for this crate's tests and included by path from
/// `tests/golden.rs`, so it is absent from production builds.
#[cfg(test)]
pub mod golden;
