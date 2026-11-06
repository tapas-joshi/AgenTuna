//! Presets repository and applied-ratio queries over the TF-017 tables
//! (TS-004): `presets`, `preset_members`, `preset_applications` and
//! `preset_application_members`.
//!
//! This module adds **no DDL** — every statement is a plain
//! SELECT/INSERT/UPDATE/DELETE against the migration-v8 schema, and the
//! in-module test asserts no statement contains `CREATE`, `ALTER` or `DROP`.
//!
//! ## Name uniqueness
//!
//! `presets` enforces `UNIQUE (name COLLATE NOCASE, scope)`. [`create_preset`]
//! and [`rename_preset`] pre-check inside an immediate transaction and surface
//! a duplicate as [`PresetError::DuplicatePresetName`] — the typed error TS-022
//! and the presets editor show instead of a raw constraint message. Callers
//! must not hold an open transaction on the same connection.
//!
//! ## Membership and existence
//!
//! `preset_members.item_id` deliberately has **no** foreign key to `items`
//! (TF-017): a preset may name an item that does not exist yet, and deleting
//! an item does not cascade on its own. [`preset_members`] therefore LEFT
//! JOINs `items` and reports `exists` per member, and [`applied_ratio`]
//! counts only existing members in its denominator — a deleted skill drops
//! out of the ratio without any cleanup. [`remove_members_for_item`] is the
//! explicit cascade item-delete flows (TS-014) call when they want the
//! membership row gone too.
//!
//! ## Application history
//!
//! [`record_application`] writes one `preset_applications` row plus one
//! `preset_application_members` row per changed member carrying `prior_enabled`
//! (the enablement before the apply, `None` when the item did not exist).
//! [`application_members`] returns them in insertion (`id`) order so
//! [`crate::ops::OperationKind::PresetUnapply`] flows can restore exactly
//! what was there. `preset_applications.preset_id` has no foreign key on
//! purpose: deleting a preset keeps its application history.
//!
//! ## Applied ratio
//!
//! [`applied_ratio`] reports `enabled / total` over **existing** members,
//! where `enabled` is `preset_members.enabled = 1`. `target` is accepted and
//! echoed for the caller's context (agent id or project id) but does not
//! change the count: per-target enablement resolution is the apply flow's
//! job (TS-022), which reads deployments, not this table.

use std::str::FromStr;

use at_model::{ItemId, ItemKind, Scope};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::DbError;

/// Why a preset repository call did not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PresetError {
    /// SQLite refused the statement.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A preset with the same name (case-insensitive) already exists in the
    /// scope, from the TF-017 `UNIQUE (name COLLATE NOCASE, scope)` constraint.
    #[error("preset `{name}` already exists in scope {scope}")]
    DuplicatePresetName {
        /// The rejected name, as supplied.
        name: String,
        /// Scope the duplicate was attempted in.
        scope: Scope,
    },
    /// A caller passed a value the schema cannot store.
    #[error("invalid {field}: {reason}")]
    InvalidValue {
        /// Field that rejected the value.
        field: &'static str,
        /// Why the value is invalid.
        reason: String,
    },
}

impl From<rusqlite::Error> for PresetError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

impl From<at_model::ParseWireError> for PresetError {
    fn from(error: at_model::ParseWireError) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// What a `preset_applications.target_kind` row addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PresetTargetKind {
    /// One agent (`target_id` is an agent id).
    #[serde(rename = "agent")]
    Agent,
    /// One project (`target_id` is a project id).
    #[serde(rename = "project")]
    Project,
}

impl PresetTargetKind {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Project => "project",
        }
    }
}

impl FromStr for PresetTargetKind {
    type Err = at_model::ParseWireError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        for kind in [Self::Agent, Self::Project] {
            if text.eq_ignore_ascii_case(kind.as_str()) {
                return Ok(kind);
            }
        }
        Err(at_model::ParseWireError {
            type_name: "PresetTargetKind",
            input: text.to_owned(),
            expected: &["agent", "project"],
        })
    }
}

