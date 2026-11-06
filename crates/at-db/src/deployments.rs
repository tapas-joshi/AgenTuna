//! Deployments repository and queries over the `deployments` table (TA-041).
//!
//! The `deployments` table is owned by TF-013 (migration v4); this module adds
//! the repository and query layer on top — no DDL. It provides:
//!
//! - [`upsert_deployment`] — insert or update the cell for
//!   `(item_id, agent_id, scope, project_id)` in place.
//! - [`effective_rows`] — the per-agent effective-config rows the TA-009
//!   overview and the effective screen consume.
//! - [`health_counts`] — managed / in-sync / total for one agent.
//! - [`find_deployment`], [`list_deployments_for_item`],
//!   [`list_deployments_for_agent`] — single-item and per-agent reads.
//!
//! All reads go through one prepared statement; none hold a write
//! transaction, so the UI can render while a scan runs elsewhere.

use at_model::{AgentId, ItemId, ItemState, Scope, SyncLabel};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::DbError;
use crate::shared::DeploymentCell;

/// One `deployments` row plus the item it deploys, for effective-config views.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveDeploymentRow {
    /// The deployment cell.
    pub cell: DeploymentCell,
    /// The item being deployed.
    pub item: crate::shared::ItemRow,
    /// Agent display name, when the agent row exists.
    pub agent_display_name: Option<String>,
}

/// Per-agent health line: how many deployments are managed, in sync and total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCounts {
    /// Deployments whose state is not `native` (AgenTuna manages them).
    pub managed: i64,
    /// Managed deployments whose sync label is `in_sync`.
    pub in_sync: i64,
    /// Every deployment row for the agent.
    pub total: i64,
}

/// Everything [`upsert_deployment`] needs to write one cell.
#[derive(Clone, Debug)]
pub struct DeploymentSpec<'a> {
    /// Item being deployed.
    pub item_id: &'a ItemId,
    /// Agent receiving the deployment.
    pub agent_id: &'a AgentId,
    /// Scope of the deployment.
    pub scope: Scope,
    /// Owning project; `None` for global/user.
    pub project_id: Option<&'a str>,
    /// Deployment state.
    pub state: ItemState,
    /// Stored mode (`symlink`, `junction`, `copy`, `native`, `none`).
    pub mode: Option<&'a str>,
    /// Path inside the agent folder.
    pub target_path: Option<&'a str>,
    /// Link target for link deployments.
    pub link_target: Option<&'a str>,
    /// Hash of the canonical store content.
    pub store_hash: Option<&'a str>,
    /// Hash found at the target.
    pub target_hash: Option<&'a str>,
    /// Baseline hash recorded at write/adopt time.
    pub baseline_hash: Option<&'a str>,
    /// Sync label.
    pub sync_label: Option<SyncLabel>,
    /// Operation that produced this state.
    pub operation_id: Option<&'a str>,
}

impl<'a> DeploymentSpec<'a> {
    /// A minimal spec: item, agent, scope, state; everything else `None`.
    #[must_use]
    pub fn new(item_id: &'a ItemId, agent_id: &'a AgentId, scope: Scope, state: ItemState) -> Self {
        Self {
            item_id,
            agent_id,
            scope,
            project_id: None,
            state,
            mode: None,
            target_path: None,
            link_target: None,
            store_hash: None,
            target_hash: None,
            baseline_hash: None,
            sync_label: None,
            operation_id: None,
        }
    }

