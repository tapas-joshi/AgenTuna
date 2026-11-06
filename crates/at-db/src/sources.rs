//! Source and update-tracking repository over `sources` (TS-003).
//!
//! One `sources` row records where an item came from (git, marketplace,
//! library, folder, zip or local) and what the remote said at the last check:
//! `remote_sha` is the upstream head, `commit_sha` the commit the item was
//! installed from, `ignored_sha` the commit the user dismissed, and
//! `local_modified` marks content that diverged from `content_hash` since
//! import (auto-update skips those rows).
//!
//! ## One row per item
//!
//! The TF-015 schema has no `UNIQUE (item_id)` and no scope column — an
//! item's scope lives on its `items` row, so "per item and scope" resolves to
//! "per item" here. [`upsert_source`] therefore keeps exactly one live row per
//! `item_id`: it updates the existing row in place (preferring a non-archived
//! one) inside an immediate transaction, or inserts when none exists. The
//! check-then-write is serialized by `BEGIN IMMEDIATE`; callers must not hold
//! an open transaction on the same connection.
//!
//! ## Archive, never delete
//!
//! [`archive_source`] sets `deleted_at`; no function in this module issues
//! `DELETE FROM sources`. History keeps the row so the origin of a removed
//! item stays explainable, matching TS-014's "archives the sources row".
//!
//! ## Auto-update resolution
//!
//! `sources.auto_update` is a tri-state override: `Off` (0), `On` (1) or
//! `Inherit` (2, also the column default when unset). [`effective_auto_update`]
//! resolves it against the global `auto_update_skills` setting (TF-023,
//! default off): an explicit `On` or `Off` wins, `Inherit` or a missing row
//! yields the global flag. Per-item `On` beats a sibling row's `Off` when a
//! data anomaly ever produces two live rows for one item.

use std::str::FromStr;

use at_model::{ItemId, ParseWireError};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::DbError;
use crate::settings;

/// Kind of source an item was installed or linked from.
///
/// Wire names match the `sources.kind` CHECK constraint exactly, including
/// `local` (a folder on this machine that is not a picked import).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SourceKind {
    /// A Git repository.
    #[serde(rename = "git")]
    Git,
    /// A marketplace listing.
    #[serde(rename = "marketplace")]
    Marketplace,
    /// Another AgenTuna store or project library.
    #[serde(rename = "library")]
    Library,
    /// A folder the user picked in the import dialog.
    #[serde(rename = "folder")]
    Folder,
    /// A zip archive.
    #[serde(rename = "zip")]
    Zip,
    /// A local path that is not a one-shot import.
    #[serde(rename = "local")]
    Local,
}

impl SourceKind {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Git,
        Self::Marketplace,
        Self::Library,
        Self::Folder,
        Self::Zip,
        Self::Local,
    ];

    /// Every stored value, in declaration order.
    pub const WIRE_NAMES: &'static [&'static str] =
        &["git", "marketplace", "library", "folder", "zip", "local"];

    /// The stable snake_case value stored in SQLite.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Marketplace => "marketplace",
            Self::Library => "library",
            Self::Folder => "folder",
            Self::Zip => "zip",
            Self::Local => "local",
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SourceKind {
    type Err = ParseWireError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        for kind in Self::ALL {
            if text.eq_ignore_ascii_case(kind.as_str()) {
                return Ok(*kind);
            }
        }
        Err(ParseWireError {
            type_name: "SourceKind",
            input: text.to_owned(),
            expected: Self::WIRE_NAMES,
        })
    }
}

/// Per-item auto-update override, stored as `sources.auto_update`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AutoUpdate {
    /// Never auto-update this item (0).
    Off,
    /// Always auto-update this item (1).
    On,
    /// Follow the global `auto_update_skills` setting (2, the column default).
    Inherit,
}

impl AutoUpdate {
    /// The stored integer value.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Inherit => 2,
        }
    }

    /// Parses a stored `auto_update` value.
    ///
    /// Returns `None` for values outside `0..=2`; the column CHECK makes that
    /// unreachable through this module's writes.
    #[must_use]
    pub const fn from_i64(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::On),
            2 => Some(Self::Inherit),
            _ => None,
        }
    }
}

impl serde::Serialize for AutoUpdate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.as_i64())
    }
}