/// One `presets` row with its member count, as [`list_presets`] returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetRecord {
    /// Preset id.
    pub id: String,
    /// Display name, unique per scope ignoring case.
    pub name: String,
    /// Free-text description.
    pub description: Option<String>,
    /// Scope the preset belongs to.
    pub scope: Scope,
    /// Shipped with AgenTuna rather than user-created.
    pub is_builtin: bool,
    /// Row creation timestamp (RFC 3339 text).
    pub created_at: String,
    /// Row update timestamp (RFC 3339 text).
    pub updated_at: String,
    /// Number of `preset_members` rows, enabled or not.
    pub member_count: i64,
    /// Number of members with `enabled = 1`.
    pub enabled_count: i64,
}

/// One `preset_members` row resolved against `items`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetMemberRecord {
    /// Membership row id.
    pub id: i64,
    /// Owning preset.
    pub preset_id: String,
    /// Referenced item id.
    pub item_id: String,
    /// Kind of the referenced item.
    pub kind: ItemKind,
    /// Whether applying the preset enables this member.
    pub enabled: bool,
    /// Whether an `items` row with `item_id` exists right now.
    pub exists: bool,
    /// Display name of the item, when it exists.
    pub item_name: Option<String>,
    /// Scope of the existing item, when it exists.
    pub item_scope: Option<Scope>,
}

/// Everything [`create_preset`] needs.
#[derive(Clone, Debug)]
pub struct NewPreset<'a> {
    /// Preset id; `None` generates `prst_<ulid>`.
    pub id: Option<&'a str>,
    /// Display name; must be unique in `scope` ignoring case.
    pub name: &'a str,
    /// Free-text description.
    pub description: Option<&'a str>,
    /// Scope the preset belongs to.
    pub scope: Scope,
    /// Whether the preset ships with AgenTuna.
    pub is_builtin: bool,
}

/// One member's state before a preset application changed it.
#[derive(Clone, Debug)]
pub struct ApplicationMemberSpec<'a> {
    /// Item whose enablement the apply changed.
    pub item_id: &'a str,
    /// Enablement before the apply; `None` when the item did not exist.
    pub prior_enabled: Option<bool>,
}

/// Everything [`record_application`] writes.
#[derive(Clone, Debug)]
pub struct NewApplication<'a> {
    /// Preset that was applied.
    pub preset_id: &'a str,
    /// What the preset was applied to.
    pub target_kind: PresetTargetKind,
    /// Agent id or project id the preset was applied to.
    pub target_id: &'a str,
    /// Operation that performed the apply, when recorded.
    pub operation_id: Option<&'a str>,
    /// Members whose state the apply recorded; one history row each.
    pub members: Vec<ApplicationMemberSpec<'a>>,
    /// `member_count` stored on the application row; `None` uses
    /// `members.len()`.
    pub member_count: Option<i64>,
}

/// One `preset_application_members` row as [`application_members`] returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationMemberRecord {
    /// Row id; ascending id is insertion order.
    pub id: i64,
    /// Owning application.
    pub application_id: i64,
    /// Item whose prior state was recorded.
    pub item_id: String,
    /// Enablement before the apply; `None` when the item did not exist.
    pub prior_enabled: Option<bool>,
}

/// One member's contribution to an [`AppliedRatio`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatioMember {
    /// Member item id.
    pub item_id: String,
    /// Kind of the member item.
    pub kind: ItemKind,
    /// Whether applying the preset enables this member.
    pub enabled: bool,
    /// Whether the item exists in `items` right now.
    pub exists: bool,
}

/// The enabled/total ratio of a preset's existing members for a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedRatio {
    /// Preset the ratio describes.
    pub preset_id: String,
    /// Target kind the caller asked about.
    pub target_kind: PresetTargetKind,
    /// Target id the caller asked about.
    pub target_id: String,
    /// Existing members with `enabled = 1`.
    pub enabled: i64,
    /// Existing members total (deleted items excluded).
    pub total: i64,
    /// Whether every existing member is enabled and at least one exists.
    pub complete: bool,
    /// Every member with its flags, in membership order.
    pub members: Vec<RatioMember>,
}

