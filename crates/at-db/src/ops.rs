//! Operations log: the append-only audit trail of everything AgenTuna changed.
//!
//! One operation is one state-changing action — a deploy, an adopt, a settings
//! change, a migration. [`begin`] writes its row in `running` state, zero or
//! more [`attach_file`] calls record the files it touched with the content hash
//! either side of the change, and [`finish`] writes the terminal status. A
//! caller that already knows the outcome uses [`record`] (operation, files and
//! terminal status in ONE transaction) or [`record_completed`] (a single
//! already-finished row with no file rows, for metadata changes such as
//! settings).
//!
//! Rows are append-only (TF-009): the only `UPDATE` in this module sets
//! `status`, `error_kind` and `finished_at`, and only while the row is still
//! `running`. `kind`, `scope` and `started_at` are never updated, and no API
//! here deletes an operation or a file row. [`crate::retention`] is the only
//! code that removes old history, under its own policy.
//!
//! IDs written by [`begin`] and [`record`] are ULIDs ([`OperationId`]). Rows
//! may also carry an opaque id written by a caller — [`record_interrupted`]
//! stores the app bridge's job id verbatim — so read models expose the primary
//! key as `String` and writes that create history take [`OperationId`].
//!
//! [`recent_changes`] reads the history timeline back. It joins the item name
//! and agent display name from `deployments` (migration v4) so the timeline
//! needs one round trip per screen; operations that touched no deployment read
//! back with both names `None`.
//!
//! [`undo_file_list`] turns one operation's file rows into the inverse actions
//! undo must take, and flags each file whose current state no longer matches
//! what the operation recorded. It hashes through a [`FileProbe`] so the undo
//! planner in at-engine (TC-010) can substitute a probe that understands copy
//! manifests and other directory shapes.

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use at_model::{ParseWireError, Scope};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::DbError;

/// Largest number of rows [`recent_changes`] returns in one call.
pub const MAX_RECENT_CHANGES: usize = 500;

/// Default number of rows [`recent_changes`] returns.
pub const DEFAULT_RECENT_CHANGES: usize = 50;

/// Declares a unit enum whose SQLite value, `Display` and `FromStr` come from
/// one table.
macro_rules! ops_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant ),+ ];
            /// Every stored value, in declaration order.
            pub const WIRE_NAMES: &'static [&'static str] = &[ $( $wire ),+ ];

            /// The stable snake_case value stored in SQLite.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $wire ),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseWireError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                $( if text.eq_ignore_ascii_case($wire) { return Ok(Self::$variant); } )+
                Err(ParseWireError {
                    type_name: stringify!($name),
                    input: text.to_owned(),
                    expected: Self::WIRE_NAMES,
                })
            }
        }
    };
}

ops_enum! {
    /// What an operation did. Declaration order follows TF-009.
    pub enum OperationKind {
        /// Deployed an item to an agent (or several agents).
        Deploy => "deploy",
        /// Removed a deployment.
        Undeploy => "undeploy",
        /// Edited a skill definition.
        EditSkill => "edit_skill",
        /// Edited an MCP server definition.
        EditMcp => "edit_mcp",
        /// Edited a rule file.
        EditRule => "edit_rule",
        /// Migrated native items into the store.
        Migrate => "migrate",
        /// Adopted a native item as a canonical item.
        Adopt => "adopt",
        /// Restored a file or item from a backup.
        Restore => "restore",
        /// Took a backup.
        Backup => "backup",
        /// Scanned agent folders for native items.
        Scan => "scan",
        /// Changed a setting.
        Settings => "settings",
        /// Checked for and applied an application or skill update.
        Update => "update",
        /// Adopted a drifted file into the store.
        DriftAdopt => "drift_adopt",
        /// Restored a drifted file from the store.
        DriftRestore => "drift_restore",
        /// Pushed the remote backup repository.
        SyncPush => "sync_push",
        /// Pulled the remote backup repository.
        SyncPull => "sync_pull",
        /// Wrote a backup to the remote repository.
        RemoteBackup => "remote_backup",
        /// Restored from the remote repository.
        RemoteRestore => "remote_restore",
        /// Undid another operation.
        Undo => "undo",
        /// Deleted an item.
        Delete => "delete",
        /// Promoted an item to a nearer scope.
        Promote => "promote",
        /// Moved an item between scopes.
        Move => "move",
        /// Applied a preset.
        PresetApply => "preset_apply",
        /// Un-applied a preset.
        PresetUnapply => "preset_unapply",
        /// Locked an item at a scope.
        Lock => "lock",
        /// Removed a lock.
        Unlock => "unlock",
    }
}