impl<'de> serde::Deserialize<'de> for AutoUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        Self::from_i64(value).ok_or_else(|| {
            serde::de::Error::custom(format!("auto_update must be 0, 1 or 2, got {value}"))
        })
    }
}

/// One `sources` row as read back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRow {
    /// Source row id.
    pub id: String,
    /// Item this source belongs to; `None` for unlinked/orphaned rows.
    pub item_id: Option<String>,
    /// Where the item came from.
    pub kind: SourceKind,
    /// Repository URL or local folder path (`folder`/`local` kinds store the
    /// path here — the schema has a single location column).
    pub url: Option<String>,
    /// Folder inside the source holding the item.
    pub subpath: Option<String>,
    /// Commit the item was installed from.
    pub commit_sha: Option<String>,
    /// Ref the user pinned (branch or tag).
    pub git_ref: Option<String>,
    /// Version reported by the source, when it has one.
    pub version: Option<String>,
    /// Content hash recorded at import; `local_modified` compares against it.
    pub content_hash: Option<String>,
    /// Marketplace listing id.
    pub marketplace_id: Option<String>,
    /// Marketplace install count at last fetch.
    pub install_count: Option<i64>,
    /// HTTP ETag from the last marketplace fetch.
    pub etag: Option<String>,
    /// Upstream head sha at `last_checked_at`.
    pub remote_sha: Option<String>,
    /// Commit the user dismissed so it stops showing as an update.
    pub ignored_sha: Option<String>,
    /// Whether on-disk content diverged from `content_hash`.
    pub local_modified: bool,
    /// Per-item auto-update override.
    pub auto_update: Option<AutoUpdate>,
    /// When the row was archived; `None` while live.
    pub deleted_at: Option<String>,
    /// When the item was (re)installed from this source.
    pub installed_at: Option<String>,
    /// When an update check last ran.
    pub last_checked_at: Option<String>,
    /// Whether the last check found `remote_sha != commit_sha`.
    pub update_available: bool,
    /// License string reported by the source, when known.
    pub license: Option<String>,
}

/// Everything [`upsert_source`] writes for one import or link.
///
/// `None` clears the column on update; the exception is `auto_update`, where
/// `None` keeps the stored override so a re-import does not silently reset a
/// user preference.
#[derive(Clone, Debug)]
pub struct SourceSpec<'a> {
    /// Item the source belongs to.
    pub item_id: &'a ItemId,
    /// Where the item came from.
    pub kind: SourceKind,
    /// Repository URL or local path.
    pub url: Option<&'a str>,
    /// Folder inside the source holding the item.
    pub subpath: Option<&'a str>,
    /// Commit the item was installed from.
    pub commit_sha: Option<&'a str>,
    /// Pinned ref (branch or tag).
    pub git_ref: Option<&'a str>,
    /// Version reported by the source.
    pub version: Option<&'a str>,
    /// Content hash recorded at import.
    pub content_hash: Option<&'a str>,
    /// Marketplace listing id.
    pub marketplace_id: Option<&'a str>,
    /// Marketplace install count.
    pub install_count: Option<i64>,
    /// HTTP ETag from the last fetch.
    pub etag: Option<&'a str>,
    /// License string.
    pub license: Option<&'a str>,
    /// Per-item auto-update override; `None` preserves the stored value.
    pub auto_update: Option<AutoUpdate>,
}

impl<'a> SourceSpec<'a> {
    /// A spec with only the required fields set.
    #[must_use]
    pub const fn new(item_id: &'a ItemId, kind: SourceKind) -> Self {
        Self {
            item_id,
            kind,
            url: None,
            subpath: None,
            commit_sha: None,
            git_ref: None,
            version: None,
            content_hash: None,
            marketplace_id: None,
            install_count: None,
            etag: None,
            license: None,
            auto_update: None,
        }
    }
}

/// What an update check learned, written by [`record_update_check`].
#[derive(Clone, Debug, Default)]
pub struct CheckOutcome<'a> {
    /// Upstream head sha the check resolved.
    pub remote_sha: Option<&'a str>,
    /// HTTP ETag returned by the check.
    pub etag: Option<&'a str>,
    /// When the check ran; `None` uses the database clock.
    pub checked_at: Option<&'a str>,
    /// Whether the check found an update (`remote_sha != commit_sha`).
    pub update_available: bool,
}