const LIST_PRESETS_SQL: &str = "\
SELECT p.id, p.name, p.description, p.scope, p.is_builtin, p.created_at, \
       p.updated_at, COUNT(m.id) AS member_count, \
       COALESCE(SUM(m.enabled), 0) AS enabled_count \
  FROM presets p \
  LEFT JOIN preset_members m ON m.preset_id = p.id \
 GROUP BY p.id \
 ORDER BY p.name COLLATE NOCASE ASC, p.scope ASC, p.id ASC";

const PRESET_MEMBERS_SQL: &str = "\
SELECT m.id, m.preset_id, m.item_id, m.kind, m.enabled, \
       CASE WHEN i.id IS NULL THEN 0 ELSE 1 END AS item_exists, \
       i.name AS item_name, i.scope AS item_scope \
  FROM preset_members m \
  LEFT JOIN items i ON i.id = m.item_id \
 WHERE m.preset_id = ?1 \
 ORDER BY m.id ASC";

const NAME_TAKEN_SQL: &str = "\
SELECT COUNT(*) FROM presets WHERE name = ?1 COLLATE NOCASE AND scope = ?2";

const INSERT_PRESET_SQL: &str = "\
INSERT INTO presets (id, name, description, scope, is_builtin) \
VALUES (?1, ?2, ?3, ?4, ?5)";

const RENAME_PRESET_SQL: &str = "\
UPDATE presets SET name = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
 WHERE id = ?1";

const DELETE_PRESET_SQL: &str = "DELETE FROM presets WHERE id = ?1";

const UPSERT_MEMBER_SQL: &str = "\
INSERT INTO preset_members (preset_id, item_id, kind, enabled) \
VALUES (?1, ?2, ?3, ?4) \
ON CONFLICT(preset_id, item_id) DO UPDATE SET \
    kind = excluded.kind, enabled = excluded.enabled";

const REMOVE_MEMBER_SQL: &str = "DELETE FROM preset_members WHERE preset_id = ?1 AND item_id = ?2";

const REMOVE_MEMBERS_FOR_ITEM_SQL: &str = "DELETE FROM preset_members WHERE item_id = ?1";

const INSERT_APPLICATION_SQL: &str = "\
INSERT INTO preset_applications (preset_id, target_kind, target_id, \
     operation_id, member_count) \
VALUES (?1, ?2, ?3, ?4, ?5)";

const INSERT_APPLICATION_MEMBER_SQL: &str = "\
INSERT INTO preset_application_members (application_id, item_id, prior_enabled) \
VALUES (?1, ?2, ?3)";

const APPLICATION_MEMBERS_SQL: &str = "\
SELECT id, application_id, item_id, prior_enabled \
  FROM preset_application_members \
 WHERE application_id = ?1 \
 ORDER BY id ASC";

const RATIO_SQL: &str = "\
SELECT m.item_id, m.kind, m.enabled, \
       CASE WHEN i.id IS NULL THEN 0 ELSE 1 END AS item_exists \
  FROM preset_members m \
  LEFT JOIN items i ON i.id = m.item_id \
 WHERE m.preset_id = ?1 \
 ORDER BY m.id ASC";

/// Creates a preset and returns its id.
///
/// `spec.id` of `None` generates `prst_<ulid>`. The name check and the insert
/// run in one immediate transaction, so two concurrent creators cannot both
/// pass the check.
///
/// # Errors
/// [`PresetError::DuplicatePresetName`] when `name` already exists in
/// `scope` ignoring case, or [`PresetError::Db`] on a SQLite failure.
pub fn create_preset(
    connection: &mut Connection,
    spec: &NewPreset<'_>,
) -> Result<String, PresetError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let taken: i64 = transaction.query_row(
        NAME_TAKEN_SQL,
        params![spec.name, spec.scope.wire_name()],
        |row| row.get(0),
    )?;
    if taken > 0 {
        return Err(PresetError::DuplicatePresetName {
            name: spec.name.to_owned(),
            scope: spec.scope,
        });
    }

    let id = spec
        .id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("prst_{}", Ulid::generate()));
    let result = transaction.execute(
        INSERT_PRESET_SQL,
        params![
            id,
            spec.name,
            spec.description,
            spec.scope.wire_name(),
            i64::from(spec.is_builtin),
        ],
    );
    match result {
        Ok(_) => {
            transaction.commit()?;
            Ok(id)
        }
        Err(error) => Err(map_preset_constraint(error, spec.name, spec.scope)),
    }
}