ops_enum! {
    /// Which part of AgenTuna started an operation.
    pub enum OperationActor {
        /// The desktop app.
        Ui => "ui",
        /// The `agentuna` command line.
        Cli => "cli",
        /// The one-time migration from another tool.
        Migration => "migration",
        /// The file watcher.
        Watcher => "watcher",
        /// The updater.
        Updater => "updater",
    }
}

ops_enum! {
    /// Where an operation stands. `Running` is not a terminal status.
    pub enum OperationStatus {
        /// Started and not finished.
        Running => "running",
        /// Finished successfully.
        Succeeded => "succeeded",
        /// Finished with an error.
        Failed => "failed",
        /// The process died before finishing.
        Interrupted => "interrupted",
        /// Undone by a later operation.
        Undone => "undone",
    }
}

impl OperationStatus {
    /// Whether this status ends an operation.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

ops_enum! {
    /// What happened to one file during an operation.
    pub enum FileAction {
        /// The file did not exist before.
        Created => "created",
        /// The file existed and its content changed.
        Modified => "modified",
        /// The file existed and was removed.
        Deleted => "deleted",
        /// A symbolic link was written; nothing existed before.
        Linked => "linked",
        /// The file moved; `path` is where it went.
        Moved => "moved",
    }
}

/// Identifier of an operation this module created: a 26-character Crockford
/// base32 ULID.
///
/// ULIDs sort by creation time as text, so `operations.id` ordering agrees with
/// `started_at` even when two rows share a millisecond. [`OperationId::parse`]
/// accepts either case and rejects anything that is not a ULID; read models use
/// `String` instead because a caller (the app bridge) may store its own id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    /// Generate an identifier for an operation starting now.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::generate().to_string())
    }

    /// Wrap an already-generated ULID.
    #[must_use]
    pub fn from_ulid(ulid: Ulid) -> Self {
        Self(ulid.to_string())
    }

    /// Parse the 26-character text of an identifier.
    ///
    /// # Errors
    ///
    /// [`ParseWireError`] when `text` is not a ULID.
    pub fn parse(text: &str) -> Result<Self, ParseWireError> {
        let ulid = Ulid::from_string(text).map_err(|_| ParseWireError {
            type_name: "OperationId",
            input: text.to_owned(),
            expected: &["26-character Crockford base32 ULID"],
        })?;
        Ok(Self(ulid.to_string()))
    }

    /// The canonical uppercase text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for OperationId {
    type Err = ParseWireError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// Everything [`begin`] writes into a new `operations` row.
#[derive(Clone, Debug)]
pub struct NewOperation<'a> {
    /// What the operation does.
    pub kind: OperationKind,
    /// Scope the change belongs to; `None` for operations that are not scoped
    /// (an update check, a migration scan).
    pub scope: Option<Scope>,
    /// Who started it.
    pub actor: OperationActor,
    /// One-line description shown in the history timeline.
    pub summary: Option<&'a str>,
    /// Groups operations that belong to one user action.
    pub correlation_id: Option<&'a str>,
    /// The operation this one undoes.
    pub undo_of: Option<&'a OperationId>,
}

impl<'a> NewOperation<'a> {
    /// A running operation of `kind` started by `actor`.
    #[must_use]
    pub const fn new(kind: OperationKind, actor: OperationActor) -> Self {
        Self {
            kind,
            scope: None,
            actor,
            summary: None,
            correlation_id: None,
            undo_of: None,
        }
    }

    /// Sets the scope.
    #[must_use]
    pub const fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Sets the summary line.
    #[must_use]
    pub const fn with_summary(mut self, summary: &'a str) -> Self {
        self.summary = Some(summary);
        self
    }

    /// Sets the correlation id.
    #[must_use]
    pub const fn with_correlation_id(mut self, correlation_id: &'a str) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    /// Marks this operation as undoing `undo_of`.
    #[must_use]
    pub const fn undoing(mut self, undo_of: &'a OperationId) -> Self {
        self.undo_of = Some(undo_of);
        self
    }
}

/// One file an operation touched, as written by [`attach_file`].
#[derive(Clone, Debug)]
pub struct TouchedFile<'a> {
    /// Absolute path of the file as the operation left it.
    pub path: &'a str,
    /// What happened to it.
    pub action: FileAction,
    /// Content hash before the operation; `None` when nothing existed before.
    pub before_hash: Option<&'a str>,
    /// Content hash after the operation; `None` when the file is gone.
    pub after_hash: Option<&'a str>,
    /// Backup that can restore the previous content.
    pub backup_id: Option<&'a str>,
    /// Size before the operation.
    pub bytes_before: Option<i64>,
    /// Size after the operation.
    pub bytes_after: Option<i64>,
}

