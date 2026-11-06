//! Cross-slice row structs and the deployment-matrix query (TF-021).
//!
//! Every list screen (skills, MCP, agents, effective config, drift, backups,
//! history) renders the same projection of one database row. Those structs
//! live here, once, so the domain repositories — skills in TS-001, MCP in
//! TM-033, agents and effective rows in TA-042 — share identical field names
//! and serde wire shapes instead of each inventing its own.
//!
//! [`deployment_matrix`] is the one query this module owns: the item × agent ×
//! scope cell lookup the matrix view reads. Domain list queries do **not**
//! live here; the module is only the row surface they return.
//!
//! Rows serialize with `serde` to stable snake_case JSON; the snapshot test in
//! `tests/shared_rows.rs` fails if a field is renamed.

use std::collections::BTreeMap;
use std::str::FromStr;

use at_model::{AgentId, ItemId, ItemKind, ItemState, LinkMode, Scope, SyncLabel};
use rusqlite::{Connection, Row, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::DbError;
use crate::ops::{FileAction, OperationKind, OperationStatus};

/// One `items` row as the list screens read it.
///
/// Constructed from a DB row by domain repositories; no `rusqlite` types leak
/// into the public shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRow {
    /// Opaque stable identity (`itm_` + UUIDv7).
    pub id: ItemId,
    /// What the item is.
    pub kind: ItemKind,
    /// Scope the row lives in.
    pub scope: Scope,
    /// Owning project for `project`/`local` rows; `None` elsewhere.
    pub project_id: Option<String>,
    /// URL-safe slug within `(kind, scope, project)`.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Canonical path inside the AgenTuna store.
    pub source_path: Option<String>,
    /// Symlink target when `source_path` is a link.
    pub link_target: Option<String>,
    /// SHA-256 of the canonical content.
    pub content_hash: Option<String>,
    /// Row state text the store scan wrote (`active`, `missing`, …).
    pub state: Option<String>,
    /// Whether AgenTuna owns the backing file.
    pub managed: bool,
    /// Source row id, when the item came from a git/library/folder source.
    pub source_id: Option<String>,
    /// Default enablement an agent sees before per-agent overrides.
    pub enabled_default: Option<bool>,
    /// Free-form tags as a JSON array.
    pub tags_json: Option<String>,
    /// Row creation timestamp (RFC 3339 text).
    pub created_at: String,
    /// Row update timestamp (RFC 3339 text).
    pub updated_at: String,
    /// Last scan that saw the backing file (RFC 3339 text).
    pub last_scanned_at: Option<String>,
}

/// One `projects` row as pickers and project chips read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    /// Project id.
    pub id: String,
    /// Display name; `None` falls back to the path in the UI.
    pub name: Option<String>,
    /// Canonical filesystem path.
    pub path: String,
    /// Discovery root the project was found under, when recorded.
    pub source_root: Option<String>,
    /// Hash of the config snapshot last seen.
    pub config_hash: Option<String>,
    /// When the project was last opened (RFC 3339 text).
    pub last_opened_at: Option<String>,
    /// When the project was last selected; mirrored from `last_used_at`.
    pub last_used_at: Option<String>,
    /// Hidden from pickers without deleting anything.
    pub hidden: bool,
    /// Whether this row is the machine-wide Global pseudo-project.
    pub is_global: bool,
    /// Row creation timestamp (RFC 3339 text).
    pub created_at: String,
}

/// One `agents` row as agent pickers and health lines read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    /// Agent id (`claude_code`, `codex`, `omp`, `cursor`, `custom:<name>`).
    pub id: AgentId,
    /// Human-facing name; `None` means the UI falls back to `id`.
    pub display_name: Option<String>,
    /// Adapter kind tag, when detection recorded one.
    pub kind: Option<String>,
    /// Detected version string.
    pub version: Option<String>,
    /// Whether the agent binary was found on this machine.
    pub installed: bool,
    /// Whether the user lets AgenTuna manage deployments to it.
    pub managed: bool,
    /// When detection last ran (RFC 3339 text).
    pub detected_at: Option<String>,
    /// Per-agent settings blob (JSON text), when detection stored one.
    pub settings_json: Option<String>,
}