    /// Set the stored mode.
    #[must_use]
    pub fn with_mode(mut self, mode: &'a str) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Set the target path inside the agent folder.
    #[must_use]
    pub fn with_target_path(mut self, path: &'a str) -> Self {
        self.target_path = Some(path);
        self
    }

    /// Set the link target for link deployments.
    #[must_use]
    pub fn with_link_target(mut self, target: &'a str) -> Self {
        self.link_target = Some(target);
        self
    }

    /// Set all three hashes at once.
    #[must_use]
    pub fn with_hashes(
        mut self,
        store: Option<&'a str>,
        target: Option<&'a str>,
        baseline: Option<&'a str>,
    ) -> Self {
        self.store_hash = store;
        self.target_hash = target;
        self.baseline_hash = baseline;
        self
    }

    /// Set the sync label.
    #[must_use]
    pub fn with_sync_label(mut self, label: SyncLabel) -> Self {
        self.sync_label = Some(label);
        self
    }

    /// Set the owning project.
    #[must_use]
    pub fn with_project(mut self, project_id: &'a str) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Set the operation that produced this state.
    #[must_use]
    pub fn with_operation(mut self, operation_id: &'a str) -> Self {
        self.operation_id = Some(operation_id);
        self
    }
}

/// Insert or update the deployment cell for
/// `(item_id, agent_id, scope, COALESCE(project_id, ''))`.
///
/// The unique index `idx_deployments_matrix` makes the pair a single row:
/// calling this twice for the same key updates `state`, `mode`, the hashes and
/// `updated_at` in place instead of inserting a second row. `updated_at` is set
/// to the current UTC timestamp on every call.
///
/// [`DbError::Constraint`] when `item_id` does not exist in `items`, a SQLite failure, or
/// [`DbError::InvalidValue`] when the label is query-derived and excluded by schema v4.
pub fn upsert_deployment(
    connection: &Connection,
    spec: &DeploymentSpec<'_>,
) -> Result<(), DbError> {
    if matches!(
        spec.sync_label,
        Some(SyncLabel::LibraryOnly | SyncLabel::Conflict | SyncLabel::Drifted)
    ) {
        return Err(DbError::InvalidValue {
            field: "sync_label",
            reason: "library_only, conflict, and drifted are query-derived and cannot be persisted"
                .to_owned(),
        });
    }
    connection.execute(
        "INSERT INTO deployments (item_id, agent_id, scope, project_id, state, mode, \
             target_path, link_target, store_hash, target_hash, baseline_hash, \
             sync_label, operation_id, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT (item_id, agent_id, scope, COALESCE(project_id, '')) \
         DO UPDATE SET state = excluded.state, \
                       mode = excluded.mode, \
                       target_path = excluded.target_path, \
                       link_target = excluded.link_target, \
                       store_hash = excluded.store_hash, \
                       target_hash = excluded.target_hash, \
                       baseline_hash = excluded.baseline_hash, \
                       sync_label = excluded.sync_label, \
                       operation_id = excluded.operation_id, \
                       updated_at = excluded.updated_at",
        params![
            spec.item_id.to_string(),
            spec.agent_id.to_string(),
            spec.scope.wire_name(),
            spec.project_id,
            spec.state.wire_name(),
            spec.mode,
            spec.target_path,
            spec.link_target,
            spec.store_hash,
            spec.target_hash,
            spec.baseline_hash,
            spec.sync_label.map(SyncLabel::wire_name),
            spec.operation_id,
        ],
    )?;
    Ok(())
}

/// Updates the recorded common Project/User content baseline after a reviewed sync applies.
///
/// This updates existing Project deployment rows only; it never creates missing rows or
/// replaces deployment state. A missing row is reported as zero updated rows.
///
/// # Errors
/// Returns [`DbError::InvalidValue`] for a malformed hash and [`DbError`] for a SQLite error.
pub fn update_project_sync_baseline(
    connection: &Connection,
    item_id: &ItemId,
    project_id: &str,
    baseline_hash: &str,
) -> Result<usize, DbError> {
    if baseline_hash.len() != 64
        || !baseline_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DbError::InvalidValue {
            field: "baseline_hash",
            reason: "expected a lowercase SHA-256 digest".to_owned(),
        });
    }
    let changed = connection.execute(
        "UPDATE deployments SET baseline_hash = ?1, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE item_id = ?2 AND scope = 'project' AND project_id = ?3",
        params![baseline_hash, item_id.to_string(), project_id],
    )?;
    Ok(changed)
}

/// One deployment cell for one `(item, agent, scope, project)` key.
///
/// Returns `None` when no row exists.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn find_deployment(
    connection: &Connection,
    item_id: &ItemId,
    agent_id: &AgentId,
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Option<DeploymentCell>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM deployments \
          WHERE item_id = ?1 AND agent_id = ?2 AND scope = ?3 \
            AND COALESCE(project_id, '') = COALESCE(?4, '')",
        DEPLOYMENT_COLUMNS
    ))?;
    let row = statement.query_row(
        params![
            item_id.to_string(),
            agent_id.to_string(),
            scope.wire_name(),
            project_id,
        ],
        crate::shared::deployment_cell_from_row,
    );
    match row {
        Ok(cell) => Ok(Some(cell)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(DbError::from(e)),
    }
}