impl<'a> TouchedFile<'a> {
    /// A touched file with only the path and action set.
    #[must_use]
    pub const fn new(path: &'a str, action: FileAction) -> Self {
        Self {
            path,
            action,
            before_hash: None,
            after_hash: None,
            backup_id: None,
            bytes_before: None,
            bytes_after: None,
        }
    }

    /// Sets both content hashes.
    #[must_use]
    pub const fn with_hashes(
        mut self,
        before_hash: Option<&'a str>,
        after_hash: Option<&'a str>,
    ) -> Self {
        self.before_hash = before_hash;
        self.after_hash = after_hash;
        self
    }

    /// Sets the backup that can restore the previous content.
    #[must_use]
    pub const fn with_backup(mut self, backup_id: &'a str) -> Self {
        self.backup_id = Some(backup_id);
        self
    }

    /// Sets both sizes.
    #[must_use]
    pub const fn with_sizes(mut self, bytes_before: i64, bytes_after: i64) -> Self {
        self.bytes_before = Some(bytes_before);
        self.bytes_after = Some(bytes_after);
        self
    }
}

/// One `operations` row as read back.
///
/// The four enum columns are nullable in the schema (v7). The writers in this
/// module always set `actor` and `status`, and set `kind` unless the row comes
/// from [`record_interrupted`], which records a generic app job that has no
/// operation kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRow {
    /// Primary key as stored.
    pub id: String,
    /// What the operation did.
    pub kind: Option<OperationKind>,
    /// Scope the change belongs to.
    pub scope: Option<Scope>,
    /// Who started it.
    pub actor: Option<OperationActor>,
    /// Current status.
    pub status: Option<OperationStatus>,
    /// One-line description.
    pub summary: Option<String>,
    /// Error classification when the operation failed.
    pub error_kind: Option<String>,
    /// When the operation started (SQLite timestamp).
    pub started_at: String,
    /// When it finished, if it has.
    pub finished_at: Option<String>,
    /// Groups operations that belong to one user action.
    pub correlation_id: Option<String>,
    /// The operation this one undoes.
    pub undo_of: Option<String>,
}

impl OperationRow {
    /// Whether the operation is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.status == Some(OperationStatus::Running)
    }
}

/// One `operation_files` row as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFile {
    /// Row id; insertion order is apply order.
    pub id: i64,
    /// Path as the operation left it.
    pub path: Option<String>,
    /// What happened to the file.
    pub action: Option<FileAction>,
    /// Content hash before the operation.
    pub before_hash: Option<String>,
    /// Content hash after the operation.
    pub after_hash: Option<String>,
    /// Backup that can restore the previous content.
    pub backup_id: Option<String>,
    /// Size before the operation.
    pub bytes_before: Option<i64>,
    /// Size after the operation.
    pub bytes_after: Option<i64>,
}

/// One operation with every file it touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationDetail {
    /// The operation row.
    pub operation: OperationRow,
    /// Files in apply order.
    pub files: Vec<OperationFile>,
}

/// Filters for [`recent_changes`].
///
/// `limit` is clamped to `1..=MAX_RECENT_CHANGES`; `scope`, `kind` and
/// `since_unix_seconds` are optional and an unset filter matches every row.
#[derive(Clone, Copy, Debug)]
pub struct RecentChangesQuery {
    /// Maximum number of rows.
    pub limit: usize,
    /// Only operations at this scope.
    pub scope: Option<Scope>,
    /// Only operations of this kind.
    pub kind: Option<OperationKind>,
    /// Only operations that started at or after this instant (Unix seconds).
    pub since_unix_seconds: Option<i64>,
}

impl Default for RecentChangesQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RECENT_CHANGES,
            scope: None,
            kind: None,
            since_unix_seconds: None,
        }
    }
}