const LIVE_FOR_ITEM_SQL: &str = "\
SELECT id FROM sources \
 WHERE item_id = ?1 \
 ORDER BY (deleted_at IS NULL) DESC, rowid DESC \
 LIMIT 1";

const HISTORY_SQL: &str = "\
SELECT id, item_id, kind, url, subpath, commit_sha, git_ref, version, \
       content_hash, marketplace_id, install_count, etag, remote_sha, \
       ignored_sha, local_modified, auto_update, deleted_at, installed_at, \
       last_checked_at, update_available, license \
  FROM sources WHERE item_id = ?1 ORDER BY rowid DESC";

const SOURCE_BY_ID_SQL: &str = "\
SELECT id, item_id, kind, url, subpath, commit_sha, git_ref, version, \
       content_hash, marketplace_id, install_count, etag, remote_sha, \
       ignored_sha, local_modified, auto_update, deleted_at, installed_at, \
       last_checked_at, update_available, license \
  FROM sources WHERE id = ?1";

const SOURCE_FOR_ITEM_SQL: &str = "\
SELECT id, item_id, kind, url, subpath, commit_sha, git_ref, version, \
       content_hash, marketplace_id, install_count, etag, remote_sha, \
       ignored_sha, local_modified, auto_update, deleted_at, installed_at, \
       last_checked_at, update_available, license \
  FROM sources WHERE item_id = ?1 AND deleted_at IS NULL \
 ORDER BY rowid DESC LIMIT 1";

const UPDATE_CANDIDATES_SQL: &str = "\
SELECT id, item_id, kind, url, subpath, commit_sha, git_ref, version, \
       content_hash, marketplace_id, install_count, etag, remote_sha, \
       ignored_sha, local_modified, auto_update, deleted_at, installed_at, \
       last_checked_at, update_available, license \
  FROM sources \
 WHERE deleted_at IS NULL \
   AND kind IN ('git', 'marketplace') \
   AND remote_sha IS NOT NULL \
   AND commit_sha IS NOT NULL \
   AND remote_sha != commit_sha \
   AND (ignored_sha IS NULL OR ignored_sha != remote_sha) \
 ORDER BY item_id ASC";

const AUTO_UPDATE_SQL: &str = "\
SELECT auto_update FROM sources WHERE item_id = ?1 AND deleted_at IS NULL";

const ARCHIVE_SQL: &str = "\
UPDATE sources SET deleted_at = COALESCE(?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
 WHERE id = ?1 AND deleted_at IS NULL";

const RECORD_CHECK_SQL: &str = "\
UPDATE sources SET \
    remote_sha = ?2, \
    etag = ?3, \
    last_checked_at = COALESCE(?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), \
    update_available = ?5 \
 WHERE id = ?1";

const SET_AUTO_UPDATE_SQL: &str = "\
UPDATE sources SET auto_update = ?2 WHERE id = ?1";

const SET_LOCAL_MODIFIED_SQL: &str = "\
UPDATE sources SET local_modified = ?2 WHERE id = ?1";

const SET_IGNORED_SHA_SQL: &str = "\
UPDATE sources SET ignored_sha = ?2 WHERE id = ?1";

const SET_COMMIT_SQL: &str = "\
UPDATE sources SET commit_sha = ?2, content_hash = ?3, update_available = 0 \
 WHERE id = ?1";

const INSERT_SOURCE_SQL: &str = "\
INSERT INTO sources (id, item_id, kind, url, subpath, commit_sha, git_ref, \
     version, content_hash, marketplace_id, install_count, etag, remote_sha, \
     ignored_sha, local_modified, auto_update, deleted_at, installed_at, \
     last_checked_at, update_available, license) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, 0, ?13, \
        NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL, 0, ?14)";

const UPDATE_SOURCE_SQL: &str = "\
UPDATE sources SET \
    kind = ?2, url = ?3, subpath = ?4, commit_sha = ?5, git_ref = ?6, \
    version = ?7, content_hash = ?8, marketplace_id = ?9, install_count = ?10, \
    etag = ?11, remote_sha = NULL, ignored_sha = NULL, local_modified = 0, \
    update_available = 0, last_checked_at = NULL, \
    auto_update = COALESCE(?12, auto_update), \
    deleted_at = NULL, \
    installed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
    license = ?13 \
 WHERE id = ?1";

