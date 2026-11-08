//! Deployments repository and queries over the `deployments` table (TA-041).
//!
//! The `deployments` table is owned by TF-013 (migration v4); this module adds
//! the repository and query layer on top — no DDL. It provides:
//!
//! - [`upsert_deployment`] — insert or update the cell for
//!   `(item_id, agent_id, scope, project_id)` in place.
//! - [`health_counts`] — managed / in-sync / total for one agent.
//! - [`find_deployment`], [`list_deployments_for_item`],
//!   [`list_deployments_for_agent`] — single-item and per-agent reads.
//!
//! All reads go through one prepared statement; none hold a write
//! transaction, so the UI can render while a scan runs elsewhere.

use std::collections::HashMap;
use std::path::Path;

use at_model::{AgentId, ItemId, ItemState, Scope, SyncLabel};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::DbError;
use crate::shared::DeploymentCell;

/// Identity of one deployment cell: the item, the agent and the scope.
pub type DeploymentCellKey = (ItemId, AgentId, Scope);

/// Target path of every deployment cell, keyed by [`DeploymentCellKey`].
///
/// [`target_paths_by_cell`] builds this for a whole list projection in one query.
pub type DeploymentTargetPaths = HashMap<DeploymentCellKey, Option<String>>;

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

/// Content hashes recording the evidence for a deployment row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeploymentHashes<'a> {
    /// Content hash of the canonical store copy.
    pub store_hash: Option<&'a str>,
    /// Content hash found at the deployed target.
    pub target_hash: Option<&'a str>,
    /// Baseline hash recorded when AgenTuna last wrote or verified the deployment.
    pub baseline_hash: Option<&'a str>,
}

impl<'a> DeploymentHashes<'a> {
    /// Construct empty hashes (`None`, `None`, `None`).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            store_hash: None,
            target_hash: None,
            baseline_hash: None,
        }
    }

    /// Construct hashes for a freshly deployed target where store, target, and baseline match `hash`.
    #[must_use]
    pub const fn in_sync(hash: &'a str) -> Self {
        Self {
            store_hash: Some(hash),
            target_hash: Some(hash),
            baseline_hash: Some(hash),
        }
    }

    /// Construct hashes from individual components.
    #[must_use]
    pub const fn new(
        store_hash: Option<&'a str>,
        target_hash: Option<&'a str>,
        baseline_hash: Option<&'a str>,
    ) -> Self {
        Self {
            store_hash,
            target_hash,
            baseline_hash,
        }
    }
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
    /// Builds a deployment row from (item, agent, target path, mode, hashes) and
    /// assigns the correct TA-005 label for a freshly applied state.
    #[must_use]
    pub fn applied(
        item_id: &'a ItemId,
        agent_id: &'a AgentId,
        scope: Scope,
        state: ItemState,
        mode: Option<&'a str>,
        target_path: Option<&'a str>,
        hashes: DeploymentHashes<'a>,
    ) -> Self {
        Self {
            item_id,
            agent_id,
            scope,
            project_id: None,
            state,
            mode,
            target_path,
            link_target: None,
            store_hash: hashes.store_hash,
            target_hash: hashes.target_hash,
            baseline_hash: hashes.baseline_hash,
            sync_label: Some(SyncLabel::InSync),
            operation_id: None,
        }
    }

    /// Alias for [`Self::applied`] building a deployment row for a freshly applied state.
    #[must_use]
    pub fn fresh(
        item_id: &'a ItemId,
        agent_id: &'a AgentId,
        scope: Scope,
        state: ItemState,
        mode: Option<&'a str>,
        target_path: Option<&'a str>,
        hashes: DeploymentHashes<'a>,
    ) -> Self {
        Self::applied(item_id, agent_id, scope, state, mode, target_path, hashes)
    }

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