impl RecentChangesQuery {
    /// A query returning at most `limit` rows.
    #[must_use]
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            scope: None,
            kind: None,
            since_unix_seconds: None,
        }
    }

    /// Only operations at `scope`.
    #[must_use]
    pub const fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Only operations of `kind`.
    #[must_use]
    pub const fn with_kind(mut self, kind: OperationKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Only operations that started at or after `unix_seconds`.
    #[must_use]
    pub const fn since(mut self, unix_seconds: i64) -> Self {
        self.since_unix_seconds = Some(unix_seconds);
        self
    }

    /// The effective row limit.
    #[must_use]
    fn effective_limit(&self) -> i64 {
        i64::try_from(self.limit.clamp(1, MAX_RECENT_CHANGES)).unwrap_or(i64::MAX)
    }
}

/// One row of the history timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentChange {
    /// Primary key as stored.
    pub id: String,
    /// What it did.
    pub kind: Option<OperationKind>,
    /// Scope it belongs to.
    pub scope: Option<Scope>,
    /// Who started it.
    pub actor: Option<OperationActor>,
    /// Current status.
    pub status: Option<OperationStatus>,
    /// One-line description.
    pub summary: Option<String>,
    /// Error classification when it failed.
    pub error_kind: Option<String>,
    /// When it started (SQLite timestamp).
    pub started_at: String,
    /// When it finished.
    pub finished_at: Option<String>,
    /// How many files it touched.
    pub file_count: i64,
    /// Item name from the deployment this operation wrote, when it wrote one.
    pub item_name: Option<String>,
    /// Agent display name from the deployment this operation wrote.
    pub agent_display_name: Option<String>,
}

/// What the file system holds at a recorded path now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentState {
    /// Nothing exists at the path (a broken symlink counts as missing).
    Missing,
    /// The path exists and hashes to this value.
    Hash(String),
    /// The path exists but this probe cannot hash it — for example a directory,
    /// whose content hash format belongs to at-fs.
    Unverifiable,
}

/// Resolves the current state of a path recorded in `operation_files`.
///
/// Undo compares the result with the recorded `after_hash` to decide whether a
/// file changed after the operation. [`DiskFileProbe`] is the default; the undo
/// planner can supply one that also understands copy manifests and link
/// targets.
pub trait FileProbe {
    /// What is at `path` now.
    fn current_state(&self, path: &Path) -> CurrentState;
}

/// The default [`FileProbe`]: hashes regular files (following symlinks) and
/// reports every other existing path as [`CurrentState::Unverifiable`].
///
/// Directory content hashes are written by at-fs from its own copy manifest
/// format, so this probe does not guess at them; pass a probe that understands
/// them when undo has to verify directory deployments.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskFileProbe;

impl FileProbe for DiskFileProbe {
    fn current_state(&self, path: &Path) -> CurrentState {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => match fs::read(path) {
                Ok(bytes) => CurrentState::Hash(sha256_hex(&bytes)),
                Err(_) => CurrentState::Unverifiable,
            },
            Ok(_) => CurrentState::Unverifiable,
            Err(_) => CurrentState::Missing,
        }
    }
}

/// What undo must do to reverse one file row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum UndoAction {
    /// Remove a file (or link) the operation created.
    Delete,
    /// Put the previous content back, from the backup or the store.
    Restore,
    /// Move the file back where it came from.
    Move,
}

/// One file the undo planner has to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoFile {
    /// Path recorded by the operation; `None` when the row carries no path.
    pub path: Option<String>,
    /// Action the operation took.
    pub action: Option<FileAction>,
    /// Its inverse; `None` when the action is unknown and cannot be inverted.
    pub inverse: Option<UndoAction>,
    /// Backup that holds the previous content.
    pub backup_id: Option<String>,
    /// Content hash the operation left behind.
    pub recorded_hash: Option<String>,
    /// Content hash before the operation; what [`UndoAction::Restore`] restores.
    pub restore_hash: Option<String>,
    /// State found at the path now.
    pub current: CurrentState,
    /// Whether the path changed after the operation, so undoing it would
    /// overwrite a newer edit.
    pub conflicted: bool,
}

/// Every file row of one operation, inverted, plus the operation itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoFileList {
    /// The operation being undone.
    pub operation: OperationRow,
    /// Files in apply order.
    pub files: Vec<UndoFile>,
    /// Days after which undo is refused by [`crate::retention`].
    pub window_days: u32,
}

/// Writes one `running` row and returns its identifier.
///
/// # Errors
///
/// [`DbError`] when the row cannot be inserted.
pub fn begin(connection: &Connection, new: &NewOperation<'_>) -> Result<OperationId, DbError> {
    let id = OperationId::new();
    insert_running(connection, &id, new)?;
    Ok(id)
}

