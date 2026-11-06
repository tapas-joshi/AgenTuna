//! Read-only access to the SQLite index, for `--read-only` and diagnostics.
//!
//! [`ReadOnlyDatabase`] opens the file with `SQLITE_OPEN_READ_ONLY` and sets
//! `PRAGMA query_only = 1`, so SQLite itself rejects every statement that would modify the
//! database. It never runs migrations, never creates the file and never changes the journal
//! mode: a missing or unreadable index is a typed [`ReadOnlyError::Open`].
//!
//! Writes attempted through [`ReadOnlyDatabase::execute`] and
//! [`ReadOnlyDatabase::execute_batch`] surface as [`ReadOnlyError::ReadOnly`] instead of a raw
//! `SQLITE_READONLY`, so a CLI can map them to its `ReadOnly` error class.
//!
//! The type is deliberately separate from [`crate::Database`]: the normal handle migrates and
//! creates directories, which a read-only command must not do.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode, OpenFlags, Params};

/// Why a read-only connection could not be opened or a statement was rejected.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadOnlyError {
    /// The index could not be opened read-only (missing file, permissions, not a database).
    #[error("{}: cannot open the index read-only: {source}", path.display())]
    Open {
        /// Database file.
        path: PathBuf,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },
    /// The statement would write and `query_only` rejected it.
    #[error("read-only mode: writes are disabled")]
    ReadOnly,
    /// Any other SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// A connection that can only read.
#[derive(Debug)]
pub struct ReadOnlyDatabase {
    connection: Connection,
    path: PathBuf,
}

impl ReadOnlyDatabase {
    /// Opens `path` read-only and enables `PRAGMA query_only`.
    ///
    /// The file must already exist: this call never creates it.
    ///
    /// # Errors
    /// [`ReadOnlyError::Open`] when SQLite cannot open the file read-only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReadOnlyError> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| ReadOnlyError::Open {
            path: path.clone(),
            source,
        })?;
        connection.execute_batch("PRAGMA query_only = 1; PRAGMA foreign_keys = ON;")?;
        Ok(Self { connection, path })
    }

    /// The database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The underlying connection, for reads.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Highest recorded schema version, or `0` when the index has no `schema_version` table.
    ///
    /// # Errors
    /// A SQLite failure other than the table being absent.
    pub fn schema_version(&self) -> Result<u32, ReadOnlyError> {
        let present: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
            [],
            |row| row.get(0),
        )?;
        if !present {
            return Ok(0);
        }
        let version: Option<u32> =
            self.connection
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })?;
        Ok(version.unwrap_or(0))
    }

    /// Runs a statement that must not write.
    ///
    /// # Errors
    /// [`ReadOnlyError::ReadOnly`] when `query_only` rejected a write.
    pub fn execute(&self, sql: &str, params: impl Params) -> Result<usize, ReadOnlyError> {
        self.connection.execute(sql, params).map_err(map_error)
    }

    /// Runs several statements that must not write.
    ///
    /// # Errors
    /// [`ReadOnlyError::ReadOnly`] when `query_only` rejected a write.
    pub fn execute_batch(&self, sql: &str) -> Result<(), ReadOnlyError> {
        self.connection.execute_batch(sql).map_err(map_error)
    }
}

/// Maps `SQLITE_READONLY` to the typed error a CLI can report as `ReadOnly`.
fn map_error(error: rusqlite::Error) -> ReadOnlyError {
    if let rusqlite::Error::SqliteFailure(inner, _) = &error
        && inner.code == ErrorCode::ReadOnly
    {
        return ReadOnlyError::ReadOnly;
    }
    ReadOnlyError::Sqlite(error)
}

#[cfg(test)]
mod tests;