/// Renames a preset, keeping the same id.
///
/// # Errors
/// [`PresetError::DuplicatePresetName`] when `new_name` collides in the
/// preset's scope, [`PresetError::Db`] when the row does not exist is *not*
/// an error (returns `Ok(false)`), or a SQLite failure.
pub fn rename_preset(
    connection: &mut Connection,
    id: &str,
    new_name: &str,
) -> Result<bool, PresetError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let scope: Option<String> = transaction
        .query_row("SELECT scope FROM presets WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(scope_text) = scope else {
        return Ok(false);
    };
    let scope = Scope::from_str(&scope_text)?;

    let taken: i64 = transaction.query_row(
        NAME_TAKEN_SQL,
        params![new_name, scope.wire_name()],
        |row| row.get(0),
    )?;
    // The preset's own row counts as "taken" only when the name is unchanged;
    // renaming to the same name is a no-op success.
    let own_name: Option<String> = transaction
        .query_row("SELECT name FROM presets WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    let same_name = own_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(new_name));
    if taken > 0 && !same_name {
        return Err(PresetError::DuplicatePresetName {
            name: new_name.to_owned(),
            scope,
        });
    }

    let changed = transaction.execute(RENAME_PRESET_SQL, params![id, new_name])?;
    transaction.commit()?;
    Ok(changed == 1)
}

/// Deletes a preset; `preset_members` cascade, applications remain as history.
///
/// Returns whether a row was deleted.
///
/// # Errors
/// A SQLite failure.
pub fn delete_preset(connection: &Connection, id: &str) -> Result<bool, DbError> {
    let changed = connection.execute(DELETE_PRESET_SQL, [id])?;
    Ok(changed == 1)
}

/// Every preset with its member and enabled counts, ordered by name.
///
/// # Errors
/// A SQLite failure or an unknown stored scope.
pub fn list_presets(connection: &Connection) -> Result<Vec<PresetRecord>, DbError> {
    let mut statement = connection.prepare(LIST_PRESETS_SQL)?;
    let rows = statement.query_map([], preset_record_from_row)?;
    let mut presets = Vec::new();
    for row in rows {
        presets.push(PresetRecord::try_from(row?)?);
    }
    Ok(presets)
}

/// Members of one preset resolved against `items`, in membership order.
///
/// `exists` is false when the item was deleted; `item_name`/`item_scope` are
/// `None` then. See the module docs for why membership survives item deletion.
///
/// # Errors
/// A SQLite failure or an unknown stored kind/scope.
pub fn preset_members(
    connection: &Connection,
    preset_id: &str,
) -> Result<Vec<PresetMemberRecord>, DbError> {
    let mut statement = connection.prepare(PRESET_MEMBERS_SQL)?;
    let rows = statement.query_map([preset_id], preset_member_from_row)?;
    let mut members = Vec::new();
    for row in rows {
        members.push(PresetMemberRecord::try_from(row?)?);
    }
    Ok(members)
}

/// Adds or updates one member of a preset.
///
/// Upserts on `UNIQUE (preset_id, item_id)`: a second call for the same item
/// replaces `kind` and `enabled` instead of duplicating the row.
///
/// # Errors
/// A SQLite failure, including a `preset_id` that does not exist.
pub fn set_member(
    connection: &Connection,
    preset_id: &str,
    item_id: &ItemId,
    kind: ItemKind,
    enabled: bool,
) -> Result<(), DbError> {
    connection.execute(
        UPSERT_MEMBER_SQL,
        params![
            preset_id,
            item_id.to_string(),
            kind.wire_name(),
            i64::from(enabled),
        ],
    )?;
    Ok(())
}

/// Removes one member from a preset. Returns whether a row was removed.
///
/// # Errors
/// A SQLite failure.
pub fn remove_member(
    connection: &Connection,
    preset_id: &str,
    item_id: &ItemId,
) -> Result<bool, DbError> {
    let changed = connection.execute(REMOVE_MEMBER_SQL, params![preset_id, item_id.to_string()])?;
    Ok(changed == 1)
}