/// Records one file the operation touched.
///
/// # Errors
///
/// [`DbError::Constraint`] when the operation id does not exist.
pub fn attach_file(
    connection: &Connection,
    operation: &OperationId,
    file: &TouchedFile<'_>,
) -> Result<(), DbError> {
    insert_file(connection, operation.as_str(), file)
}

/// Writes the terminal status of an operation.
///
/// Only a `running` row is finished, and only `status`, `error_kind` and
/// `finished_at` change: `kind`, `scope` and `started_at` stay as written.
/// Returns `false` when the row is unknown, already finished, or `status` is
/// not terminal.
///
/// # Errors
///
/// [`DbError`] when the update cannot be written.
pub fn finish(
    connection: &Connection,
    operation: &OperationId,
    status: OperationStatus,
    error_kind: Option<&str>,
) -> Result<bool, DbError> {
    if !status.is_terminal() {
        return Ok(false);
    }
    let updated = connection.execute(
        "UPDATE operations \
            SET status = ?2, \
                error_kind = ?3, \
                finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?1 AND status = 'running'",
        params![operation.as_str(), status.as_str(), error_kind],
    )?;
    Ok(updated == 1)
}

/// Writes an operation, its files and its terminal status in one transaction.
///
/// Use this when the outcome is already known; nothing is left in `running`
/// state if any step fails.
///
/// # Errors
///
/// [`DbError::InvalidValue`] when `status` is not terminal, or [`DbError`] when
/// a statement fails.
pub fn record(
    connection: &Connection,
    new: &NewOperation<'_>,
    files: &[TouchedFile<'_>],
    status: OperationStatus,
    error_kind: Option<&str>,
) -> Result<OperationId, DbError> {
    if !status.is_terminal() {
        return Err(DbError::InvalidValue {
            field: "status",
            reason: format!("record() needs a terminal status, got {status}"),
        });
    }
    let id = OperationId::new();
    let transaction = connection.unchecked_transaction()?;
    insert_running(&transaction, &id, new)?;
    for file in files {
        insert_file(&transaction, id.as_str(), file)?;
    }
    transaction.execute(
        "UPDATE operations \
            SET status = ?2, \
                error_kind = ?3, \
                finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?1",
        params![id.as_str(), status.as_str(), error_kind],
    )?;
    transaction.commit()?;
    Ok(id)
}

/// Writes an already-finished `succeeded` row with no file rows.
///
/// One `INSERT`, one transaction: the shape settings writes need (TF-023).
///
/// # Errors
///
/// [`DbError`] when the row cannot be inserted.
pub fn record_completed(
    connection: &Connection,
    kind: OperationKind,
    actor: OperationActor,
    summary: Option<&str>,
) -> Result<OperationId, DbError> {
    let id = OperationId::new();
    connection.execute(
        "INSERT INTO operations (id, kind, scope, actor, status, summary, \
                                 correlation_id, undo_of, finished_at) \
         VALUES (?1, ?2, NULL, ?3, 'succeeded', ?4, NULL, NULL, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![id.as_str(), kind.as_str(), actor.as_str(), summary],
    )?;
    Ok(id)
}

/// Records the `interrupted` row for a job that outlived the process.
///
/// This is the `at-db` side of the app bridge's `OperationsLog` seam: the
/// bridge calls it once per job that was still running at shutdown, passing the
/// job id (stored verbatim; it need not be a ULID), the job name as the summary
/// and the epoch milliseconds the job started at. A row that already exists is
/// finished rather than overwritten, so nothing append-only changes.
///
/// # Errors
///
/// [`DbError`] when the row cannot be written.
pub fn record_interrupted(
    connection: &Connection,
    id: &str,
    kind: Option<OperationKind>,
    summary: Option<&str>,
    started_at_unix_ms: i64,
) -> Result<(), DbError> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE operations \
            SET status = 'interrupted', \
                finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?1 AND status = 'running'",
        [id],
    )?;
    transaction.execute(
        "INSERT INTO operations (id, kind, actor, status, summary, started_at, created_at, finished_at) \
         SELECT ?1, ?2, ?3, 'interrupted', ?4, \
                strftime('%Y-%m-%dT%H:%M:%fZ', ?5 / 1000.0, 'unixepoch'), \
                strftime('%Y-%m-%dT%H:%M:%fZ', ?5 / 1000.0, 'unixepoch'), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE NOT EXISTS (SELECT 1 FROM operations WHERE id = ?1)",
        params![
            id,
            kind.map(OperationKind::as_str),
            OperationActor::Ui.as_str(),
            summary,
            started_at_unix_ms,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

/// The newest operations, newest first, with deployment names joined in.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored enum value is unknown to this
/// binary.
pub fn recent_changes(
    connection: &Connection,
    query: &RecentChangesQuery,
) -> Result<Vec<RecentChange>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, kind, scope, actor, status, summary, error_kind, \
                started_at, finished_at \
           FROM operations \
          WHERE (?1 IS NULL OR scope = ?1) \
            AND (?2 IS NULL OR kind = ?2) \
            AND (?3 IS NULL OR started_at >= strftime('%Y-%m-%dT%H:%M:%fZ', ?3, 'unixepoch')) \
          ORDER BY started_at DESC, rowid DESC \
          LIMIT ?4",
    )?;
    let rows = statement
        .query_map(
            params![
                query.scope.map(Scope::wire_name),
                query.kind.map(OperationKind::as_str),
                query.since_unix_seconds,
                query.effective_limit(),
            ],
            |row| {
                Ok(RawChange {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    scope: row.get(2)?,
                    actor: row.get(3)?,
                    status: row.get(4)?,
                    summary: row.get(5)?,
                    error_kind: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;

    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let counts = file_counts(connection, &ids)?;
    let names = deployment_names(connection, &ids)?;

    let mut changes = Vec::with_capacity(rows.len());
    for row in rows {
        let (item_name, agent_display_name) = names
            .iter()
            .find(|joined| joined.operation_id == row.id)
            .map(|joined| (joined.item_name.clone(), joined.agent_display_name.clone()))
            .unwrap_or((None, None));
        changes.push(RecentChange {
            id: row.id.clone(),
            kind: parse_optional(row.kind.as_deref())?,
            scope: parse_optional(row.scope.as_deref())?,
            actor: parse_optional(row.actor.as_deref())?,
            status: parse_optional(row.status.as_deref())?,
            summary: row.summary,
            error_kind: row.error_kind,
            started_at: row.started_at,
            finished_at: row.finished_at,
            file_count: counts
                .iter()
                .find(|(joined, _)| joined == &row.id)
                .map_or(0, |(_, count)| *count),
            item_name,
            agent_display_name,
        });
    }
    Ok(changes)
}

/// One operation with every file row it wrote.
///
/// Returns `None` for an unknown id.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored enum value is unknown.
pub fn operation_detail(
    connection: &Connection,
    id: &str,
) -> Result<Option<OperationDetail>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {OPERATION_COLUMNS} FROM operations WHERE id = ?1"
    ))?;
    let raw = statement.query_row([id], raw_operation).optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let operation = raw.into_row()?;
    let files = files_for(connection, &operation.id)?;
    Ok(Some(OperationDetail { operation, files }))
}

/// The inverse actions of one operation, using [`DiskFileProbe`].
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn undo_file_list(connection: &Connection, id: &str) -> Result<Option<UndoFileList>, DbError> {
    undo_file_list_with(connection, id, &DiskFileProbe)
}

/// The inverse actions of one operation, checked through `probe`.
///
/// A file is flagged [`UndoFile::conflicted`] when what is at its path now
/// differs from the `after_hash` the operation recorded: a file that should
/// exist is gone or changed, or a file the operation deleted is back. Paths the
/// probe reports as [`CurrentState::Unverifiable`] are never flagged — the
/// caller decides what to do with a path it cannot verify.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn undo_file_list_with(
    connection: &Connection,
    id: &str,
    probe: &dyn FileProbe,
) -> Result<Option<UndoFileList>, DbError> {
    let Some(detail) = operation_detail(connection, id)? else {
        return Ok(None);
    };
    let files = detail
        .files
        .iter()
        .map(|file| undo_file(file, probe))
        .collect();
    Ok(Some(UndoFileList {
        operation: detail.operation,
        files,
        window_days: crate::retention::UNDO_WINDOW_DAYS,
    }))
}

