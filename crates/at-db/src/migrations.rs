//! Numbered schema migrations and the runner that applies them.
//!
//! A [`Migrations`] set is an ascending sequence starting at v1. Each version
//! runs in its own transaction and is recorded in `schema_version` together
//! with the SHA-256 of the SQL that ran. During an open the runner checks every
//! recorded row against the shipped SQL: a changed migration or a database from
//! a newer binary is refused before anything is written.
//!
//! Stories append their migration to [`SHIPPED_MIGRATIONS`] and add the
//! matching `.sql` file under `crates/at-db/migrations/` (embedded with
//! `include_str!`, never read at runtime). Numbered migrations land in waves,
//! so the registry may temporarily contain a higher version before its
//! lower-numbered neighbours: [`Migrations::shipped`] applies the longest
//! contiguous prefix starting at v1 and leaves later migrations dormant until
//! the gap is filled. A binary therefore never skips a schema step.

use std::collections::HashSet;

use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::DbError;

/// SQL for migration v1: creates only the `schema_version` bookkeeping table.
pub const MIGRATION_V1_SQL: &str = include_str!("../migrations/0001_schema_version.sql");

/// SQL for migration v2: projects, agents and agent paths (TF-011).
pub const MIGRATION_V2_SQL: &str =
    include_str!("../migrations/0002_projects_agents_agent_paths.sql");

/// SQL for migration v3: items and their content versions (TF-012).
pub const MIGRATION_V3_SQL: &str = include_str!("../migrations/0003_items_item_versions.sql");

/// SQL for migration v4: the deployment matrix (TF-013).
pub const MIGRATION_V4_SQL: &str = include_str!("../migrations/0004_deployments.sql");

/// SQL for migration v5: native items, locks and drift history (TF-014).
pub const MIGRATION_V5_SQL: &str = include_str!("../migrations/0005_native_items_drift_locks.sql");

/// SQL for migration v6: health-check history and sources (TF-015).
pub const MIGRATION_V6_SQL: &str = include_str!("../migrations/0006_health_sources.sql");

/// SQL for migration v7: operations, operation files and backups (TF-016).
pub const MIGRATION_V7_SQL: &str = include_str!("../migrations/0007_operations_backups.sql");

/// SQL for migration v8: presets and preset application history (TF-017).
pub const MIGRATION_V8_SQL: &str = include_str!("../migrations/0008_presets.sql");

/// SQL for migration v9: settings, secret references and remote backup state (TF-018).
pub const MIGRATION_V9_SQL: &str =
    include_str!("../migrations/0009_settings_secrets_remote_backup.sql");

/// SQL for migration v10: native entry link target and discovery outcome (TS-028).
pub const MIGRATION_V10_SQL: &str = include_str!("../migrations/0010_native_discovery_details.sql");

/// Name of the table that records applied migrations.
pub(crate) const SCHEMA_VERSION_TABLE: &str = "schema_version";

/// Every migration this binary ships, in ascending version order.
///
/// TF-011..TF-018 append their `include_str!` migration here (v2..v9) and add
/// the matching `.sql` file under `crates/at-db/migrations/`. Entries are
/// sorted by version; a gap is allowed while lower-numbered migrations are
/// still in flight, and [`Migrations::shipped`] only applies the contiguous
/// prefix.
pub const SHIPPED_MIGRATIONS: &[Migration] = &[
    Migration::new(1, MIGRATION_V1_SQL),
    Migration::new(2, MIGRATION_V2_SQL),
    Migration::new(3, MIGRATION_V3_SQL),
    Migration::new(4, MIGRATION_V4_SQL),
    Migration::new(5, MIGRATION_V5_SQL),
    Migration::new(6, MIGRATION_V6_SQL),
    Migration::new(7, MIGRATION_V7_SQL),
    Migration::new(8, MIGRATION_V8_SQL),
    Migration::new(9, MIGRATION_V9_SQL),
    Migration::new(10, MIGRATION_V10_SQL),
];

/// One numbered schema migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Migration {
    version: u32,
    sql: &'static str,
}

impl Migration {
    /// A migration that upgrades the schema from `version - 1` to `version`.
    ///
    /// Versions are validated when the list is built with [`Migrations::new`].
    #[must_use]
    pub const fn new(version: u32, sql: &'static str) -> Self {
        Self { version, sql }
    }

