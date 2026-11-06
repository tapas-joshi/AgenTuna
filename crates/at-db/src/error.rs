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
    /// A path could not be canonicalised on disk.
    #[error("{path}: {source}", path = .path.display())]
    PathUnresolved {
        /// Path that could not be resolved.
        path: PathBuf,
        /// Operating-system error from `std::fs::canonicalize`.
        #[source]
        source: std::io::Error,
    },
    /// A database integrity pragma returned a failure row.
    #[error("database integrity check failed: {details}")]
    IntegrityCheckFailed {
        /// SQLite's integrity-check output, which contains no database contents.
        details: String,
    },
    /// A database file or its backup could not be maintained.
    #[error("{path}: {source}", path = .path.display())]
    FileOperation {
        /// File or directory involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// A settings value could not be encoded as JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A caller passed a value the schema cannot store.
    #[error("invalid {field}: {reason}")]
    InvalidValue {
        /// Field that rejected the value.
        field: &'static str,
        /// Why the value is invalid.
        reason: String,
    },
    /// A stored value is not one this binary knows.
    #[error(transparent)]
    UnknownValue(#[from] at_model::ParseWireError),
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

impl From<at_model::ItemIdError> for DbError {
    fn from(error: at_model::ItemIdError) -> Self {
        Self::InvalidValue {
            field: "item_id",
            reason: error.to_string(),
        }
    }
}

impl From<at_model::AgentIdError> for DbError {
    fn from(error: at_model::AgentIdError) -> Self {
        Self::InvalidValue {
            field: "agent_id",
            reason: error.to_string(),
        }
    }
}
