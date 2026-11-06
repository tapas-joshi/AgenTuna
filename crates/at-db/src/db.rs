//! Connection setup and the public [`Database`] handle.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::DbError;
use crate::migrations::{self, AppliedMigration, Migrations};

/// How long a connection waits for a locked database before failing.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// An open SQLite index whose schema is current for this binary.
///
/// Opening configures the connection (`foreign_keys=ON`, `synchronous=NORMAL`,
/// a 5 s busy timeout, WAL for file databases), validates the recorded
/// migrations, and applies any pending ones. All of that happens before the
/// handle is returned, so repositories can assume a ready schema.
#[derive(Debug)]
pub struct Database {
    connection: Connection,
    path: Option<PathBuf>,
}

impl Database {
    /// Open (or create) the database at `path` and apply shipped migrations.
    pub fn open_file(path: impl AsRef<Path>) -> Result<Self, DbError> {
        Self::open_file_with(path, Migrations::shipped()?)
    }

    /// Open an in-memory database and apply shipped migrations.
    ///
    /// In-memory databases keep SQLite's `memory` journal mode: WAL needs a
    /// file to write its log to.
    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::open_in_memory_with(Migrations::shipped()?)
    }

    /// Open (or create) `path` with an explicit migration set.
    ///
    /// Used by tests and by binaries that manage their own migration list.
    pub fn open_file_with(path: impl AsRef<Path>, migrations: Migrations) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        let mut connection = Connection::open(&path)?;
        prepare(&mut connection, Some(&path), &migrations)?;
        Ok(Self {
            connection,
            path: Some(path),
        })
    }

    /// Open an in-memory database with an explicit migration set.
    pub fn open_in_memory_with(migrations: Migrations) -> Result<Self, DbError> {
        let mut connection = Connection::open_in_memory()?;
        prepare(&mut connection, None, &migrations)?;
        Ok(Self {
            connection,
            path: None,
        })
    }

    /// The underlying connection, for repositories that only read or write rows.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The underlying connection, for repositories that need a transaction.
    #[must_use]
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Path of the database file; `None` for in-memory databases.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Highest schema version recorded in the database (0 only for an empty
    /// `schema_version` table, which a successful open never leaves behind).
    pub fn schema_version(&self) -> Result<u32, DbError> {
        let version: Option<u32> =
            self.connection
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })?;
        Ok(version.unwrap_or(0))
    }

    /// Recorded migrations, oldest first.
    pub fn applied_migrations(&self) -> Result<Vec<AppliedMigration>, DbError> {
        migrations::applied_migrations(&self.connection)
    }
}

/// Configure pragmas, check recorded migrations, then apply pending ones.
fn prepare(
    connection: &mut Connection,
    path: Option<&Path>,
    migrations: &Migrations,
) -> Result<(), DbError> {
    configure_pragmas(connection)?;
    let has_bookkeeping = migrations::schema_version_table_exists(connection)?;
    if has_bookkeeping {
        // Validation happens before WAL is enabled so a refused database is
        // left byte-identical: no persistent pragma is written and no
        // transaction starts.
        let applied = migrations::applied_migrations(connection)?;
        migrations::validate_applied(&applied, migrations)?;
    }
    if let Some(path) = path {
        enable_wal(connection, path)?;
    }
    if !has_bookkeeping {
        migrations::ensure_schema_version_table(connection)?;
    }
    migrations::apply_pending(connection, migrations)
}

/// Connection-local pragmas that every handle must carry.
fn configure_pragmas(connection: &Connection) -> Result<(), DbError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
    Ok(())
}

/// Switch a file database to WAL; refuse to continue in another journal mode.
fn enable_wal(connection: &Connection, path: &Path) -> Result<(), DbError> {
    let mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::WalUnavailable {
            path: path.to_path_buf(),
            actual: mode,
        });
    }
    Ok(())
}
