//! Overview queries: scope counts, last edited, recent changes (TA-042).
//!
//! The SQL layer for TA-009's `at_engine::overview::build`. Every function is
//! read-only and uses one prepared statement — no write transaction is held
//! while the UI renders, and no query fans out into per-row lookups.
//!
//! - [`counts_for_scope`] — skills/mcp/rules/locks in one query.
//! - [`last_edited`] — the newest `operations.created_at` per scope.
//! - [`recent_changes`] — the history timeline with file counts.
//! - [`failing_health_checks`], [`drift_candidates`], [`native_deployment_candidates`],
//!   [`missing_agent_candidates`] — needs-attention candidates.

use at_model::{AgentId, ItemId, Scope};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::DbError;

use crate::shared::operation_from_row;

/// Per-scope counts the overview card shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeCounts {
    /// Items of kind `skill` at this scope.
    pub skills: i64,
    /// Items of kind `mcp_server` at this scope.
    pub mcp: i64,
    /// Items of kind `rule` at this scope.
    pub rules: i64,
    /// Locks placed at this scope.
    pub locks: i64,
}

/// One scope's counts plus its last-edited timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOverview {
    /// The scope.
    pub scope: Scope,
    /// Item and lock counts.
    pub counts: ScopeCounts,
    /// Newest `operations.created_at` at this scope, when any exist.
    pub last_edited: Option<String>,
    /// Actor that performed the newest operation at this scope, when any exist.
    pub last_edited_by: Option<String>,
}

/// One row of the recent-changes timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentChangeRow {
    /// The operation.
    pub operation: crate::shared::OperationRow,
    /// How many files the operation touched.
    pub file_count: i64,
}

/// A needs-attention candidate: one row the overview can jump to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionCandidate {
    /// What kind of problem this is.
    pub kind: AttentionKind,
    /// Severity for ordering (lower sorts first).
    pub severity: AttentionSeverity,
    /// Item or agent the candidate belongs to, when known.
    pub item_id: Option<ItemId>,
    /// Agent the candidate belongs to, when known.
    pub agent_id: Option<AgentId>,
    /// Scope the candidate belongs to.
    pub scope: Option<Scope>,
    /// One-line title.
    pub title: String,
    /// One-line subtitle.
    pub subtitle: Option<String>,
    /// Navigation destination for the exact row.
    pub navigation: Option<NavigationTarget>,
}

/// Destination the UI can route to from an attention row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigationTarget {
    /// Screen identifier (skills, mcp, drift or agents).
    pub screen: String,
    /// Exact row identifier, when one exists.
    pub row_id: Option<String>,
}

/// The kind of needs-attention candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AttentionKind {
    /// A health check failed.
    FailingHealthCheck,
    /// An external edit drifted from the deployment.
    DriftEvent,
    /// A deployment is `native` (not managed by AgenTuna).
    NativeDeployment,
    /// An agent row is missing or unmanaged.
    MissingAgent,
    /// A lower-precedence skill copy is shadowed.
    ShadowedSkill,
}

/// An indexed shadowed skill for Overview attention rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowedSkillRow {
    /// Stable item identity.
    pub item_id: ItemId,
    /// Display name.
    pub name: String,
    /// Lower-scope copy.
    pub scope: Scope,
    /// Effective winning scope.
    pub winner_scope: Scope,
}

/// Lists skills that are shadowed by a higher-precedence canonical scope copy.
///
/// # Errors
/// Returns [`DbError`] when a query fails or stored scope/item identifiers are invalid.
pub fn shadowed_skills(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<ShadowedSkillRow>, DbError> {
    let limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT i.id, i.name, i.scope, (SELECT i2.scope FROM items i2 \
           WHERE i2.kind='skill' AND i2.slug=i.slug AND i2.scope != i.scope \
             AND (i2.scope NOT IN ('project','local') OR i2.project_id IS i.project_id) \
             AND CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > \
                 CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END \
           ORDER BY CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END DESC LIMIT 1) \
         FROM items i WHERE i.kind='skill' \
           AND EXISTS (SELECT 1 FROM items i2 WHERE i2.kind='skill' AND i2.slug=i.slug AND i2.scope != i.scope \
             AND (i2.scope NOT IN ('project','local') OR i2.project_id IS i.project_id) \
             AND CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > \
                 CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END) \
         ORDER BY i.name COLLATE NOCASE, i.scope LIMIT ?1"
    )?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, name, scope, winner_scope)| {
            Ok(ShadowedSkillRow {
                item_id: id.parse().map_err(DbError::from)?,
                name,
                scope: scope.parse().map_err(DbError::from)?,
                winner_scope: winner_scope.parse().map_err(DbError::from)?,
            })
        })
        .collect()
}

