//! Connection setup and the public [`Database`] handle.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    unclean_marker: Option<PathBuf>,
    startup_warning: Option<PathBuf>,
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
        Self::open_file_impl(path.as_ref(), migrations, true)
    }

    /// Open `path` like [`Database::open_file`] but never quarantine a corrupt
    /// file: corruption is returned as an error and the file is left untouched.
    ///
    /// For diagnostics (`agentuna doctor`), which must report a damaged index
    /// and may only move it aside when the user asked for `--fix`.
    pub fn open_file_inspecting(path: impl AsRef<Path>) -> Result<Self, DbError> {
        Self::open_file_impl(path.as_ref(), Migrations::shipped()?, false)
    }

    fn open_file_impl(path: &Path, migrations: Migrations, recover: bool) -> Result<Self, DbError> {
        let path = path.to_path_buf();
        let mut connection = Connection::open(&path)?;
        let unclean_marker = unclean_marker_path(&path);
        let (backup, startup_warning) = match migration_backup(&connection, &path, &migrations) {
            Ok(backup) => (backup, None),
            Err(error) if recover && is_corruption_error(&error) => {
                drop(connection);
                let quarantined = quarantine_database(&path)?;
                connection = Connection::open(&path)?;
                (None, Some(quarantined))
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = prepare(&mut connection, Some(&path), &migrations) {
            if let Some(backup_path) = &backup {
                drop(connection);
                restore_database(&path, backup_path)?;
            }
            return Err(error);
        }
        if backup.is_some() {
            prune_db_backups(&path)?;
        }
        Ok(Self {
            connection,
            path: Some(path),
            unclean_marker: Some(unclean_marker),
            startup_warning,
        })
    }

    /// Open an in-memory database with an explicit migration set.
    pub fn open_in_memory_with(migrations: Migrations) -> Result<Self, DbError> {
        let mut connection = Connection::open_in_memory()?;
        prepare(&mut connection, None, &migrations)?;
        Ok(Self {
            connection,
            path: None,
            unclean_marker: None,
            startup_warning: None,
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
    /// Quarantine path when startup recovered a corrupt database.
    #[must_use]
    pub fn startup_warning(&self) -> Option<&Path> {
        self.startup_warning.as_deref()
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

impl Drop for Database {
    fn drop(&mut self) {
        if self.path.is_none() {
            return;
        }
        if let Err(error) = self
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA optimize;")
        {
            tracing::warn!(%error, "database clean-shutdown pragmas failed");
            return;
        }
        if let Some(marker) = &self.unclean_marker
            && let Err(error) = remove_unclean_marker(marker)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %marker.display(), %error, "could not remove database unclean-shutdown marker");
        }
    }
}
#[allow(clippy::disallowed_methods)] // Remove only this database's sibling unclean-shutdown marker.
fn remove_unclean_marker(marker: &Path) -> std::io::Result<()> {
    fs::remove_file(marker)
}
/// Configure pragmas, check recorded migrations, then apply pending ones.
fn prepare(
    connection: &mut Connection,
    path: Option<&Path>,
    migrations: &Migrations,
) -> Result<(), DbError> {
    configure_pragmas(connection)?;
    check_integrity(connection, path)?;
    let has_bookkeeping = migrations::schema_version_table_exists(connection)?;
    if has_bookkeeping {
        // Validation happens before WAL is enabled so a refused database is
        // left byte-identical: no persistent pragma is written and no
        // transaction starts.
        let applied = migrations::applied_migrations(connection)?;
        migrations::validate_applied(&applied, migrations)?;
    }
    if let Some(path) = path {
        write_unclean_marker(&unclean_marker_path(path))?;
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
/// Checks every open before migrations; the full pragma follows an unclean prior exit.
fn check_integrity(connection: &Connection, path: Option<&Path>) -> Result<(), DbError> {
    let full = path.is_some_and(|path| unclean_marker_path(path).exists());
    let pragma = if full {
        "PRAGMA integrity_check"
    } else {
        "PRAGMA quick_check"
    };
    let result: String = connection.query_row(pragma, [], |row| row.get(0))?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(DbError::IntegrityCheckFailed { details: result })
    }
}

fn unclean_marker_path(path: &Path) -> PathBuf {
    path.with_extension("unclean")
}

fn write_unclean_marker(path: &Path) -> Result<(), DbError> {
    // Create only the SQLite database's unclean-shutdown marker.
    #[allow(clippy::disallowed_methods)]
    let mut marker = File::create(path).map_err(|source| DbError::FileOperation {
        path: path.to_path_buf(),
        source,
    })?;
    marker
        .write_all(b"unclean\n")
        .and_then(|()| marker.sync_all())
        .map_err(|source| DbError::FileOperation {
            path: path.to_path_buf(),
            source,
        })
}
fn migration_backup(
    connection: &Connection,
    path: &Path,
    migrations: &Migrations,
) -> Result<Option<PathBuf>, DbError> {
    configure_pragmas(connection)?;
    check_integrity(connection, Some(path))?;
    if !migrations::schema_version_table_exists(connection)? {
        return Ok(None);
    }
    let applied = migrations::applied_migrations(connection)?;
    migrations::validate_applied(&applied, migrations)?;
    let applied_versions: std::collections::HashSet<_> =
        applied.iter().map(|row| row.version).collect();
    if !migrations
        .iter()
        .any(|migration| !applied_versions.contains(&migration.version()))
    {
        return Ok(None);
    }
    connection.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("backups")
        .join("db");
    // Create only the sibling directory reserved for DB backups.
    #[allow(clippy::disallowed_methods)]
    fs::create_dir_all(&directory).map_err(|source| DbError::FileOperation {
        path: directory.clone(),
        source,
    })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| DbError::FileOperation {
            path: directory.clone(),
            source: std::io::Error::other(source),
        })?
        .as_nanos();
    let backup = directory.join(format!("agentuna-{}-{stamp}.db", env!("CARGO_PKG_VERSION")));
    // Copy only the SQLite database into its sibling migration backup.
    #[allow(clippy::disallowed_methods)]
    fs::copy(path, &backup).map_err(|source| DbError::FileOperation {
        path: backup.clone(),
        source,
    })?;
    File::open(&backup)
        .and_then(|file| file.sync_all())
        .map_err(|source| DbError::FileOperation {
            path: backup.clone(),
            source,
        })?;
    File::open(&directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| DbError::FileOperation {
            path: directory,
            source,
        })?;
    Ok(Some(backup))
}

fn restore_database(path: &Path, backup: &Path) -> Result<(), DbError> {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        // Remove only SQLite WAL/SHM sidecars before restoring its DB file.
        #[allow(clippy::disallowed_methods)]
        match fs::remove_file(&sidecar) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(source) => {
                return Err(DbError::FileOperation {
                    path: sidecar,
                    source,
                });
            }
        }
    }
    // Restore only the SQLite database from its sibling migration backup.
    #[allow(clippy::disallowed_methods)]
    fs::copy(backup, path).map_err(|source| DbError::FileOperation {
        path: path.to_path_buf(),
        source,
    })?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| DbError::FileOperation {
            path: path.to_path_buf(),
            source,
        })
}

fn prune_db_backups(path: &Path) -> Result<(), DbError> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("backups")
        .join("db");
    let entries = fs::read_dir(&directory).map_err(|source| DbError::FileOperation {
        path: directory.clone(),
        source,
    })?;
    let mut backups = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry.file_name().is_some_and(|name| {
                name.to_string_lossy().starts_with("agentuna-")
                    && entry.extension().is_some_and(|extension| extension == "db")
            })
        })
        .collect::<Vec<_>>();
    backups
        .sort_by_key(|entry| std::cmp::Reverse(entry.file_name().map(|name| name.to_os_string())));
    for stale in backups.into_iter().skip(3) {
        #[allow(clippy::disallowed_methods)] // Prune only expired SQLite database backup files.
        fs::remove_file(&stale).map_err(|source| DbError::FileOperation {
            path: stale,
            source,
        })?;
    }
    Ok(())
}
fn is_corruption_error(error: &DbError) -> bool {
    match error {
        DbError::IntegrityCheckFailed { .. } => true,
        DbError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        _ => false,
    }
}

fn quarantine_database(path: &Path) -> Result<PathBuf, DbError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| DbError::FileOperation {
            path: path.to_path_buf(),
            source: std::io::Error::other(source),
        })?
        .as_millis();
    let mut quarantined = path.as_os_str().to_os_string();
    quarantined.push(format!(".corrupt-{stamp}"));
    let quarantined = PathBuf::from(quarantined);
    // Move only the corrupt SQLite database into its sibling quarantine.
    #[allow(clippy::disallowed_methods)]
    fs::rename(path, &quarantined).map_err(|source| DbError::FileOperation {
        path: path.to_path_buf(),
        source,
    })?;
    for suffix in ["-wal", "-shm"] {
        let mut original = path.as_os_str().to_os_string();
        original.push(suffix);
        let original = PathBuf::from(original);
        let mut saved = quarantined.as_os_str().to_os_string();
        saved.push(suffix);
        let saved = PathBuf::from(saved);
        // Move only SQLite WAL/SHM sidecars into matching quarantine siblings.
        #[allow(clippy::disallowed_methods)]
        match fs::rename(&original, &saved) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(source) => {
                return Err(DbError::FileOperation {
                    path: original,
                    source,
                });
            }
        }
    }
    Ok(quarantined)
}