/// Removes every membership row naming `item_id` — the explicit cascade an
/// item-delete flow runs because `preset_members.item_id` has no foreign key.
///
/// Returns the number of rows removed.
///
/// # Errors
/// A SQLite failure.
pub fn remove_members_for_item(connection: &Connection, item_id: &ItemId) -> Result<u64, DbError> {
    let changed = connection.execute(REMOVE_MEMBERS_FOR_ITEM_SQL, [item_id.to_string()])?;
    Ok(u64::try_from(changed).unwrap_or(u64::MAX))
}

/// Records one preset application and the prior state of every changed member
/// in a single transaction.
///
/// Returns the new `preset_applications.id`. `member_count` defaults to the
/// number of member rows written.
///
/// # Errors
/// A SQLite failure, including an `application_id` foreign-key violation on
/// `preset_application_members` (cannot happen — the application row is
/// inserted first in the same transaction).
pub fn record_application(
    connection: &mut Connection,
    new: &NewApplication<'_>,
) -> Result<i64, PresetError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    transaction.execute(
        INSERT_APPLICATION_SQL,
        params![
            new.preset_id,
            new.target_kind.as_str(),
            new.target_id,
            new.operation_id,
            new.member_count
                .unwrap_or_else(|| { i64::try_from(new.members.len()).unwrap_or(i64::MAX) }),
        ],
    )?;
    let application_id = transaction.last_insert_rowid();

    for member in &new.members {
        transaction.execute(
            INSERT_APPLICATION_MEMBER_SQL,
            params![
                application_id,
                member.item_id,
                member.prior_enabled.map(i64::from),
            ],
        )?;
    }

    transaction.commit()?;
    Ok(application_id)
}

/// The recorded prior states of one application, in insertion order.
///
/// Unapply restores exactly these values; `prior_enabled = None` means the
/// item did not exist before the apply.
///
/// # Errors
/// A SQLite failure.
pub fn application_members(
    connection: &Connection,
    application_id: i64,
) -> Result<Vec<ApplicationMemberRecord>, DbError> {
    let mut statement = connection.prepare(APPLICATION_MEMBERS_SQL)?;
    let rows = statement.query_map([application_id], |row| {
        Ok(ApplicationMemberRecord {
            id: row.get(0)?,
            application_id: row.get(1)?,
            item_id: row.get(2)?,
            prior_enabled: row.get::<_, Option<i64>>(3)?.map(|value| value != 0),
        })
    })?;
    let mut members = Vec::new();
    for row in rows {
        members.push(row?);
    }
    Ok(members)
}

/// The enabled/total ratio of a preset's existing members for a target.
///
/// `total` counts members whose item still exists in `items`; `enabled`
/// counts those with `preset_members.enabled = 1`. `complete` is true only
/// when at least one member exists and all of them are enabled — an empty or
/// fully-deleted preset reports `0/0`, `complete = false`. See the module
/// docs for why `target` does not filter the count.
///
/// # Errors
/// A SQLite failure or an unknown stored kind.
pub fn applied_ratio(
    connection: &Connection,
    preset_id: &str,
    target_kind: PresetTargetKind,
    target_id: &str,
) -> Result<AppliedRatio, PresetError> {
    let mut statement = connection.prepare(RATIO_SQL)?;
    let rows = statement.query_map([preset_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    let mut members = Vec::new();
    let mut enabled = 0_i64;
    let mut total = 0_i64;
    for row in rows {
        let (item_id, kind, member_enabled, exists) = row?;
        let kind = ItemKind::from_str(&kind).map_err(DbError::from)?;
        let member_enabled = member_enabled != 0;
        let exists = exists != 0;
        if exists {
            total += 1;
            if member_enabled {
                enabled += 1;
            }
        }
        members.push(RatioMember {
            item_id,
            kind,
            enabled: member_enabled,
            exists,
        });
    }

    Ok(AppliedRatio {
        preset_id: preset_id.to_owned(),
        target_kind,
        target_id: target_id.to_owned(),
        enabled,
        total,
        complete: total > 0 && enabled == total,
        members,
    })
}

/// A `presets` row as SQLite returns it, before enum parsing.
struct RawPresetRecord {
    id: String,
    name: String,
    description: Option<String>,
    scope: String,
    is_builtin: i64,
    created_at: String,
    updated_at: String,
    member_count: i64,
    enabled_count: i64,
}

fn preset_record_from_row(row: &Row<'_>) -> rusqlite::Result<RawPresetRecord> {
    Ok(RawPresetRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        scope: row.get(3)?,
        is_builtin: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        member_count: row.get(7)?,
        enabled_count: row.get(8)?,
    })
}

impl TryFrom<RawPresetRecord> for PresetRecord {
    type Error = DbError;

    fn try_from(raw: RawPresetRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            name: raw.name,
            description: raw.description,
            scope: Scope::from_str(&raw.scope).map_err(DbError::from)?,
            is_builtin: raw.is_builtin != 0,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
            member_count: raw.member_count,
            enabled_count: raw.enabled_count,
        })
    }
}