    /// Version this migration upgrades to.
    #[must_use]
    pub const fn version(self) -> u32 {
        self.version
    }

    /// Embedded SQL applied for this version.
    #[must_use]
    pub const fn sql(self) -> &'static str {
        self.sql
    }

    /// Lowercase hex SHA-256 of [`Self::sql`], as stored in `schema_version`.
    #[must_use]
    pub fn sha256(self) -> String {
        sha256_hex(self.sql.as_bytes())
    }
}

/// A validated, gap-free migration sequence starting at v1.
#[derive(Clone, Debug)]
pub struct Migrations {
    list: Vec<Migration>,
}

impl Migrations {
    /// Validate `migrations` and keep them in order.
    ///
    /// The list must be non-empty, start at v1, and have no gaps or duplicates.
    /// Registration errors are reported instead of panicking, so a bad list in
    /// a future binary is a typed error rather than a crash.
    pub fn new(migrations: &[Migration]) -> Result<Self, DbError> {
        if migrations.is_empty() {
            return Err(DbError::InvalidMigrations {
                reason: "no migrations registered".to_owned(),
            });
        }
        let mut expected = 1_u32;
        for migration in migrations {
            if migration.version != expected {
                return Err(DbError::InvalidMigrations {
                    reason: format!("expected migration v{expected}, got v{}", migration.version),
                });
            }
            if migration.sql.trim().is_empty() {
                return Err(DbError::InvalidMigrations {
                    reason: format!("migration v{} has empty SQL", migration.version),
                });
            }
            expected = expected
                .checked_add(1)
                .ok_or_else(|| DbError::InvalidMigrations {
                    reason: "migration versions overflow u32".to_owned(),
                })?;
        }
        Ok(Self {
            list: migrations.to_vec(),
        })
    }

    /// A set built from a registry that may skip versions.
    ///
    /// Unlike [`Self::new`], missing versions are allowed: the set contains
    /// exactly the migrations it was given, in ascending order. This exists for
    /// two callers. [`Self::shipped`] uses it internally to represent the
    /// contiguous prefix of a wave-based registry, and tests use it to apply a
    /// migration whose lower-numbered neighbours are owned by another branch.
    /// The list must be non-empty, start at v1, and be strictly ascending.
    pub fn sparse(migrations: &[Migration]) -> Result<Self, DbError> {
        if migrations.is_empty() {
            return Err(DbError::InvalidMigrations {
                reason: "no migrations registered".to_owned(),
            });
        }
        let mut previous = 0_u32;
        for migration in migrations {
            if migration.version == 0 {
                return Err(DbError::InvalidMigrations {
                    reason: "migration versions start at v1".to_owned(),
                });
            }
            if previous == 0 && migration.version != 1 {
                return Err(DbError::InvalidMigrations {
                    reason: format!("expected migration v1 first, got v{}", migration.version),
                });
            }
            if migration.version <= previous {
                return Err(DbError::InvalidMigrations {
                    reason: format!("migration v{} is not after v{previous}", migration.version),
                });
            }
            if migration.sql.trim().is_empty() {
                return Err(DbError::InvalidMigrations {
                    reason: format!("migration v{} has empty SQL", migration.version),
                });
            }
            previous = migration.version;
        }
        Ok(Self {
            list: migrations.to_vec(),
        })
    }

    /// The longest contiguous run starting at v1 in [`SHIPPED_MIGRATIONS`].
    ///
    /// Higher versions registered ahead of a missing neighbour stay dormant
    /// until that neighbour lands; this keeps every open on the same ordered
    /// upgrade path instead of applying v7 before v3..v6 exist. Duplicate
    /// versions in the registry are an error.
    pub fn shipped() -> Result<Self, DbError> {
        let mut sorted = SHIPPED_MIGRATIONS.to_vec();
        sorted.sort_by_key(|migration| migration.version);
        let mut prefix = Vec::new();
        let mut expected = 1_u32;
        for migration in sorted {
            if migration.version < expected {
                return Err(DbError::InvalidMigrations {
                    reason: format!(
                        "duplicate migration v{} in SHIPPED_MIGRATIONS",
                        migration.version
                    ),
                });
            }
            if migration.version == expected {
                prefix.push(migration);
                expected = expected
                    .checked_add(1)
                    .ok_or_else(|| DbError::InvalidMigrations {
                        reason: "migration versions overflow u32".to_owned(),
                    })?;
            }
        }
        Self::new(&prefix)
    }