/// Every deployment row for one item, across agents and scopes.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn list_deployments_for_item(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Vec<DeploymentCell>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM deployments WHERE item_id = ?1 ORDER BY agent_id, scope",
        DEPLOYMENT_COLUMNS
    ))?;
    let rows = statement
        .query_map(
            params![item_id.to_string()],
            crate::shared::deployment_cell_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every deployment row for one agent, across items and scopes.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn list_deployments_for_agent(
    connection: &Connection,
    agent_id: &AgentId,
) -> Result<Vec<DeploymentCell>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM deployments WHERE agent_id = ?1 ORDER BY item_id, scope",
        DEPLOYMENT_COLUMNS
    ))?;
    let rows = statement
        .query_map(
            params![agent_id.to_string()],
            crate::shared::deployment_cell_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The effective-config rows for one agent at one scope (and optionally one
/// project).
///
/// One prepared statement joins `deployments` to `items` and `agents`, so the
/// effective screen never issues a per-row query. `scope` filters the
/// deployment's scope; `project_id` of `None` selects the global/user rows
/// (the `COALESCE(project_id, '')` sentinel matches the index).
///
/// Rows are ordered by item name so the UI can render them directly.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn effective_rows(
    connection: &Connection,
    agent_id: &AgentId,
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Vec<EffectiveDeploymentRow>, DbError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {}, {}, a.display_name \
           FROM deployments d \
           JOIN items i ON i.id = d.item_id \
           LEFT JOIN agents a ON a.id = d.agent_id \
          WHERE d.agent_id = ?1 AND d.scope = ?2 \
            AND COALESCE(d.project_id, '') = COALESCE(?3, '') \
          ORDER BY i.name COLLATE NOCASE, i.id",
        DEPLOYMENT_COLUMNS_PREFIXED, ITEM_COLUMNS_PREFIXED
    ))?;
    let rows = statement
        .query_map(
            params![agent_id.to_string(), scope.wire_name(), project_id],
            |row| {
                let cell = crate::shared::deployment_cell_from_row_at(row, 0)?;
                let item = crate::shared::item_from_row_at(row, ITEM_COLUMN_OFFSET)?;
                let agent_display_name = row.get(ITEM_COLUMN_OFFSET + ITEM_COLUMN_COUNT)?;
                Ok(EffectiveDeploymentRow {
                    cell,
                    item,
                    agent_display_name,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Health line for one agent: managed, in-sync and total deployment counts.
///
/// `managed` counts rows whose state is not `native` (AgenTuna owns the
/// deployment); `in_sync` counts managed rows whose `sync_label` is
/// `in_sync`; `total` counts every row for the agent. Deployments of
/// unmanaged agents are excluded from the health line by the caller passing
/// the agent id — this function never counts other agents.
///
/// An unknown agent returns zero counts rather than an error.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn health_counts(connection: &Connection, agent_id: &AgentId) -> Result<HealthCounts, DbError> {
    let mut statement = connection.prepare(
        "SELECT COUNT(*) AS total, \
                SUM(state != 'native') AS managed, \
                SUM(state = 'linked' AND sync_label = 'in_sync') AS in_sync \
           FROM deployments WHERE agent_id = ?1",
    )?;
    let counts = statement.query_row(params![agent_id.to_string()], |row| {
        Ok(HealthCounts {
            total: row.get(0)?,
            managed: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            in_sync: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
        })
    })?;
    Ok(counts)
}

/// Column list every `deployments` read selects, in
/// [`deployment_cell_from_row`] order.
const DEPLOYMENT_COLUMNS: &str = "id, item_id, agent_id, scope, project_id, state, mode, \
     target_path, link_target, store_hash, target_hash, baseline_hash, sync_label, \
     updated_at, last_verified_at, operation_id";

/// `deployments` columns for the effective-rows join (16 columns, index 0..15).
const DEPLOYMENT_COLUMNS_PREFIXED: &str = "d.id AS id, d.item_id AS item_id, \
     d.agent_id AS agent_id, d.scope AS scope, d.project_id AS project_id, \
     d.state AS state, d.mode AS mode, d.target_path AS target_path, \
     d.link_target AS link_target, d.store_hash AS store_hash, \
     d.target_hash AS target_hash, d.baseline_hash AS baseline_hash, \
     d.sync_label AS sync_label, d.updated_at AS updated_at, \
     d.last_verified_at AS last_verified_at, d.operation_id AS operation_id";

/// `items` columns for the effective-rows join (17 columns, index 16..32).
const ITEM_COLUMNS_PREFIXED: &str = "i.id AS id, i.kind AS kind, i.scope AS scope, \
     i.project_id AS project_id, i.slug AS slug, i.name AS name, \
     i.source_path AS source_path, i.link_target AS link_target, \
     i.content_hash AS content_hash, i.state AS state, i.managed AS managed, \
     i.source_id AS source_id, i.enabled_default AS enabled_default, \
     i.tags_json AS tags_json, i.created_at AS created_at, \
     i.updated_at AS updated_at, i.last_scanned_at AS last_scanned_at";

/// Number of `deployments` columns in the effective-rows result.
const DEPLOYMENT_COLUMN_COUNT: usize = 16;

/// Offset of the `items` columns inside the effective-rows result.
const ITEM_COLUMN_OFFSET: usize = DEPLOYMENT_COLUMN_COUNT;

/// Number of `items` columns in the effective-rows result.
const ITEM_COLUMN_COUNT: usize = 17;