/// One `deployments` row as matrix cells and health summaries read it.
///
/// The schema stores `mode` as `symlink`/`junction`/`copy`/`native`/`none`
/// (TF-013); [`DeploymentCell::mode`] keeps the stored string verbatim because
/// `junction` and `none` have no `at_model::LinkMode` counterpart. Rows whose
/// mode is `symlink` or `copy` also expose [`DeploymentCell::link_mode`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCell {
    /// Item the cell belongs to.
    pub item_id: ItemId,
    /// Agent the cell belongs to.
    pub agent_id: AgentId,
    /// Scope the cell belongs to.
    pub scope: Scope,
    /// Owning project; `None` for global/user rows.
    pub project_id: Option<String>,
    /// Deployment state.
    pub state: ItemState,
    /// Stored mode string (`symlink`, `junction`, `copy`, `native`, `none`).
    pub mode: Option<String>,
    /// `link`/`copy` view of `mode` when it maps to a materialisation mode.
    pub link_mode: Option<LinkMode>,
    /// Path inside the agent folder the deployment targets.
    pub target_path: Option<String>,
    /// Where the link points, for link deployments.
    pub link_target: Option<String>,
    /// Hash of the canonical store content.
    pub store_hash: Option<String>,
    /// Hash found at the target when last verified.
    pub target_hash: Option<String>,
    /// Hash recorded when AgenTuna last wrote or adopted the target.
    pub baseline_hash: Option<String>,
    /// Library-versus-project relation.
    pub sync_label: Option<SyncLabel>,
    /// When the row last changed (RFC 3339 text).
    pub updated_at: String,
    /// When the deployment was last verified (RFC 3339 text).
    pub last_verified_at: Option<String>,
    /// Operation that produced the current state.
    pub operation_id: Option<String>,
}

/// One row of the deployment matrix: an item, one agent, one scope cell.
///
/// `cell` is `None` when no `deployments` row exists for the pair — the matrix
/// still renders the cell (as "not deployed") so every item × agent × scope
/// combination yields exactly one [`DeploymentMatrixRow`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentMatrixRow {
    /// The item.
    pub item: ItemRow,
    /// The agent the cell is for.
    pub agent: AgentRef,
    /// Scope of the cell.
    pub scope: Scope,
    /// Project the cell belongs to; `None` for global/user scopes.
    pub project_id: Option<String>,
    /// The deployment cell, when a row exists.
    pub cell: Option<DeploymentCell>,
}

/// One `backups` row as the backup list and restore flow read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRow {
    /// Backup id.
    pub id: String,
    /// Operation that created the backup, when recorded.
    pub operation_id: Option<String>,
    /// Path the backup restores to.
    pub original_path: Option<String>,
    /// Where the backup file lives.
    pub backup_path: Option<String>,
    /// SHA-256 of the backed-up content.
    pub content_hash: Option<String>,
    /// Size in bytes.
    pub size: Option<i64>,
    /// When the backup was taken (RFC 3339 text).
    pub created_at: String,
    /// Whether retention must never prune it.
    pub pinned: bool,
    /// When the backup becomes eligible for pruning (RFC 3339 text).
    pub expires_at: Option<String>,
    /// How many times it has been restored.
    pub restore_count: i64,
}

