//! SQLite index, repository layer, and durable history.
//!
//! [`Database`] opens a bundled-SQLite connection, applies the pragmas the rest
//! of the app relies on (`foreign_keys=ON`, `synchronous=NORMAL`, a 5 s busy
//! timeout, WAL for file databases), and runs embedded numbered migrations in
//! one transaction per version. Migration v1 only creates `schema_version`;
//! domain tables arrive in v2..v9, owned by TF-011..TF-018. [`migrations::Migrations::shipped`]
//! applies the longest contiguous prefix, so a migration registered ahead of a
//! numbered neighbour waits for it instead of skipping a schema step.
//!
//! Every applied migration is recorded with the SHA-256 of its SQL. Editing a
//! shipped migration or opening a database written by a newer binary is a typed
//! error ([`DbError::MigrationChecksumMismatch`] / [`DbError::SchemaTooNew`])
//! raised before the database is modified.

mod db;
mod error;
pub mod migrations;
pub mod projects;
pub mod read_only;

pub use db::Database;
pub use error::DbError;
pub use migrations::{AppliedMigration, Migration, Migrations, SHIPPED_MIGRATIONS};
pub use read_only::{ReadOnlyDatabase, ReadOnlyError};
