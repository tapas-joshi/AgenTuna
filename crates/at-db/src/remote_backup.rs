//! Remote backup state repository over `remote_backup_state` (migration v9,
//! owned by TF-018).
//!
//! The table is a singleton (`CHECK (id = 1)`): one row describing the GitHub
//! repository remote backups push to and pull from — repo URL, branch, last
//! push/pull/commit and the coarse status the settings screen renders.
//! [`remote_backup_state`] reads it, [`save_remote_backup_state`] writes the
//! whole row back. TC-017 (push), TC-018 (sync) and TC-019 (restore) update it
//! after each explicit user action; nothing here polls or touches the network.
//!
//! This module issues no DDL: every statement is a query or row change against
//! the schema TF-018 created.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::DbError;

/// Coarse state of the remote backup configuration.
///
/// The wire names match the `status` CHECK constraint in migration v9.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RemoteBackupStatus {
    /// No repository configured; the seeded default.
    Unconfigured,
    /// Configured and idle between explicit actions.
    Idle,
    /// A push, pull or restore is in flight.
    Syncing,
    /// The last action failed; `error_kind` names why.
    Error,
}

impl RemoteBackupStatus {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Unconfigured, Self::Idle, Self::Syncing, Self::Error];

    /// The value stored in `remote_backup_state.status`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Idle => "idle",
            Self::Syncing => "syncing",
            Self::Error => "error",
        }
    }

    /// Parses a stored status.
    ///
    /// # Errors
    /// [`DbError::InvalidValue`] when the stored text is not a known status.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_str().eq_ignore_ascii_case(value))
            .ok_or_else(|| DbError::InvalidValue {
                field: "remote_backup_state.status",
                reason: format!("unknown status `{value}`"),
            })
    }
}

impl std::fmt::Display for RemoteBackupStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The singleton `remote_backup_state` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBackupState {
    /// GitHub repository the backups live in (`https://github.com/…` or `git@…`).
    pub repo_url: Option<String>,
    /// Branch snapshots are pushed to and pulled from.
    pub branch: Option<String>,
    /// When the last push succeeded (RFC 3339 text).
    pub last_push_at: Option<String>,
    /// When the last pull/merge succeeded (RFC 3339 text).
    pub last_pull_at: Option<String>,
    /// Commit the local snapshot was last pushed as.
    pub last_commit: Option<String>,
    /// Coarse status.
    pub status: RemoteBackupStatus,
    /// Stable error code of the last failure, when `status` is `Error`.
    pub error_kind: Option<String>,
}

impl Default for RemoteBackupState {
    /// The seeded state migration v9 writes: nothing configured.
    fn default() -> Self {
        Self {
            repo_url: None,
            branch: None,
            last_push_at: None,
            last_pull_at: None,
            last_commit: None,
            status: RemoteBackupStatus::Unconfigured,
            error_kind: None,
        }
    }
}

/// Reads the singleton remote backup row.
///
/// Migration v9 seeds the row, so a migrated database always has one; a
/// database missing it (a hand-built fixture) reads as the default
/// [`RemoteBackupState::default`] rather than failing.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored status is unknown.
pub fn remote_backup_state(connection: &Connection) -> Result<RemoteBackupState, DbError> {
    let state = connection
        .query_row(
            "SELECT repo_url, branch, last_push_at, last_pull_at, last_commit, status, error_kind \
               FROM remote_backup_state WHERE id = 1",
            [],
            remote_backup_state_from_row,
        )
        .optional()?
        .unwrap_or_default();
    Ok(state)
}

/// Writes the whole singleton row back.
///
/// The row always exists after migration v9; the `INSERT … ON CONFLICT` keeps
/// the write working on a database where the seed row is absent (a fixture or
/// a repaired index). Callers read-modify-write: fetch with
/// [`remote_backup_state`], change the fields the action touched, save.
///
/// # Errors
///
/// [`DbError`] when the upsert fails.
pub fn save_remote_backup_state(
    connection: &Connection,
    state: &RemoteBackupState,
) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO remote_backup_state \
             (id, repo_url, branch, last_push_at, last_pull_at, last_commit, status, error_kind) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET \
             repo_url = excluded.repo_url, \
             branch = excluded.branch, \
             last_push_at = excluded.last_push_at, \
             last_pull_at = excluded.last_pull_at, \
             last_commit = excluded.last_commit, \
             status = excluded.status, \
             error_kind = excluded.error_kind",
        params![
            state.repo_url,
            state.branch,
            state.last_push_at,
            state.last_pull_at,
            state.last_commit,
            state.status.as_str(),
            state.error_kind,
        ],
    )?;
    Ok(())
}

/// Records the outcome of one remote action without touching the rest of the row.
///
/// `status` becomes the new coarse status; `error_kind` is stored with it
/// (`Some` on [`RemoteBackupStatus::Error`], normally `None` otherwise — the
/// column is cleared when `None` is passed). Timestamps and repo fields are
/// left as stored; use [`save_remote_backup_state`] for those.
///
/// Returns `false` when the singleton row is missing.
///
/// # Errors
///
/// [`DbError`] when the update fails.
pub fn set_remote_backup_status(
    connection: &Connection,
    status: RemoteBackupStatus,
    error_kind: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        "UPDATE remote_backup_state \
            SET status = ?1, error_kind = ?2 \
          WHERE id = 1",
        params![status.as_str(), error_kind],
    )?;
    Ok(changed == 1)
}

/// Maps the singleton row.
fn remote_backup_state_from_row(row: &Row<'_>) -> rusqlite::Result<RemoteBackupState> {
    let status: String = row.get(5)?;
    Ok(RemoteBackupState {
        repo_url: row.get(0)?,
        branch: row.get(1)?,
        last_push_at: row.get(2)?,
        last_pull_at: row.get(3)?,
        last_commit: row.get(4)?,
        status: RemoteBackupStatus::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(&status))
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(UnknownRemoteBackupStatus {
                        value: status.clone(),
                    }),
                )
            })?,
        error_kind: row.get(6)?,
    })
}

/// Error for a stored `remote_backup_state.status` outside the CHECK constraint.
#[derive(Debug, thiserror::Error)]
#[error("stored remote backup status `{value}` is not one of unconfigured, idle, syncing, error")]
struct UnknownRemoteBackupStatus {
    value: String,
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        let statements = [
            "SELECT repo_url, branch, last_push_at, last_pull_at, last_commit, status, error_kind FROM remote_backup_state WHERE id = 1",
            "INSERT INTO remote_backup_state (id, repo_url, branch, last_push_at, last_pull_at, last_commit, status, error_kind) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(id) DO UPDATE SET repo_url = excluded.repo_url, branch = excluded.branch, last_push_at = excluded.last_push_at, last_pull_at = excluded.last_pull_at, last_commit = excluded.last_commit, status = excluded.status, error_kind = excluded.error_kind",
            "UPDATE remote_backup_state SET status = ?1, error_kind = ?2 WHERE id = 1",
        ];
        for statement in statements {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
            let lowered = statement.to_ascii_lowercase();
            for keyword in ["create ", "alter ", "drop "] {
                assert!(
                    !lowered.contains(keyword),
                    "statement must not change the schema: {statement}"
                );
            }
        }
    }
}