/// Converts indexed shadowed skill copies into attention candidates.
///
/// # Errors
/// Returns [`DbError`] when either query fails.
pub fn shadowed_skill_candidates(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    shadowed_skills(connection, limit).map(|rows| {
        rows.into_iter()
            .map(|row| AttentionCandidate {
                kind: AttentionKind::ShadowedSkill,
                severity: AttentionSeverity::Warning,
                item_id: Some(row.item_id),
                agent_id: None,
                scope: Some(row.scope),
                title: format!("Skill shadowed: {}", row.name),
                subtitle: Some(format!("Winning copy is at {}", row.winner_scope)),
                navigation: Some(NavigationTarget {
                    screen: "skills".into(),
                    row_id: Some(row.item_id.to_string()),
                }),
            })
            .collect()
    })
}

/// Severity ordering for needs-attention candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AttentionSeverity {
    /// Critical: broken or unsafe.
    Critical,
    /// Warning: degraded but usable.
    Warning,
    /// Info: worth knowing.
    Info,
}

/// Counts per scope per kind, in one query.
///
/// Returns one [`ScopeCounts`] per scope that has items or locks; scopes with
/// neither are absent. The overview card fills missing scopes with zeroes.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn counts_for_scope(connection: &Connection) -> Result<Vec<(Scope, ScopeCounts)>, DbError> {
    let mut statement = connection.prepare(
        "SELECT scope, \
                SUM(kind = 'skill') AS skills, \
                SUM(kind = 'mcp_server') AS mcp, \
                SUM(kind = 'rule') AS rules, \
                0 AS locks \
           FROM items \
          GROUP BY scope \
          UNION ALL \
         SELECT scope, 0, 0, 0, COUNT(*) \
           FROM locks \
          GROUP BY scope",
    )?;
    let mut rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ScopeCounts {
                    skills: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    mcp: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    rules: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    locks: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Merge the two UNION ALL branches: items rows carry kinds, locks rows
    // carry only the lock count.
    let mut merged: std::collections::BTreeMap<Scope, ScopeCounts> =
        std::collections::BTreeMap::new();
    for (scope_text, counts) in rows.drain(..) {
        let scope: Scope = scope_text.parse().map_err(DbError::from)?;
        let entry = merged.entry(scope).or_default();
        entry.skills += counts.skills;
        entry.mcp += counts.mcp;
        entry.rules += counts.rules;
        entry.locks += counts.locks;
    }
    Ok(merged.into_iter().collect())
}

/// The newest `operations.created_at` per scope.
///
/// Returns one [`ScopeOverview`] per scope that has operations; scopes with no
/// operations are absent (the overview card renders "Not set up"). `last_edited`
/// is `None` when a scope has no operations.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn last_edited(connection: &Connection) -> Result<Vec<ScopeOverview>, DbError> {
    let mut statement = connection.prepare(
        "SELECT o.scope, o.created_at, o.actor \
           FROM operations o \
           JOIN (SELECT scope, MAX(created_at) AS latest \
                   FROM operations \
                  WHERE scope IS NOT NULL \
                  GROUP BY scope) newest \
             ON newest.scope = o.scope AND newest.latest = o.created_at \
          ORDER BY o.id DESC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut newest: std::collections::BTreeMap<Scope, (Option<String>, Option<String>)> =
        std::collections::BTreeMap::new();
    for (scope_text, last_edited, actor) in rows {
        let scope: Scope = scope_text.parse().map_err(DbError::from)?;
        // Rows arrive newest id first; the first row per scope is the newest operation.
        newest.entry(scope).or_insert((last_edited, actor));
    }
    Ok(newest
        .into_iter()
        .map(|(scope, (last_edited, last_edited_by))| ScopeOverview {
            scope,
            counts: ScopeCounts::default(),
            last_edited,
            last_edited_by,
        })
        .collect())
}