/// Builds a deployment row from (item, agent, target path, mode, hashes) and
/// assigns the correct TA-005 label for a freshly applied state.
#[must_use]
pub fn applied_deployment<'a>(
    item_id: &'a ItemId,
    agent_id: &'a AgentId,
    scope: Scope,
    state: ItemState,
    mode: Option<&'a str>,
    target_path: Option<&'a str>,
    hashes: DeploymentHashes<'a>,
) -> DeploymentSpec<'a> {
    DeploymentSpec::applied(item_id, agent_id, scope, state, mode, target_path, hashes)
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
    let Some(sync_label) = spec.sync_label else {
        return Err(DbError::InvalidValue {
            field: "sync_label",
            reason: "sync_label is required; deployments must not be written with a missing label"
                .to_owned(),
        });
    };
    if matches!(
        sync_label,
        SyncLabel::LibraryOnly | SyncLabel::Conflict | SyncLabel::Drifted
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

/// Updates store, target, and baseline hashes for deployments matching `target_path` and `kind`.
///
/// If `store_hash` is `Some`, `store_hash` is updated; otherwise existing `store_hash` is kept.
/// `target_hash` and `baseline_hash` are set to `target_and_baseline_hash`.
///
/// Returns the number of rows updated.
///
/// # Errors
/// Returns [`DbError`] when the database update fails.
pub fn update_hashes_for_target_path(
    connection: &Connection,
    target_path: &Path,
    scope: Option<Scope>,
    kind: &str,
    store_hash: Option<&str>,
    target_and_baseline_hash: &str,
) -> Result<usize, DbError> {
    let path_text = target_path.to_string_lossy();
    let scope_text = scope.map(Scope::wire_name);
    let updated = connection.execute(
        "UPDATE deployments SET store_hash = COALESCE(?1, store_hash), \
                target_hash = ?2, baseline_hash = ?2 \
          WHERE target_path = ?3 AND (?4 IS NULL OR scope = ?4) \
            AND item_id IN (SELECT id FROM items WHERE kind = ?5)",
        params![
            store_hash,
            target_and_baseline_hash,
            path_text.as_ref(),
            scope_text,
            kind
        ],
    )?;
    Ok(updated)
}

/// Records one canonical store hash on every deployment row in a scope.
///
/// A shared store document (`mcp.json`) has one bytes-level hash for every server it holds.
/// Writing one entry changes the document for all of them, so a per-row refresh of only the
/// written native document leaves the other rows describing an older canonical file. Callers
/// that change the store document refresh the whole scope here.
///
/// Returns the number of rows updated.
///
/// # Errors
/// Returns [`DbError`] when the update fails.
pub fn update_store_hash_for_scope(
    connection: &Connection,
    kind: at_model::ItemKind,
    scope: Scope,
    project_id: Option<&str>,
    store_hash: &str,
) -> Result<usize, DbError> {
    let updated = connection.execute(
        "UPDATE deployments SET store_hash = ?1 \
          WHERE scope = ?2 AND COALESCE(project_id, '') = COALESCE(?3, '') \
            AND item_id IN (SELECT id FROM items WHERE kind = ?4)",
        params![store_hash, scope.wire_name(), project_id, kind.wire_name()],
    )?;
    Ok(updated)
}

/// Updates store, target, and baseline hashes for one item deployment cell matching
/// `item_id`, `agent_id`, `scope`, and `project_id`.
///
/// If `store_hash` is `Some`, `store_hash` is updated; otherwise existing `store_hash` is kept.
/// `target_hash` and `baseline_hash` are set to `target_and_baseline_hash`.
///
/// Returns the number of rows updated.
///
/// # Errors
/// Returns [`DbError`] when the database update fails.
pub fn update_item_baseline_hashes(
    connection: &Connection,
    item_id: &ItemId,
    agent_id: &str,
    scope: Scope,
    project_id: &str,
    store_hash: Option<&str>,
    target_and_baseline_hash: &str,
) -> Result<usize, DbError> {
    let updated = connection.execute(
        "UPDATE deployments SET store_hash = COALESCE(?1, store_hash), \
                target_hash = ?2, baseline_hash = ?2 \
          WHERE item_id = ?3 AND agent_id = ?4 AND scope = ?5 \
            AND COALESCE(project_id, '') = ?6",
        params![
            store_hash,
            target_and_baseline_hash,
            item_id.to_string(),
            agent_id,
            scope.wire_name(),
            project_id,
        ],
    )?;
    Ok(updated)
}

/// Updates store, target, and baseline hashes for one item deployment cell matching
/// `item_id`, `agent_id`, and `scope`.
///
/// Sets `store_hash = store_hash`, `target_hash = target_and_baseline_hash`,
/// and `baseline_hash = target_and_baseline_hash`.
///
/// Returns the number of rows updated.
///
/// # Errors
/// Returns [`DbError`] when the database update fails.
pub fn update_drift_hashes(
    connection: &Connection,
    item_id: &ItemId,
    agent_id: &AgentId,
    scope: Scope,
    store_hash: &str,
    target_and_baseline_hash: &str,
) -> Result<usize, DbError> {
    let updated = connection.execute(
        "UPDATE deployments SET store_hash = ?1, target_hash = ?2, baseline_hash = ?2 \
         WHERE item_id = ?3 AND agent_id = ?4 AND scope = ?5",
        params![
            store_hash,
            target_and_baseline_hash,
            item_id.to_string(),
            agent_id.to_string(),
            scope.wire_name(),
        ],
    )?;
    Ok(updated)
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

/// Target path of every deployment cell, keyed by item, agent and scope.
///
/// [`list_deployments_for_item`] costs one prepared statement per item, so a list projection
/// that reads one cell per agent column pays for thousands of statements on a full index. This
/// query answers the whole screen with one statement. A cell that exists in several projects
/// keeps the first row `ORDER BY item_id, agent_id, scope` returns, which is the same answer
/// the per-item loop's `find` produced.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored identifier is unreadable.
pub fn target_paths_by_cell(connection: &Connection) -> Result<DeploymentTargetPaths, DbError> {
    let mut statement = connection.prepare(
        "SELECT item_id, agent_id, scope, target_path FROM deployments \
         ORDER BY item_id, agent_id, scope",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            parse_cell_column::<ItemId>(row, 0)?,
            parse_cell_column::<AgentId>(row, 1)?,
            parse_cell_column::<Scope>(row, 2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut cells = HashMap::new();
    for row in rows {
        let (item_id, agent_id, scope, target_path) = row?;
        cells
            .entry((item_id, agent_id, scope))
            .or_insert(target_path);
    }
    Ok(cells)
}

/// One typed deployment column, with a parse failure reported as a SQL conversion error.
fn parse_cell_column<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text = row.get::<_, String>(index)?;
    text.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

/// Item ids of every deployment row one operation wrote, sorted and deduplicated.
///
/// An operation that migrated or adopted items stamps its id on each row it writes, so this
/// is how undo finds the metadata that must disappear with the files.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn operation_item_ids(
    connection: &Connection,
    operation_id: &str,
) -> Result<Vec<String>, DbError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT item_id FROM deployments WHERE operation_id = ?1 ORDER BY item_id",
    )?;
    let rows = statement
        .query_map([operation_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Project ids of every deployment row one operation wrote, sorted and deduplicated.
///
/// A Project-scope operation stamps its project on each deployment row it writes, so a
/// caller that only knows the operation id (the CLI undo, for example) can recover the
/// project root it must operate on without being told it again on the command line.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn operation_project_ids(
    connection: &Connection,
    operation_id: &str,
) -> Result<Vec<String>, DbError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT project_id FROM deployments \
         WHERE operation_id = ?1 AND project_id IS NOT NULL ORDER BY project_id",
    )?;
    let rows = statement
        .query_map([operation_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Deletes every deployment row one operation wrote and returns how many were removed.
///
/// # Errors
///
/// [`DbError`] when the delete fails.
pub fn delete_for_operation(connection: &Connection, operation_id: &str) -> Result<usize, DbError> {
    let deleted = connection.execute(
        "DELETE FROM deployments WHERE operation_id = ?1",
        [operation_id],
    )?;
    Ok(deleted)
}

/// Brings the deployment row an undone operation wrote back in line with the filesystem.
///
/// `undo_operation` restores files from `operation_files`; the deployment row the undone
/// operation wrote is metadata the file-level inverse does not touch. Left alone, undoing an
/// enable leaves the row claiming a link that no longer exists, and undoing a disable leaves a
/// restored link recorded as off. Either way the next reconcile reports AgenTuna's own undo as
/// external drift. Only the row owned by `operation_id` for `path` is changed, so a later
/// command that already took ownership of the cell keeps its state.
///
/// Returns the number of rows updated.
///
/// # Errors
///
/// [`DbError`] when the update fails.
pub fn align_undone_target(
    connection: &Connection,
    operation_id: &str,
    path: &Path,
    deployed: bool,
) -> Result<usize, DbError> {
    let path_text = path.to_string_lossy();
    let changed = if deployed {
        connection.execute(
            "UPDATE deployments SET state = 'linked', sync_label = 'in_sync', \
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
              WHERE operation_id = ?1 AND target_path = ?2 AND state = 'off'",
            params![operation_id, path_text.as_ref()],
        )?
    } else {
        // The target is gone. Every row that claimed it is stale, not only the rows of this
        // operation: a multi-phase delete removes the store row in a later phase, and leaving
        // another phase's row linked would report that file as newly drifted in between.
        connection.execute(
            "UPDATE deployments SET state = 'off', target_hash = NULL, baseline_hash = NULL, \
                    store_hash = NULL, link_target = NULL, sync_label = NULL, \
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
              WHERE target_path = ?2 AND state != 'off'",
            params![operation_id, path_text.as_ref()],
        )?
    };
    Ok(changed)
}

/// Deletes one deployment cell by item and agent.
///
/// # Errors
/// Returns the database failure when the delete fails.
pub fn delete_cell(
    connection: &Connection,
    item_id: &ItemId,
    agent_id: &AgentId,
) -> Result<usize, DbError> {
    Ok(connection.execute(
        "DELETE FROM deployments WHERE item_id = ?1 AND agent_id = ?2",
        params![item_id.to_string(), agent_id.to_string()],
    )?)
}

/// Deletes every deployment row that belongs to `item_id`.
///
/// Used when an item's canonical definition is removed: a row that outlives its manifest entry
/// describes a server the user can no longer open, and every later list, delete and drift pass
/// has to filter it out.
///
/// Returns the number of rows deleted.
///
/// # Errors
///
/// [`DbError`] when the delete fails.
pub fn delete_for_item(connection: &Connection, item_id: &ItemId) -> Result<usize, DbError> {
    let deleted = connection.execute(
        "DELETE FROM deployments WHERE item_id = ?1",
        [item_id.to_string()],
    )?;
    Ok(deleted)
}

/// How many deployment rows still reference `item_id`.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn count_for_item(connection: &Connection, item_id: &ItemId) -> Result<i64, DbError> {
    let count = connection.query_row(
        "SELECT COUNT(*) FROM deployments WHERE item_id = ?1",
        [item_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Per-agent deployment counts split by item kind for the Agents screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemKindCounts {
    /// Deployments of skills whose state is not `native`.
    pub managed_skills: i64,
    /// Deployments of MCP servers whose state is not `native`.
    pub managed_servers: i64,
    /// Skill rows AgenTuna does not manage (`native`).
    pub native_skills: i64,
    /// MCP server rows AgenTuna does not manage (`native`).
    pub native_servers: i64,
}

/// Counts managed and native deployments for one agent, split by item kind.
///
/// An unknown agent returns zero counts rather than an error.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn item_kind_counts(
    connection: &Connection,
    agent_id: &AgentId,
) -> Result<ItemKindCounts, DbError> {
    let mut statement = connection.prepare(
        "SELECT i.kind, \
                SUM(d.state != 'native'), \
                SUM(d.state = 'native') \
           FROM deployments d JOIN items i ON i.id = d.item_id \
          WHERE d.agent_id = ?1 AND i.kind IN ('skill', 'mcp_server') \
          GROUP BY i.kind",
    )?;
    let mut counts = ItemKindCounts::default();
    let rows = statement.query_map(params![agent_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            row.get::<_, Option<i64>>(2)?.unwrap_or(0),
        ))
    })?;
    for row in rows {
        let (kind, managed, native) = row?;
        match kind.as_str() {
            "skill" => {
                counts.managed_skills = managed;
                counts.native_skills = native;
            }
            "mcp_server" => {
                counts.managed_servers = managed;
                counts.native_servers = native;
            }
            _ => {}
        }
    }
    Ok(counts)
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

/// Count of deployments that materialise as a link (`symlink` or Windows `junction`).
///
/// The Agents card needs this one number; reading every deployment row of the agent to count
/// it allocates thousands of cells per screen load on a full index (TF-035).
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn linked_mode_count(connection: &Connection, agent_id: &AgentId) -> Result<i64, DbError> {
    let count = connection.query_row(
        "SELECT COUNT(*) FROM deployments \
          WHERE agent_id = ?1 AND mode IN ('symlink', 'junction')",
        [agent_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(count)
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

/// Snapshot of one `deployments` row captured before a delete removed its item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoDeploymentSnapshot {
    /// Owning item identity.
    pub item_id: String,
    /// Agent identity.
    pub agent_id: String,
    /// Scope wire name.
    pub scope: String,
    /// Owning project id.
    pub project_id: Option<String>,
    /// Deployment state.
    pub state: String,
    /// Stored mode.
    pub mode: Option<String>,
    /// Native target path.
    pub target_path: Option<String>,
    /// Link target.
    pub link_target: Option<String>,
    /// Canonical store hash.
    pub store_hash: Option<String>,
    /// Hash observed at the target.
    pub target_hash: Option<String>,
    /// Baseline hash recorded at write time.
    pub baseline_hash: Option<String>,
    /// Sync label.
    pub sync_label: Option<String>,
    /// Operation that last wrote the row.
    ///
    /// Undo re-alignment matches on it: a row restored by a later delete's undo can only be
    /// flipped back to `linked` by undoing the operation that turned it off.
    pub operation_id: Option<String>,
}

/// Reads every deployment row of one item for undo metadata.
///
/// # Errors
/// Returns the database failure when the rows cannot be read.
pub fn undo_snapshots(
    connection: &Connection,
    item_id: &ItemId,
) -> Result<Vec<UndoDeploymentSnapshot>, DbError> {
    let mut statement = connection.prepare(
        "SELECT item_id, agent_id, scope, project_id, state, mode, target_path, link_target, \
         store_hash, target_hash, baseline_hash, sync_label, operation_id \
         FROM deployments WHERE item_id = ?1 ORDER BY id",
    )?;
    let rows = statement.query_map([item_id.to_string()], |row| {
        Ok(UndoDeploymentSnapshot {
            item_id: row.get(0)?,
            agent_id: row.get(1)?,
            scope: row.get(2)?,
            project_id: row.get(3)?,
            state: row.get(4)?,
            mode: row.get(5)?,
            target_path: row.get(6)?,
            link_target: row.get(7)?,
            store_hash: row.get(8)?,
            target_hash: row.get(9)?,
            baseline_hash: row.get(10)?,
            sync_label: row.get(11)?,
            operation_id: row.get(12)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Restores captured deployment rows for one item, replacing its current cells.
///
/// # Errors
/// Returns the database failure when the rows cannot be written.
pub fn restore_undo_snapshots(
    connection: &Connection,
    item_id: &ItemId,
    snapshots: &[UndoDeploymentSnapshot],
) -> Result<(), DbError> {
    connection.execute(
        "DELETE FROM deployments WHERE item_id = ?1",
        [item_id.to_string()],
    )?;
    for deployment in snapshots {
        connection.execute(
            "INSERT INTO deployments (item_id, agent_id, scope, project_id, state, mode, \
             target_path, link_target, store_hash, target_hash, baseline_hash, sync_label, \
             operation_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                deployment.item_id,
                deployment.agent_id,
                deployment.scope,
                deployment.project_id,
                deployment.state,
                deployment.mode,
                deployment.target_path,
                deployment.link_target,
                deployment.store_hash,
                deployment.target_hash,
                deployment.baseline_hash,
                deployment.sync_label,
                deployment.operation_id,
            ],
        )?;
    }
    Ok(())
}

/// One deployment cell plus the item kind and slug, for a native path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentCellSnapshot {
    /// Owning item identity.
    pub item_id: String,
    /// Agent identity.
    pub agent_id: String,
    /// Scope wire name.
    pub scope: String,
    /// Owning project id.
    pub project_id: Option<String>,
    /// Item kind wire name.
    pub kind: String,
    /// Item slug.
    pub slug: String,
    /// Recorded mode.
    pub mode: Option<String>,
    /// Recorded link target.
    pub link_target: Option<String>,
}

/// Lists the deployment cells that claim one native target path.
///
/// # Errors
/// Returns the database failure when the rows cannot be read.
pub fn cells_for_target(
    connection: &Connection,
    target_path: &str,
) -> Result<Vec<DeploymentCellSnapshot>, DbError> {
    let mut statement = connection.prepare(
        "SELECT d.item_id, d.agent_id, d.scope, d.project_id, i.kind, i.slug, d.mode, \
                d.link_target \
         FROM deployments d JOIN items i ON i.id = d.item_id \
         WHERE d.target_path = ?1 AND d.state <> 'off' \
           AND i.kind IN ('skill', 'mcp_server')",
    )?;
    let rows = statement.query_map([target_path], |row| {
        Ok(DeploymentCellSnapshot {
            item_id: row.get(0)?,
            agent_id: row.get(1)?,
            scope: row.get(2)?,
            project_id: row.get(3)?,
            kind: row.get(4)?,
            slug: row.get(5)?,
            mode: row.get(6)?,
            link_target: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Updates the hashes and materialisation of one deployment cell.
///
/// # Errors
/// Returns the database failure when the update fails.
#[allow(clippy::too_many_arguments)]
pub fn update_hashes_and_mode(
    connection: &Connection,
    item_id: &str,
    agent_id: &str,
    scope: Scope,
    project_id: Option<&str>,
    store_hash: Option<&str>,
    target_hash: &str,
    baseline_hash: Option<&str>,
    mode: &str,
    link_target: Option<&str>,
) -> Result<usize, DbError> {
    Ok(connection.execute(
        "UPDATE deployments SET store_hash = ?1, target_hash = ?2, baseline_hash = ?3, \
                mode = ?4, link_target = ?5 \
         WHERE item_id = ?6 AND agent_id = ?7 AND scope = ?8 \
           AND COALESCE(project_id, '') = COALESCE(?9, '')",
        params![
            store_hash,
            target_hash,
            baseline_hash,
            mode,
            link_target,
            item_id,
            agent_id,
            scope.wire_name(),
            project_id,
        ],
    )?)
}