/// Inverts one recorded file row and checks it against the file system.
fn undo_file(file: &OperationFile, probe: &dyn FileProbe) -> UndoFile {
    let inverse = file.action.map(|action| match action {
        FileAction::Created | FileAction::Linked => UndoAction::Delete,
        FileAction::Modified | FileAction::Deleted => UndoAction::Restore,
        FileAction::Moved => UndoAction::Move,
    });
    let current = match file.path.as_deref() {
        Some(path) => probe.current_state(Path::new(path)),
        None => CurrentState::Missing,
    };
    let conflicted = match (&file.after_hash, &current) {
        (Some(recorded), CurrentState::Hash(found)) => recorded != found,
        (Some(_), CurrentState::Missing) => true,
        (None, CurrentState::Hash(_)) => true,
        (None, CurrentState::Missing) | (_, CurrentState::Unverifiable) => false,
    };
    UndoFile {
        path: file.path.clone(),
        action: file.action,
        inverse,
        backup_id: file.backup_id.clone(),
        recorded_hash: file.after_hash.clone(),
        restore_hash: file.before_hash.clone(),
        current,
        conflicted,
    }
}

/// Column list every `operations` read selects, in [`RawOperation`] order.
const OPERATION_COLUMNS: &str = "id, kind, scope, actor, status, summary, error_kind, \
     started_at, finished_at, correlation_id, undo_of";