/// One `operations` row as the history list and audit views read it.
///
/// Mirrors [`crate::ops::OperationRow`] but serializes for the UI/CLI wire; the
/// two stay in sync by construction (same column list, same enum parsing).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRow {
    /// Operation id as stored (ULID or caller-supplied text).
    pub id: String,
    /// What the operation did.
    pub kind: Option<OperationKind>,
    /// Scope it ran at.
    pub scope: Option<Scope>,
    /// Which part of AgenTuna started it.
    pub actor: Option<crate::ops::OperationActor>,
    /// Current status.
    pub status: Option<OperationStatus>,
    /// One-line description.
    pub summary: Option<String>,
    /// Error classification when it failed.
    pub error_kind: Option<String>,
    /// When it started (RFC 3339 text).
    pub started_at: String,
    /// When the row was written (RFC 3339 text).
    pub created_at: String,
    /// When it finished (RFC 3339 text).
    pub finished_at: Option<String>,
    /// Groups related operations (for example a migration and its children).
    pub correlation_id: Option<String>,
    /// The operation this one undid.
    pub undo_of: Option<String>,
}

/// One `operation_files` row as undo and file lists read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationFileRow {
    /// Row id.
    pub id: i64,
    /// Owning operation.
    pub operation_id: String,
    /// Path the operation touched.
    pub path: Option<String>,
    /// What happened to it.
    pub action: Option<FileAction>,
    /// Content hash before the operation.
    pub before_hash: Option<String>,
    /// Content hash after the operation.
    pub after_hash: Option<String>,
    /// Backup taken for the change, when one exists.
    pub backup_id: Option<String>,
    /// Size before the operation.
    pub bytes_before: Option<i64>,
    /// Size after the operation.
    pub bytes_after: Option<i64>,
}

/// One `locks` row as lock chips and the "why is this on" trace read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRow {
    /// Lock row id.
    pub id: i64,
    /// Locked item.
    pub item_id: ItemId,
    /// Scope whose definition is pinned.
    pub scope: Scope,
    /// Human-readable reason.
    pub reason: Option<String>,
    /// Who placed the lock.
    pub locked_by: Option<String>,
    /// When the lock was placed (RFC 3339 text).
    pub locked_at: String,
}

/// One `native_items` row as the "not in AgenTuna" list reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeItemRow {
    /// Native row id.
    pub id: String,
    /// Agent whose folder holds it.
    pub agent_id: AgentId,
    /// Scope of the agent folder.
    pub scope: Scope,
    /// What the item is.
    pub kind: ItemKind,
    /// Display name.
    pub name: String,
    /// Filesystem path; the natural key of the row.
    pub path: String,
    /// Content hash when last scanned.
    pub content_hash: Option<String>,
    /// First scan that saw it (RFC 3339 text).
    pub first_seen_at: String,
    /// Most recent scan that saw it (RFC 3339 text).
    pub last_seen_at: String,
    /// Canonical item it was adopted into, when adopted.
    pub adopted_item_id: Option<ItemId>,
    /// Scan status (`new`, `adopted`, `ignored`, `missing`).
    pub status: String,
}

/// One `drift_events` row as the drift list reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftEventRow {
    /// Drift row id.
    pub id: i64,
    /// Item the drift belongs to, when known.
    pub item_id: Option<ItemId>,
    /// Agent whose target drifted.
    pub agent_id: AgentId,
    /// Scope of the drifted deployment.
    pub scope: Scope,
    /// Path that drifted.
    pub path: String,
    /// Hash the deployment expected.
    pub expected_hash: Option<String>,
    /// Hash actually found.
    pub actual_hash: Option<String>,
    /// When drift was detected (RFC 3339 text).
    pub detected_at: String,
    /// When it was resolved (RFC 3339 text).
    pub resolved_at: Option<String>,
    /// Resolution taken (`adopt`, `restore`, `ignore`).
    pub resolution: Option<String>,
    /// Backup taken during resolution.
    pub backup_id: Option<String>,
}

