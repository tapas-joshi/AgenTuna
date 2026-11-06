//! Scan native agent items, conflict detection, and migration planner.

/// Fixtures and a round-trip harness for migration safety tests. Compiled only with the
/// `testkit` cargo feature.
#[cfg(feature = "testkit")]
pub mod testkit;
