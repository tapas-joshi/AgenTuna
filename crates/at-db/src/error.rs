//! Typed errors for the SQLite index.

use std::path::PathBuf;

use rusqlite::ErrorCode;

/// Why a database operation did not happen (or did not complete).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DbError {
    /// SQLite reported an error that is not a constraint violation.
    #[error("sqlite: {0}")]
    Sqlite(#[source] rusqlite::Error),
    /// A statement violated a SQL constraint (foreign key, unique, check, not null).
    #[error("constraint violated: {message}")]
    Constraint {
        /// SQLite's message, which names the violated constraint.
        message: String,
    },
    /// A migration changed after it was applied to this database.
    #[error("migration v{version}: checksum mismatch (recorded {recorded}, expected {expected})")]
    MigrationChecksumMismatch {
        /// Version whose recorded checksum does not match the shipped SQL.
        version: u32,
        /// SHA-256 stored in `schema_version` when the migration ran.
        recorded: String,
        /// SHA-256 of the SQL this binary ships for that version.
        expected: String,
    },
    /// The database was written by a newer binary.
    #[error("database schema v{found} is newer than this binary supports (v{supported})")]
    SchemaTooNew {
        /// Highest version recorded in the database.
        found: u32,
        /// Highest version this binary ships.
        supported: u32,
    },
    /// The database records a migration version this binary does not ship.
    #[error("database records migration v{version}, which this binary does not ship")]
    MigrationMissing {
        /// Recorded version with no matching migration.
        version: u32,
    },
    /// The migration list passed to the runner is not a valid sequence.
    #[error("invalid migrations: {reason}")]
    InvalidMigrations {
        /// What is wrong with the list.
        reason: String,
    },
    /// A file database could not be switched to WAL journal mode.
    #[error("{path}: could not enable WAL journal mode (journal_mode is {actual})", path = .path.display())]
    WalUnavailable {
        /// Database file that refused WAL.
        path: PathBuf,
        /// Journal mode SQLite kept instead.
        actual: String,
    },
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(inner, _) = &error
            && inner.code == ErrorCode::ConstraintViolation
        {
            return Self::Constraint {
                message: error.to_string(),
            };
        }
        Self::Sqlite(error)
    }
}
