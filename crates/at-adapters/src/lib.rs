//! Agent detection, native paths, capabilities, and adapters.

pub mod adapter;
pub mod claude_adapter;
pub mod claude_mcp;
pub mod claude_settings;
pub mod codex_adapter;
pub mod codex_mcp;
pub mod codex_projects;
pub mod cursor_adapter;
pub mod custom_agent;
pub mod generic_mcp;
pub mod mcp_capabilities;
pub mod mcp_projection;
pub mod native_skills;
pub mod omp_adapter;
pub mod omp_mcp;
pub mod paths;
pub mod secret_launch;

/// Golden-file round-trip harness for the format-preserving adapters.
///
/// Test support only: compiled for this crate's tests and included by path from
/// `tests/golden.rs`, so it is absent from production builds.
#[cfg(test)]
pub mod golden;