/// The deployment-matrix cell for one `(item, agent, scope, project)` key.
///
/// Returned by [`deployment_matrix`]; the key is what callers group by, the
/// value is everything the cell renders. `project_key` is the empty string for
/// global/user rows (the same sentinel the `idx_deployments_matrix` index
/// uses) so callers never have to special-case `NULL`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixCell {
    /// Item id.
    pub item_id: ItemId,
    /// Agent id.
    pub agent_id: AgentId,
    /// Scope.
    pub scope: Scope,
    /// Project id, or the empty sentinel for non-project scopes.
    pub project_key: String,
    /// Deployment state; `None` when no row exists yet.
    pub state: Option<ItemState>,
    /// Stored mode string.
    pub mode: Option<String>,
    /// Store content hash.
    pub store_hash: Option<String>,
    /// Target content hash.
    pub target_hash: Option<String>,
    /// Baseline content hash.
    pub baseline_hash: Option<String>,
    /// Sync label.
    pub sync_label: Option<SyncLabel>,
    /// Target path inside the agent folder.
    pub target_path: Option<String>,
    /// Link target for link deployments.
    pub link_target: Option<String>,
    /// When the row last changed.
    pub updated_at: Option<String>,
    /// When the deployment was last verified.
    pub last_verified_at: Option<String>,
    /// Operation that produced the current state.
    pub operation_id: Option<String>,
}

/// Result of [`deployment_matrix`]: the cells plus the items and agents they
/// cover, so callers can render headers without a second query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentMatrix {
    /// Every cell, keyed by `(item_id, agent_id, scope, project_key)`.
    pub cells: Vec<MatrixCell>,
    /// The items the matrix covers, in the order they were requested.
    pub items: Vec<ItemRow>,
    /// The agents the matrix covers, in the order they were requested.
    pub agents: Vec<AgentRef>,
}

/// Matrix lookup for a set of items across agents and scopes.
///
/// One prepared statement joins `deployments` to `items` and `agents`, so a
/// 3k-item × 3-agent × 3-scope matrix is a single query — never one query per
/// agent or per item. `item_ids`, `agent_ids` and `scopes` may be empty (an
/// empty filter matches nothing); `project_id` of `None` selects the
/// global/user cells (the `COALESCE(project_id, '')` sentinel).
///
/// The returned [`DeploymentMatrix`] contains one [`MatrixCell`] per requested
/// pair that has a `deployments` row; pairs without a row are absent (callers
/// render them as "not deployed"). `items` and `agents` are resolved in two
/// additional prepared statements — the matrix itself stays at one query.
///
/// # Errors
///
/// [`DbError`] when a statement fails or a stored value is not a valid enum.
pub fn deployment_matrix(
    connection: &Connection,
    item_ids: &[ItemId],
    agent_ids: &[AgentId],
    scopes: &[Scope],
    project_id: Option<&str>,
) -> Result<DeploymentMatrix, DbError> {
    let cells = matrix_cells(connection, item_ids, agent_ids, scopes, project_id)?;
    let items = items_by_ids(connection, item_ids)?;
    let agents = agents_by_ids(connection, agent_ids)?;
    Ok(DeploymentMatrix {
        cells,
        items,
        agents,
    })
}