/// An `operations` row as SQLite returns it, before enum parsing.
struct RawOperation {
    id: String,
    kind: Option<String>,
    scope: Option<String>,
    actor: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    error_kind: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    correlation_id: Option<String>,
    undo_of: Option<String>,
}

impl RawOperation {
    /// Parse the stored enum columns.
    fn into_row(self) -> Result<OperationRow, DbError> {
        Ok(OperationRow {
            kind: parse_optional(self.kind.as_deref())?,
            scope: parse_optional(self.scope.as_deref())?,
            actor: parse_optional(self.actor.as_deref())?,
            status: parse_optional(self.status.as_deref())?,
            id: self.id,
            summary: self.summary,
            error_kind: self.error_kind,
            started_at: self.started_at,
            finished_at: self.finished_at,
            correlation_id: self.correlation_id,
            undo_of: self.undo_of,
        })
    }
}

/// The timeline row before enum parsing.
struct RawChange {
    id: String,
    kind: Option<String>,
    scope: Option<String>,
    actor: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    error_kind: Option<String>,
    started_at: String,
    finished_at: Option<String>,
}

/// Insert one `running` row.
fn insert_running(
    connection: &Connection,
    id: &OperationId,
    new: &NewOperation<'_>,
) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO operations (id, kind, scope, actor, status, summary, correlation_id, undo_of) \
         VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
        params![
            id.as_str(),
            new.kind.as_str(),
            new.scope.map(Scope::wire_name),
            new.actor.as_str(),
            new.summary,
            new.correlation_id,
            new.undo_of.map(OperationId::as_str),
        ],
    )?;
    Ok(())
}

/// Insert one file row.
fn insert_file(
    connection: &Connection,
    operation: &str,
    file: &TouchedFile<'_>,
) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO operation_files \
             (operation_id, path, action, before_hash, after_hash, backup_id, bytes_before, bytes_after) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            operation,
            file.path,
            file.action.as_str(),
            file.before_hash,
            file.after_hash,
            file.backup_id,
            file.bytes_before,
            file.bytes_after,
        ],
    )?;
    Ok(())
}

/// Map one `operations` row; every read selects [`OPERATION_COLUMNS`].
fn raw_operation(row: &Row<'_>) -> rusqlite::Result<RawOperation> {
    Ok(RawOperation {
        id: row.get(0)?,
        kind: row.get(1)?,
        scope: row.get(2)?,
        actor: row.get(3)?,
        status: row.get(4)?,
        summary: row.get(5)?,
        error_kind: row.get(6)?,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        correlation_id: row.get(9)?,
        undo_of: row.get(10)?,
    })
}