/// The recent-changes timeline: newest operations first, with file counts.
///
/// `limit` is clamped to `1..=500`; ties on `created_at` break by `id` so the
/// order is stable. One prepared statement joins `operations` to
/// `operation_files` for the count; no per-row queries.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored enum value is unknown.
pub fn recent_changes(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<RecentChangeRow>, DbError> {
    let effective_limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT o.id, o.kind, o.scope, o.actor, o.status, o.summary, o.error_kind, \
                o.started_at, o.created_at, o.finished_at, o.correlation_id, o.undo_of, \
                COUNT(f.id) AS file_count \
           FROM operations o \
           LEFT JOIN operation_files f ON f.operation_id = o.id \
          GROUP BY o.id \
          ORDER BY o.created_at DESC, o.id DESC \
          LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![effective_limit], |row| {
            Ok((operation_from_row(row)?, row.get::<_, i64>(12)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(operation, file_count)| RecentChangeRow {
            operation,
            file_count,
        })
        .collect())
}

/// Health checks that failed, as needs-attention candidates.
///
/// One row per `(item_id, agent_id)` pair whose newest check is `failed`.
/// `limit` caps the result; the overview shows the first few and a total.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn failing_health_checks(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    let effective_limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT h.item_id, h.agent_id, h.scope, h.server_name, h.error_code \
           FROM health_checks h \
           JOIN ( \
               SELECT item_id, agent_id, MAX(checked_at) AS latest \
                 FROM health_checks \
                GROUP BY item_id, agent_id \
           ) latest ON latest.item_id = h.item_id \
                    AND latest.agent_id = h.agent_id \
                    AND latest.latest = h.checked_at \
          WHERE h.status = 'failed' \
          ORDER BY h.checked_at DESC \
          LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![effective_limit], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (item_id, agent_id, scope, server_name, error_code) in rows {
        out.push(AttentionCandidate {
            navigation: Some(NavigationTarget {
                screen: "mcp".into(),
                row_id: item_id.clone(),
            }),
            kind: AttentionKind::FailingHealthCheck,
            severity: AttentionSeverity::Warning,
            item_id: item_id
                .as_deref()
                .map(|s| s.parse::<ItemId>())
                .transpose()
                .map_err(DbError::from)?,
            agent_id: agent_id
                .as_deref()
                .map(|s| s.parse::<AgentId>())
                .transpose()
                .map_err(DbError::from)?,
            scope: Some(scope.parse::<Scope>().map_err(DbError::from)?),
            title: format!("Health check failed: {server_name}"),
            subtitle: error_code.clone(),
        });
    }
    Ok(out)
}

/// Unresolved drift events, as needs-attention candidates.
///
/// # Errors
///
/// [`DbError`] when the query fails or a stored value is unknown.
pub fn drift_candidates(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    let effective_limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT id, item_id, agent_id, scope, path, detected_at \
           FROM drift_events \
          WHERE resolved_at IS NULL \
          ORDER BY detected_at DESC \
          LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![effective_limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (_id, item_id, agent_id, scope, path, detected_at) in rows {
        out.push(AttentionCandidate {
            navigation: Some(NavigationTarget {
                screen: "drift".into(),
                row_id: Some(_id.to_string()),
            }),
            kind: AttentionKind::DriftEvent,
            severity: AttentionSeverity::Warning,
            item_id: item_id
                .as_deref()
                .map(|s| s.parse::<ItemId>())
                .transpose()
                .map_err(DbError::from)?,
            agent_id: Some(agent_id.parse::<AgentId>().map_err(DbError::from)?),
            scope: Some(scope.parse::<Scope>().map_err(DbError::from)?),
            title: format!("Drift detected: {path}"),
            subtitle: Some(detected_at.clone()),
        });
    }
    Ok(out)
}

/// Deployments in `native` state, as needs-attention candidates.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn native_deployment_candidates(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    let effective_limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT d.item_id, d.agent_id, d.scope, d.project_id, i.name \
           FROM deployments d \
           JOIN items i ON i.id = d.item_id \
          WHERE d.state = 'native' \
          ORDER BY i.name COLLATE NOCASE \
          LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![effective_limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (item_id, agent_id, scope, project_id, name) in rows {
        out.push(AttentionCandidate {
            kind: AttentionKind::NativeDeployment,
            severity: AttentionSeverity::Info,
            item_id: Some(item_id.parse::<ItemId>().map_err(DbError::from)?),
            agent_id: Some(agent_id.parse::<AgentId>().map_err(DbError::from)?),
            scope: Some(scope.parse::<Scope>().map_err(DbError::from)?),
            title: format!("Native item: {name}"),
            subtitle: project_id.clone(),
            navigation: Some(NavigationTarget {
                screen: "skills".into(),
                row_id: Some(item_id.clone()),
            }),
        });
    }
    Ok(out)
}

/// Agents that are installed but unmanaged, as needs-attention candidates.
///
/// # Errors
///
/// [`DbError`] when the query fails.
pub fn missing_agent_candidates(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    let effective_limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT id, display_name, version \
           FROM agents \
          WHERE installed = 1 AND managed = 0 \
          ORDER BY COALESCE(display_name, id) \
          LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![effective_limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, display_name, version) in rows {
        out.push(AttentionCandidate {
            kind: AttentionKind::MissingAgent,
            severity: AttentionSeverity::Info,
            item_id: None,
            agent_id: Some(id.parse::<AgentId>().map_err(DbError::from)?),
            scope: None,
            title: format!(
                "Agent not managed: {}",
                display_name.unwrap_or_else(|| id.clone())
            ),
            subtitle: version.clone(),
            navigation: Some(NavigationTarget {
                screen: "agents".into(),
                row_id: Some(id.clone()),
            }),
        });
    }
    Ok(out)
}

/// All needs-attention candidates, ordered by severity then kind.
///
/// The overview calls this once and renders the first few per class; the
/// total count is retained so the UI can show "Show all N".
///
/// # Errors
///
/// [`DbError`] when any of the underlying queries fails.
pub fn needs_attention(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<AttentionCandidate>, DbError> {
    let mut all = Vec::new();
    all.extend(failing_health_checks(connection, limit)?);
    all.extend(shadowed_skill_candidates(connection, limit)?);
    all.extend(drift_candidates(connection, limit)?);
    all.extend(native_deployment_candidates(connection, limit)?);
    all.extend(missing_agent_candidates(connection, limit)?);
    all.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.kind.cmp(&b.kind))
            .then(a.title.cmp(&b.title))
    });
    all.truncate(limit);
    Ok(all)
}