/// Writes the source row for an import or link: one live row per item.
///
/// When a row already exists for `item_id` (archived rows included) it is
/// updated in place inside an immediate transaction — the import metadata is
/// replaced, update-tracking fields (`remote_sha`, `ignored_sha`,
/// `local_modified`, `update_available`, `last_checked_at`) reset because the
/// new content has not been checked yet, `deleted_at` cleared so a re-import
/// revives an archived row, and `auto_update` kept unless `spec.auto_update`
/// is `Some`. Otherwise a new row is inserted with a fresh `src_`-prefixed id.
///
/// # Errors
///
/// [`DbError`] on a SQLite failure or when called while the connection already
/// has an open transaction (`BEGIN IMMEDIATE` would fail). The item foreign
/// key is `ON DELETE SET NULL`, so an unknown `item_id` still inserts — the
/// row simply stays unlinked until a matching item appears.
pub fn upsert_source(
    connection: &mut Connection,
    spec: &SourceSpec<'_>,
) -> Result<String, DbError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let existing: Option<String> = transaction
        .query_row(LIVE_FOR_ITEM_SQL, [spec.item_id.to_string()], |row| {
            row.get(0)
        })
        .optional()?;

    let id = match existing {
        Some(id) => {
            transaction.execute(
                UPDATE_SOURCE_SQL,
                params![
                    id,
                    spec.kind.as_str(),
                    spec.url,
                    spec.subpath,
                    spec.commit_sha,
                    spec.git_ref,
                    spec.version,
                    spec.content_hash,
                    spec.marketplace_id,
                    spec.install_count,
                    spec.etag,
                    spec.auto_update.map(AutoUpdate::as_i64),
                    spec.license,
                ],
            )?;
            id
        }
        None => {
            let id = format!("src_{}", ulid::Ulid::generate());
            transaction.execute(
                INSERT_SOURCE_SQL,
                params![
                    id,
                    spec.item_id.to_string(),
                    spec.kind.as_str(),
                    spec.url,
                    spec.subpath,
                    spec.commit_sha,
                    spec.git_ref,
                    spec.version,
                    spec.content_hash,
                    spec.marketplace_id,
                    spec.install_count,
                    spec.etag,
                    spec.auto_update.map(AutoUpdate::as_i64),
                    spec.license,
                ],
            )?;
            id
        }
    };

    transaction.commit()?;
    Ok(id)
}

/// The live (non-archived) source row for `item_id`, newest first if several
/// exist.
///
/// # Errors
/// A SQLite failure or an unknown stored `kind`/`auto_update` value.
pub fn source_for(connection: &Connection, item_id: &ItemId) -> Result<Option<SourceRow>, DbError> {
    let mut statement = connection.prepare(SOURCE_FOR_ITEM_SQL)?;
    let row = statement
        .query_row([item_id.to_string()], source_row_from_row)
        .optional()?;
    row.map(|raw| raw.try_into()).transpose()
}