/// The cells of [`deployment_matrix`] without the item/agent lookups.
///
/// One prepared statement, no joins: the matrix is a flat lookup over
/// `deployments` keyed by `(item_id, agent_id, scope, project_id)`. The
/// statement-count hook in `tests/deployment_matrix.rs` asserts exactly one
/// query per call and a reused prepared statement across calls.
pub fn matrix_cells(
    connection: &Connection,
    item_ids: &[ItemId],
    agent_ids: &[AgentId],
    scopes: &[Scope],
    project_id: Option<&str>,
) -> Result<Vec<MatrixCell>, DbError> {
    if item_ids.is_empty() || agent_ids.is_empty() || scopes.is_empty() {
        return Ok(Vec::new());
    }

    let item_params: Vec<String> = item_ids.iter().map(ToString::to_string).collect();
    let agent_params: Vec<String> = agent_ids.iter().map(ToString::to_string).collect();
    let scope_params: Vec<&str> = scopes.iter().map(|s| s.wire_name()).collect();

    // `project_id` filters deployments by their stored project. `None` selects
    // only rows with `project_id IS NULL` (global/user cells); `Some(id)`
    // selects that project's rows. The empty sentinel is never stored.
    let project_key_owned;
    let project_filter = match project_id {
        Some(_) => "AND d.project_id = ?",
        None => "AND d.project_id IS NULL",
    };

    let sql = format!(
        "SELECT d.item_id, d.agent_id, d.scope, COALESCE(d.project_id, ''), \
                d.state, d.mode, d.store_hash, d.target_hash, d.baseline_hash, \
                d.sync_label, d.target_path, d.link_target, d.updated_at, \
                d.last_verified_at, d.operation_id \
           FROM deployments d \
          WHERE d.item_id IN ({}) \
            AND d.agent_id IN ({}) \
            AND d.scope IN ({}) \
            {}",
        placeholders(item_ids.len()),
        placeholders(agent_ids.len()),
        placeholders(scopes.len()),
        project_filter,
    );

    let mut statement = connection.prepare(&sql)?;
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = item_params
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .chain(agent_params.iter().map(|s| s as &dyn rusqlite::ToSql))
        .chain(scope_params.iter().map(|s| s as &dyn rusqlite::ToSql))
        .collect();
    if let Some(pid) = project_id {
        project_key_owned = pid.to_string();
        params_vec.push(&project_key_owned as &dyn rusqlite::ToSql);
    }

    let cells = statement
        .query_map(params_from_iter(params_vec), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(cells.len());
    for (
        item_id,
        agent_id,
        scope,
        project_key,
        state,
        mode,
        store_hash,
        target_hash,
        baseline_hash,
        sync_label,
        target_path,
        link_target,
        updated_at,
        last_verified_at,
        operation_id,
    ) in cells
    {
        out.push(MatrixCell {
            item_id: parse_item_id(&item_id)?,
            agent_id: parse_agent_id(&agent_id)?,
            scope: parse_scope(&scope)?,
            project_key,
            state: Some(parse_item_state(&state)?),
            mode,
            store_hash,
            target_hash,
            baseline_hash,
            sync_label: parse_optional(sync_label.as_deref())?,
            target_path,
            link_target,
            updated_at: Some(updated_at),
            last_verified_at,
            operation_id,
        });
    }
    Ok(out)
}

/// Every `items` row named by `ids`, in the order the ids were given.
///
/// One prepared statement with an `IN` list; callers that need a different
/// projection should not reuse this — it is the row surface, not a query API.
pub fn items_by_ids(connection: &Connection, ids: &[ItemId]) -> Result<Vec<ItemRow>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let params: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let sql = format!(
        "SELECT {} FROM items WHERE id IN ({})",
        ITEM_COLUMNS,
        placeholders(ids.len())
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), item_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    // The IN clause does not preserve request order; sort by the input order.
    let order: BTreeMap<String, usize> = params
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect();
    let mut rows = rows;
    rows.sort_by_key(|row| {
        order
            .get(&row.id.to_string())
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(rows)
}

/// Every `agents` row named by `ids`, in the order the ids were given.
pub fn agents_by_ids(connection: &Connection, ids: &[AgentId]) -> Result<Vec<AgentRef>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let params: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let sql = format!(
        "SELECT {} FROM agents WHERE id IN ({})",
        AGENT_COLUMNS,
        placeholders(ids.len())
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), agent_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    let order: BTreeMap<String, usize> = params
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect();
    let mut rows = rows;
    rows.sort_by_key(|row| {
        order
            .get(&row.id.to_string())
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(rows)
}

/// Column list every `items` read selects, in [`item_from_row`] order.
const ITEM_COLUMNS: &str = "id, kind, scope, project_id, slug, name, source_path, \
     link_target, content_hash, state, managed, source_id, enabled_default, \
     tags_json, created_at, updated_at, last_scanned_at";

/// Column list every `agents` read selects, in [`agent_from_row`] order.
const AGENT_COLUMNS: &str = "id, display_name, kind, version, installed, managed, \
     detected_at, settings_json";

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

/// Parse an `ItemId` from its stored text form.
fn parse_item_id(value: &str) -> Result<ItemId, DbError> {
    value.parse::<ItemId>().map_err(|e| DbError::InvalidValue {
        field: "item_id",
        reason: e.to_string(),
    })
}

/// Parse an `AgentId` from its stored text form.
fn parse_agent_id(value: &str) -> Result<AgentId, DbError> {
    value.parse::<AgentId>().map_err(|e| DbError::InvalidValue {
        field: "agent_id",
        reason: e.to_string(),
    })
}

/// Parse a `Scope` from its stored text form.
fn parse_scope(value: &str) -> Result<Scope, DbError> {
    value.parse::<Scope>().map_err(|e| DbError::InvalidValue {
        field: "scope",
        reason: e.to_string(),
    })
}

/// Parse an `ItemState` from its stored text form.
fn parse_item_state(value: &str) -> Result<ItemState, DbError> {
    value
        .parse::<ItemState>()
        .map_err(|e| DbError::InvalidValue {
            field: "state",
            reason: e.to_string(),
        })
}

/// Parse an optional stored enum value.
fn parse_optional<T>(value: Option<&str>) -> Result<Option<T>, DbError>
where
    T: FromStr<Err = at_model::ParseWireError>,
{
    value
        .map(|text| text.parse().map_err(DbError::from))
        .transpose()
}

/// Map a stored `mode` to the `at_model::LinkMode` wire values.
///
/// This is the read half of the TA-014 mapping between the two mode vocabularies:
/// `at_model::LinkMode` serialises as `link`/`copy` (the settings and engine wire names),
/// while the `deployments.mode` column and `at_fs::StoredLinkMode` store `symlink`/`junction`/
/// `copy`. `symlink` is the at-fs spelling of `link`; `junction`, `native` and `none` have no
/// `LinkMode` counterpart and stay `None`.
///
/// The write half is the caller's job: when persisting a `LinkMode`, translate it to the
/// stored spelling (`LinkMode::Link` → `StoredLinkMode::Symlink.as_db()` = `"symlink"`,
/// `LinkMode::Copy` → `StoredLinkMode::Copy.as_db()` = `"copy"`) before calling
/// [`crate::deployments::upsert_deployment`]. `junction` is never produced from a `LinkMode`;
/// it is a Windows fallback recorded by at-fs after the fact.
fn link_mode_of(mode: &str) -> Option<LinkMode> {
    match mode {
        "symlink" => Some(LinkMode::Link),
        "copy" => Some(LinkMode::Copy),
        _ => None,
    }
}

/// One `items` row from a result that starts at column `offset`.
///
/// Used when a query joins `deployments` and `items`; the deployment columns
/// occupy `0..=15`, so the item columns start at `ITEM_COLUMN_OFFSET`.
pub(crate) fn item_from_row_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: row.get::<_, String>(offset)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        kind: row.get::<_, String>(offset + 1)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 1,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        scope: row.get::<_, String>(offset + 2)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 2,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        project_id: row.get(offset + 3)?,
        slug: row.get(offset + 4)?,
        name: row.get(offset + 5)?,
        source_path: row.get(offset + 6)?,
        link_target: row.get(offset + 7)?,
        content_hash: row.get(offset + 8)?,
        state: row.get(offset + 9)?,
        managed: row.get::<_, i64>(offset + 10)? != 0,
        source_id: row.get(offset + 11)?,
        enabled_default: row.get::<_, Option<i64>>(offset + 12)?.map(|v| v != 0),
        tags_json: row.get(offset + 13)?,
        created_at: row.get(offset + 14)?,
        updated_at: row.get(offset + 15)?,
        last_scanned_at: row.get(offset + 16)?,
    })
}