    /// Highest version in the set; never zero because the set is non-empty.
    #[must_use]
    pub fn latest(&self) -> u32 {
        self.list.last().map_or(0, |migration| migration.version)
    }

    /// The migration for `version`, if the set ships one.
    ///
    /// Sparse sets skip versions, so this searches by version rather than by
    /// position.
    #[must_use]
    pub fn get(&self, version: u32) -> Option<&Migration> {
        self.list
            .binary_search_by_key(&version, |migration| migration.version)
            .ok()
            .map(|index| &self.list[index])
    }

    /// Migrations in ascending version order.
    pub fn iter(&self) -> impl Iterator<Item = &Migration> {
        self.list.iter()
    }
}

/// A row of the `schema_version` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Schema version this row records.
    pub version: u32,
    /// When the migration ran, as UTC RFC 3339 text (`YYYY-MM-DDTHH:MM:SS.sssZ`).
    pub applied_at: String,
    /// Lowercase hex SHA-256 of the SQL that ran for this version.
    pub sha256: String,
}

/// Whether the database already has the bookkeeping table.
pub(crate) fn schema_version_table_exists(connection: &Connection) -> Result<bool, DbError> {
    let count: u32 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [SCHEMA_VERSION_TABLE],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// All rows in `schema_version`, oldest version first.
pub(crate) fn applied_migrations(
    connection: &Connection,
) -> Result<Vec<AppliedMigration>, DbError> {
    let mut statement = connection
        .prepare("SELECT version, applied_at, sha256 FROM schema_version ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        Ok(AppliedMigration {
            version: row.get(0)?,
            applied_at: row.get(1)?,
            sha256: row.get(2)?,
        })
    })?;
    let mut applied = Vec::new();
    for row in rows {
        applied.push(row?);
    }
    Ok(applied)
}

/// Check recorded migrations against the shipped set without writing anything.
///
/// [`DbError::SchemaTooNew`] is returned before any checksum work, so a newer
/// database is reported as such even when its hashes would also differ.
pub(crate) fn validate_applied(
    applied: &[AppliedMigration],
    migrations: &Migrations,
) -> Result<(), DbError> {
    let supported = migrations.latest();
    if let Some(found) = applied.iter().map(|row| row.version).max()
        && found > supported
    {
        return Err(DbError::SchemaTooNew { found, supported });
    }
    for row in applied {
        let Some(migration) = migrations.get(row.version) else {
            return Err(DbError::MigrationMissing {
                version: row.version,
            });
        };
        let expected = migration.sha256();
        if row.sha256 != expected {
            return Err(DbError::MigrationChecksumMismatch {
                version: row.version,
                recorded: row.sha256.clone(),
                expected,
            });
        }
    }
    Ok(())
}

/// Create the bookkeeping table before the first migration is applied.
///
/// Migration v1 contains the same `CREATE TABLE IF NOT EXISTS`, so executing it
/// here is the bootstrap and applying v1 afterwards is a recorded no-op DDL.
pub(crate) fn ensure_schema_version_table(connection: &Connection) -> Result<(), DbError> {
    connection.execute_batch(MIGRATION_V1_SQL)?;
    Ok(())
}

/// Apply every migration not yet recorded, one transaction per version.
///
/// A failing statement rolls back that version's transaction, including any DDL
/// it already executed, and no `schema_version` row is written for it.
pub(crate) fn apply_pending(
    connection: &mut Connection,
    migrations: &Migrations,
) -> Result<(), DbError> {
    let already: HashSet<u32> = applied_migrations(connection)?
        .into_iter()
        .map(|row| row.version)
        .collect();
    for migration in migrations
        .iter()
        .filter(|migration| !already.contains(&migration.version))
    {
        let checksum = migration.sha256();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql())?;
        let applied_at: String =
            transaction.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                row.get(0)
            })?;
        transaction.execute(
            "INSERT INTO schema_version (version, applied_at, sha256) VALUES (?1, ?2, ?3)",
            params![migration.version, applied_at, checksum],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