/// Every source row ever recorded for `item_id`, archived included, newest
/// first — the origin history a deleted item's audit needs.
///
/// # Errors
/// Same as [`source_for`].
pub fn source_history(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Vec<SourceRow>, DbError> {
    let mut statement = connection.prepare(HISTORY_SQL)?;
    let rows = statement.query_map([item_id.to_string()], source_row_from_row)?;
    collect_sources(rows)
}

/// One source row by id, archived included.
///
/// # Errors
/// Same as [`source_for`].
pub fn get_source(connection: &Connection, id: &str) -> Result<Option<SourceRow>, DbError> {
    let mut statement = connection.prepare(SOURCE_BY_ID_SQL)?;
    let row = statement.query_row([id], source_row_from_row).optional()?;
    row.map(|raw| raw.try_into()).transpose()
}

/// Git and marketplace sources whose remote head differs from the installed
/// commit, minus rows whose `ignored_sha` matches that remote head.
///
/// This is the update-badge feed: `remote_sha != commit_sha` is the signal,
/// `ignored_sha` is the user's "not this one" dismissal, and archived rows
/// never appear. `local_modified` rows are still returned — the badge shows,
/// and the update flow decides to skip or override.
///
/// # Errors
/// A SQLite failure or an unknown stored enum value.
pub fn update_candidates(connection: &Connection) -> Result<Vec<SourceRow>, DbError> {
    let mut statement = connection.prepare(UPDATE_CANDIDATES_SQL)?;
    let rows = statement.query_map([], source_row_from_row)?;
    collect_sources(rows)
}

/// Whether auto-update is effectively on for `item_id`.
///
/// Resolution order: any live source row with `auto_update = On` wins; then
/// any with `Off`; otherwise the row is `Inherit` (or absent) and the global
/// `auto_update_skills` setting decides — which defaults to off when unset or
/// undecodable (TF-023 semantics via [`settings::load`]).
///
/// # Errors
/// A SQLite or settings failure.
pub fn effective_auto_update(connection: &Connection, item_id: &ItemId) -> Result<bool, DbError> {
    let mut statement = connection.prepare(AUTO_UPDATE_SQL)?;
    let overrides = statement
        .query_map([item_id.to_string()], |row| row.get::<_, Option<i64>>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut saw_off = false;
    for value in overrides.into_iter().flatten() {
        match AutoUpdate::from_i64(value) {
            Some(AutoUpdate::On) => return Ok(true),
            Some(AutoUpdate::Off) => saw_off = true,
            Some(AutoUpdate::Inherit) | None => {}
        }
    }
    if saw_off {
        return Ok(false);
    }

    let settings = settings::load(connection).map_err(|error| match error {
        settings::SettingsError::Db(db) => db,
        other => DbError::InvalidValue {
            field: "settings.auto_update_skills",
            reason: other.to_string(),
        },
    })?;
    Ok(settings.auto_update_skills)
}

/// Archives a source row by setting `deleted_at`; the row is never removed.
///
/// `at` is an RFC 3339 timestamp for deterministic tests; `None` uses the
/// database clock. Returns whether a live row was archived — `false` when the
/// id is unknown or already archived.
///
/// # Errors
/// A SQLite failure.
pub fn archive_source(
    connection: &Connection,
    id: &str,
    at: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(ARCHIVE_SQL, params![id, at])?;
    Ok(changed == 1)
}

/// Records what an update check learned for one source row.
///
/// Writes `remote_sha`, `etag`, `last_checked_at` and `update_available`
/// together so the badge and the candidate query never see a half-written
/// check. Returns whether the row exists.
///
/// # Errors
/// A SQLite failure.
pub fn record_update_check(
    connection: &Connection,
    id: &str,
    outcome: &CheckOutcome<'_>,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        RECORD_CHECK_SQL,
        params![
            id,
            outcome.remote_sha,
            outcome.etag,
            outcome.checked_at,
            i64::from(outcome.update_available),
        ],
    )?;
    Ok(changed == 1)
}

/// Sets the per-item auto-update override (`Off`, `On` or `Inherit`).
///
/// # Errors
/// A SQLite failure; returns `false` when the row does not exist.
pub fn set_auto_update(
    connection: &Connection,
    id: &str,
    auto_update: AutoUpdate,
) -> Result<bool, DbError> {
    let changed = connection.execute(SET_AUTO_UPDATE_SQL, params![id, auto_update.as_i64()])?;
    Ok(changed == 1)
}

/// Marks whether the on-disk content diverged from `content_hash`.
///
/// Auto-update reads this to skip locally modified skills (TS-024).
///
/// # Errors
/// A SQLite failure; returns `false` when the row does not exist.
pub fn set_local_modified(
    connection: &Connection,
    id: &str,
    modified: bool,
) -> Result<bool, DbError> {
    let changed = connection.execute(SET_LOCAL_MODIFIED_SQL, params![id, i64::from(modified)])?;
    Ok(changed == 1)
}

/// Records the commit the user dismissed (`ignored_sha`), or clears it with
/// `None` so the version shows as an update candidate again.
///
/// # Errors
/// A SQLite failure; returns `false` when the row does not exist.
pub fn set_ignored_sha(
    connection: &Connection,
    id: &str,
    ignored_sha: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(SET_IGNORED_SHA_SQL, params![id, ignored_sha])?;
    Ok(changed == 1)
}

/// Bumps `commit_sha` (and optionally `content_hash`) after an update was
/// applied, clearing `update_available`.
///
/// # Errors
/// A SQLite failure; returns `false` when the row does not exist.
pub fn set_installed_commit(
    connection: &Connection,
    id: &str,
    commit_sha: &str,
    content_hash: Option<&str>,
) -> Result<bool, DbError> {
    let changed = connection.execute(SET_COMMIT_SQL, params![id, commit_sha, content_hash])?;
    Ok(changed == 1)
}

/// A `sources` row as SQLite returns it, before enum parsing.
struct RawSourceRow {
    id: String,
    item_id: Option<String>,
    kind: String,
    url: Option<String>,
    subpath: Option<String>,
    commit_sha: Option<String>,
    git_ref: Option<String>,
    version: Option<String>,
    content_hash: Option<String>,
    marketplace_id: Option<String>,
    install_count: Option<i64>,
    etag: Option<String>,
    remote_sha: Option<String>,
    ignored_sha: Option<String>,
    local_modified: i64,
    auto_update: Option<i64>,
    deleted_at: Option<String>,
    installed_at: Option<String>,
    last_checked_at: Option<String>,
    update_available: i64,
    license: Option<String>,
}

fn source_row_from_row(row: &Row<'_>) -> rusqlite::Result<RawSourceRow> {
    Ok(RawSourceRow {
        id: row.get(0)?,
        item_id: row.get(1)?,
        kind: row.get(2)?,
        url: row.get(3)?,
        subpath: row.get(4)?,
        commit_sha: row.get(5)?,
        git_ref: row.get(6)?,
        version: row.get(7)?,
        content_hash: row.get(8)?,
        marketplace_id: row.get(9)?,
        install_count: row.get(10)?,
        etag: row.get(11)?,
        remote_sha: row.get(12)?,
        ignored_sha: row.get(13)?,
        local_modified: row.get(14)?,
        auto_update: row.get(15)?,
        deleted_at: row.get(16)?,
        installed_at: row.get(17)?,
        last_checked_at: row.get(18)?,
        update_available: row.get(19)?,
        license: row.get(20)?,
    })
}

impl TryFrom<RawSourceRow> for SourceRow {
    type Error = DbError;

    fn try_from(raw: RawSourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            item_id: raw.item_id,
            kind: SourceKind::from_str(&raw.kind)?,
            url: raw.url,
            subpath: raw.subpath,
            commit_sha: raw.commit_sha,
            git_ref: raw.git_ref,
            version: raw.version,
            content_hash: raw.content_hash,
            marketplace_id: raw.marketplace_id,
            install_count: raw.install_count,
            etag: raw.etag,
            remote_sha: raw.remote_sha,
            ignored_sha: raw.ignored_sha,
            local_modified: raw.local_modified != 0,
            auto_update: raw
                .auto_update
                .map(|value| {
                    AutoUpdate::from_i64(value).ok_or_else(|| DbError::InvalidValue {
                        field: "sources.auto_update",
                        reason: format!("stored value {value} is outside 0..=2"),
                    })
                })
                .transpose()?,
            deleted_at: raw.deleted_at,
            installed_at: raw.installed_at,
            last_checked_at: raw.last_checked_at,
            update_available: raw.update_available != 0,
            license: raw.license,
        })
    }
}

fn collect_sources(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<RawSourceRow>>,
) -> Result<Vec<SourceRow>, DbError> {
    let mut sources = Vec::new();
    for row in rows {
        sources.push(SourceRow::try_from(row?)?);
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every statement this module issues must compile against the shipped
    /// schema and must not change it.
    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        for statement in [
            LIVE_FOR_ITEM_SQL,
            HISTORY_SQL,
            SOURCE_BY_ID_SQL,
            SOURCE_FOR_ITEM_SQL,
            UPDATE_CANDIDATES_SQL,
            AUTO_UPDATE_SQL,
            ARCHIVE_SQL,
            RECORD_CHECK_SQL,
            SET_AUTO_UPDATE_SQL,
            SET_LOCAL_MODIFIED_SQL,
            SET_IGNORED_SHA_SQL,
            SET_COMMIT_SQL,
            INSERT_SOURCE_SQL,
            UPDATE_SOURCE_SQL,
        ] {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
        }
        // INSERT/UPDATE need bound parameters; prepare() alone does not check
        // them, so the no-DDL guard below is what pins the write shape.
        for statement in [INSERT_SOURCE_SQL, UPDATE_SOURCE_SQL] {
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