/// A resolved `preset_members` row as SQLite returns it.
struct RawPresetMemberRecord {
    id: i64,
    preset_id: String,
    item_id: String,
    kind: String,
    enabled: i64,
    exists: i64,
    item_name: Option<String>,
    item_scope: Option<String>,
}

fn preset_member_from_row(row: &Row<'_>) -> rusqlite::Result<RawPresetMemberRecord> {
    Ok(RawPresetMemberRecord {
        id: row.get(0)?,
        preset_id: row.get(1)?,
        item_id: row.get(2)?,
        kind: row.get(3)?,
        enabled: row.get(4)?,
        exists: row.get(5)?,
        item_name: row.get(6)?,
        item_scope: row.get(7)?,
    })
}

impl TryFrom<RawPresetMemberRecord> for PresetMemberRecord {
    type Error = DbError;

    fn try_from(raw: RawPresetMemberRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            preset_id: raw.preset_id,
            item_id: raw.item_id,
            kind: ItemKind::from_str(&raw.kind).map_err(DbError::from)?,
            enabled: raw.enabled != 0,
            exists: raw.exists != 0,
            item_name: raw.item_name,
            item_scope: raw
                .item_scope
                .map(|text| Scope::from_str(&text).map_err(DbError::from))
                .transpose()?,
        })
    }
}

/// Maps a constraint violation on the name+scope unique index to the typed
/// error; anything else stays a [`DbError`].
fn map_preset_constraint(error: rusqlite::Error, name: &str, scope: Scope) -> PresetError {
    let db_error = DbError::from(error);
    if let DbError::Constraint { message } = &db_error
        && (message.contains("presets.name") || message.contains("sqlite_autoindex_presets"))
    {
        return PresetError::DuplicatePresetName {
            name: name.to_owned(),
            scope,
        };
    }
    PresetError::Db(db_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every statement this module issues must compile against the shipped
    /// schema and must not change it — TS-004 owns no DDL.
    #[test]
    fn every_statement_matches_the_shipped_schema() {
        let database = crate::Database::open_in_memory().expect("in-memory database opens");
        let connection = database.connection();
        for statement in [
            LIST_PRESETS_SQL,
            PRESET_MEMBERS_SQL,
            NAME_TAKEN_SQL,
            INSERT_PRESET_SQL,
            RENAME_PRESET_SQL,
            DELETE_PRESET_SQL,
            UPSERT_MEMBER_SQL,
            REMOVE_MEMBER_SQL,
            REMOVE_MEMBERS_FOR_ITEM_SQL,
            INSERT_APPLICATION_SQL,
            INSERT_APPLICATION_MEMBER_SQL,
            APPLICATION_MEMBERS_SQL,
            RATIO_SQL,
            "SELECT scope FROM presets WHERE id = ?1",
            "SELECT name FROM presets WHERE id = ?1",
        ] {
            connection
                .prepare(statement)
                .unwrap_or_else(|error| panic!("statement does not compile: {error}\n{statement}"));
        }
        for statement in [
            INSERT_PRESET_SQL,
            RENAME_PRESET_SQL,
            DELETE_PRESET_SQL,
            UPSERT_MEMBER_SQL,
            REMOVE_MEMBER_SQL,
            REMOVE_MEMBERS_FOR_ITEM_SQL,
            INSERT_APPLICATION_SQL,
            INSERT_APPLICATION_MEMBER_SQL,
        ] {
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