/// Every file row of one operation, in apply order.
fn files_for(connection: &Connection, operation: &str) -> Result<Vec<OperationFile>, DbError> {
    let mut statement = connection.prepare(
        "SELECT id, path, action, before_hash, after_hash, backup_id, bytes_before, bytes_after \
           FROM operation_files WHERE operation_id = ?1 ORDER BY id",
    )?;
    let rows = statement
        .query_map([operation], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut files = Vec::with_capacity(rows.len());
    for (id, path, action, before_hash, after_hash, backup_id, bytes_before, bytes_after) in rows {
        files.push(OperationFile {
            id,
            path,
            action: parse_optional(action.as_deref())?,
            before_hash,
            after_hash,
            backup_id,
            bytes_before,
            bytes_after,
        });
    }
    Ok(files)
}

/// How many file rows each of `ids` has.
fn file_counts(connection: &Connection, ids: &[String]) -> Result<Vec<(String, i64)>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(ids.len());
    let mut statement = connection.prepare(&format!(
        "SELECT operation_id, COUNT(*) FROM operation_files \
          WHERE operation_id IN ({placeholders}) \
          GROUP BY operation_id"
    ))?;
    let counts = statement
        .query_map(params_from_iter(ids.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(counts)
}

/// Item and agent names joined to one operation id.
struct DeploymentName {
    operation_id: String,
    item_name: Option<String>,
    agent_display_name: Option<String>,
}

/// Item and agent names for the deployments each of `ids` touched.
///
/// `deployments` (migration v4) records the operation that wrote it, so the
/// timeline reads names without a second query per row. `MIN` picks one name
/// when an operation deployed several items or agents.
fn deployment_names(
    connection: &Connection,
    ids: &[String],
) -> Result<Vec<DeploymentName>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(ids.len());
    let mut statement = connection.prepare(&format!(
        "SELECT d.operation_id, MIN(i.name), MIN(a.display_name) \
           FROM deployments d \
           LEFT JOIN items i ON i.id = d.item_id \
           LEFT JOIN agents a ON a.id = d.agent_id \
          WHERE d.operation_id IN ({placeholders}) \
          GROUP BY d.operation_id"
    ))?;
    let names = statement
        .query_map(params_from_iter(ids.iter()), |row| {
            Ok(DeploymentName {
                operation_id: row.get(0)?,
                item_name: row.get(1)?,
                agent_display_name: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// `?,?,?` for an `IN` list of `count` values.
fn placeholders(count: usize) -> String {
    let mut list = String::with_capacity(count * 2);
    for index in 0..count {
        if index > 0 {
            list.push(',');
        }
        list.push('?');
    }
    list
}

/// Parse an optional stored enum value.
fn parse_optional<T>(value: Option<&str>) -> Result<Option<T>, DbError>
where
    T: FromStr<Err = ParseWireError>,
{
    match value {
        None => Ok(None),
        Some(text) => T::from_str(text).map(Some).map_err(DbError::UnknownValue),
    }
}

/// Lowercase hex SHA-256, the hash shape at-fs records.
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

#[cfg(test)]
mod tests {
    use at_model::Scope;

    use super::{
        FileAction, OperationActor, OperationId, OperationKind, OperationStatus, parse_optional,
    };

    #[test]
    fn operation_kind_wire_names_round_trip() {
        assert_eq!(OperationKind::ALL.len(), 26);
        for kind in OperationKind::ALL {
            let parsed: OperationKind = kind.as_str().parse().expect("wire name parses");
            assert_eq!(parsed, *kind);
        }
        assert!(OperationKind::WIRE_NAMES.contains(&"drift_adopt"));
        assert!("deploy_all_the_things".parse::<OperationKind>().is_err());
    }

    #[test]
    fn enums_reject_unknown_strings() {
        assert!("sudo".parse::<OperationActor>().is_err());
        assert!("waiting".parse::<OperationStatus>().is_err());
        assert!("touched".parse::<FileAction>().is_err());
        assert!("UI".parse::<OperationActor>().is_ok());
        assert_eq!(FileAction::ALL.len(), 5);
    }

    #[test]
    fn only_terminal_statuses_finish_an_operation() {
        assert!(!OperationStatus::Running.is_terminal());
        for status in [
            OperationStatus::Succeeded,
            OperationStatus::Failed,
            OperationStatus::Interrupted,
            OperationStatus::Undone,
        ] {
            assert!(status.is_terminal());
        }
    }

    #[test]
    fn operation_ids_are_ulids_and_sort_by_text() {
        let first = OperationId::new();
        let second = OperationId::new();
        assert_eq!(first.as_str().len(), 26);
        assert_eq!(OperationId::parse(first.as_str()), Ok(first.clone()));
        assert!(OperationId::parse("job_0190").is_err());
        assert_ne!(first, second);
        assert_eq!(first.as_str() < second.as_str(), first < second);
    }

    #[test]
    fn parse_optional_keeps_none_and_reports_bad_values() {
        assert_eq!(
            parse_optional::<Scope>(None).expect("none stays none"),
            None
        );
        let error = parse_optional::<Scope>(Some("galaxy"));
        assert!(matches!(error, Err(crate::DbError::UnknownValue(_))));
    }
}