/// Map one `items` row.
pub(crate) fn item_from_row(row: &Row<'_>) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: row.get::<_, String>(0)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        kind: parse_kind(&row.get::<_, String>(1)?)?,
        scope: parse_scope_text(&row.get::<_, String>(2)?)?,
        project_id: row.get(3)?,
        slug: row.get(4)?,
        name: row.get(5)?,
        source_path: row.get(6)?,
        link_target: row.get(7)?,
        content_hash: row.get(8)?,
        state: row.get(9)?,
        managed: row.get::<_, i64>(10)? != 0,
        source_id: row.get(11)?,
        enabled_default: row.get::<_, Option<i64>>(12)?.map(|v| v != 0),
        tags_json: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        last_scanned_at: row.get(16)?,
    })
}

/// Map one `agents` row.
pub(crate) fn agent_from_row(row: &Row<'_>) -> rusqlite::Result<AgentRef> {
    Ok(AgentRef {
        id: row.get::<_, String>(0)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        display_name: row.get(1)?,
        kind: row.get(2)?,
        version: row.get(3)?,
        installed: row.get::<_, i64>(4)? != 0,
        managed: row.get::<_, i64>(5)? != 0,
        detected_at: row.get(6)?,
        settings_json: row.get(7)?,
    })
}

/// Parse an `ItemKind` from its stored text form, for `FromSql` contexts.
fn parse_kind(value: &str) -> rusqlite::Result<ItemKind> {
    value.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Parse a `Scope` from its stored text form, for `FromSql` contexts.
fn parse_scope_text(value: &str) -> rusqlite::Result<Scope> {
    value.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// One `deployments` row as [`DeploymentCell`].
///
/// Used by [`matrix_cells`]; kept crate-private because the column list is an
/// implementation detail of that query.
pub(crate) fn deployment_cell_from_row(row: &Row<'_>) -> rusqlite::Result<DeploymentCell> {
    deployment_cell_from_row_at(row, 0)
}

/// One `deployments` row from a result that starts at column `offset`.
///
/// The deployments column list starts with `id` at `offset`, so the item,
/// agent, scope and project columns sit at `offset + 1..=offset + 4`.
pub(crate) fn deployment_cell_from_row_at(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<DeploymentCell> {
    Ok(DeploymentCell {
        item_id: row.get::<_, String>(offset + 1)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 1,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        agent_id: row.get::<_, String>(offset + 2)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 2,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        scope: row.get::<_, String>(offset + 3)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 3,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        project_id: row.get(offset + 4)?,
        state: row.get::<_, String>(offset + 5)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 5,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        mode: row.get(offset + 6)?,
        link_mode: row
            .get::<_, Option<String>>(offset + 6)?
            .and_then(|s| link_mode_of(&s)),
        store_hash: row.get(offset + 9)?,
        target_hash: row.get(offset + 10)?,
        baseline_hash: row.get(offset + 11)?,
        sync_label: row
            .get::<_, Option<String>>(offset + 12)?
            .map(|s| {
                s.parse::<SyncLabel>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        offset + 12,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        target_path: row.get(offset + 7)?,
        link_target: row.get(offset + 8)?,
        updated_at: row.get(offset + 13)?,
        last_verified_at: row.get(offset + 14)?,
        operation_id: row.get(offset + 15)?,
    })
}

/// One `backups` row as [`BackupRow`].
#[allow(dead_code)]
pub(crate) fn backup_from_row(row: &Row<'_>) -> rusqlite::Result<BackupRow> {
    Ok(BackupRow {
        id: row.get(0)?,
        operation_id: row.get(1)?,
        original_path: row.get(2)?,
        backup_path: row.get(3)?,
        content_hash: row.get(4)?,
        size: row.get(5)?,
        created_at: row.get(6)?,
        pinned: row.get::<_, i64>(7)? != 0,
        expires_at: row.get(8)?,
        restore_count: row.get(9)?,
    })
}

/// One `operations` row as [`OperationRow`].
pub(crate) fn operation_from_row(row: &Row<'_>) -> rusqlite::Result<OperationRow> {
    // Columns: id, kind, scope, actor, status, summary, error_kind,
    //          started_at, created_at, finished_at, correlation_id, undo_of
    Ok(OperationRow {
        id: row.get(0)?,
        kind: row
            .get::<_, Option<String>>(1)?
            .map(|s| {
                s.parse::<OperationKind>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        scope: row
            .get::<_, Option<String>>(2)?
            .map(|s| {
                s.parse::<Scope>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        actor: row
            .get::<_, Option<String>>(3)?
            .map(|s| {
                s.parse::<crate::ops::OperationActor>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        status: row
            .get::<_, Option<String>>(4)?
            .map(|s| {
                s.parse::<OperationStatus>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        summary: row.get(5)?,
        error_kind: row.get(6)?,
        started_at: row.get(7)?,
        created_at: row.get::<_, String>(8)?,
        finished_at: row.get(9)?,
        correlation_id: row.get(10)?,
        undo_of: row.get(11)?,
    })
}

/// One `operation_files` row as [`OperationFileRow`].
#[allow(dead_code)]
pub(crate) fn operation_file_from_row(row: &Row<'_>) -> rusqlite::Result<OperationFileRow> {
    Ok(OperationFileRow {
        id: row.get(0)?,
        operation_id: row.get(1)?,
        path: row.get(2)?,
        action: row
            .get::<_, Option<String>>(3)?
            .map(|s| {
                s.parse::<FileAction>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        before_hash: row.get(4)?,
        after_hash: row.get(5)?,
        backup_id: row.get(6)?,
        bytes_before: row.get(7)?,
        bytes_after: row.get(8)?,
    })
}

/// One `locks` row as [`LockRow`].
pub(crate) fn lock_from_row(row: &Row<'_>) -> rusqlite::Result<LockRow> {
    Ok(LockRow {
        id: row.get(0)?,
        item_id: row.get::<_, String>(1)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        scope: row.get::<_, String>(2)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        reason: row.get(3)?,
        locked_by: row.get(4)?,
        locked_at: row.get(5)?,
    })
}

/// One `native_items` row as [`NativeItemRow`].
#[allow(dead_code)]
pub(crate) fn native_item_from_row(row: &Row<'_>) -> rusqlite::Result<NativeItemRow> {
    Ok(NativeItemRow {
        id: row.get(0)?,
        agent_id: row.get::<_, String>(1)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        scope: row.get::<_, String>(2)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        kind: row.get::<_, String>(3)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        name: row.get(4)?,
        path: row.get(5)?,
        content_hash: row.get(6)?,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        adopted_item_id: row
            .get::<_, Option<String>>(9)?
            .map(|s| {
                s.parse::<ItemId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        status: row.get(10)?,
    })
}

/// One `drift_events` row as [`DriftEventRow`].
#[allow(dead_code)]
pub(crate) fn drift_event_from_row(row: &Row<'_>) -> rusqlite::Result<DriftEventRow> {
    Ok(DriftEventRow {
        id: row.get(0)?,
        item_id: row
            .get::<_, Option<String>>(1)?
            .map(|s| {
                s.parse::<ItemId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        agent_id: row.get::<_, String>(2)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        scope: row.get::<_, String>(3)?.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        path: row.get(4)?,
        expected_hash: row.get(5)?,
        actual_hash: row.get(6)?,
        detected_at: row.get(7)?,
        resolved_at: row.get(8)?,
        resolution: row.get(9)?,
        backup_id: row.get(10)?,
    })
}
