//! Scan native agent items, conflict detection, and migration planner.

/// Native discovery, conflict analysis, reviewed plans and migration operations.
pub mod scan;

/// Compare copies of the same native item and identify conflicts.
pub mod conflicts;

/// Build reviewed plans and exact dry-run rows.
pub mod plan;

/// Fixtures and a round-trip harness for migration safety tests. Compiled only with the
/// `testkit` cargo feature.
#[cfg(feature = "testkit")]
pub mod testkit;
