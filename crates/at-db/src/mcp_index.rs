//! MCP servers × agents × latest-health list projection.
use at_model::{AgentId, ItemId, Scope};
use rusqlite::{Connection, params};
use std::collections::HashMap;

use crate::DbError;

/// Agent-specific status in the MCP list matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpCellState {
    /// A deployment is linked to the canonical definition.
    Linked,
    /// The item is explicitly disabled for this agent.
    Off,
    /// A lock prevents changes at this or a higher-precedence scope.
    Locked,
    /// This definition loses to a higher-precedence copy.
    Shadowed,
    /// A native-only server was discovered for this agent.
    Native {
        /// Native definition path.
        path: String,
    },
    /// Agent is installed but no deployment exists.
    None,
    /// Agent is not installed.
    NotInstalled,
}

/// One MCP server's cell for an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAgentCell {
    /// Agent identity.
    pub agent_id: AgentId,
    /// Projected state.
    pub state: McpCellState,
    /// Config path for a deployment or native server.
    pub config_path: Option<String>,
}

/// A server row, including its matrix cells and newest health result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpListRow {
    /// Canonical identity; native-only rows have no canonical id.
    pub item_id: Option<ItemId>,
    /// Display name.
    pub display_name: String,
    /// Canonical or native discovery scope.
    pub scope: Scope,
    /// Source path of the canonical definition or native discovery.
    pub source_path: Option<String>,
    /// Transport label when present in a stored item state; otherwise unknown.
    pub transport_subtitle: Option<String>,
    /// Scope that currently locks this item.
    pub locked_by: Option<Scope>,
    /// Most recent health status, if checked.
    pub latest_health: Option<String>,
    /// Cell per detected or configured agent.
    pub agents: Vec<McpAgentCell>,
    /// True for a row synthesized from native discovery.
    pub native_only: bool,
}

/// Totals for MCP list chips, derived from the same row set as the table.
///
/// Returns [`DbError`] if the list query fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpListCounts {
    /// Total server rows.
    pub all: usize,
    /// Native-only rows.
    pub not_in_agentuna: usize,
    /// Latest failed health rows excluding locked servers.
    pub failing: usize,
}

/// Computes the MCP list chips from the list query's own row set.
///
/// # Errors
/// Returns [`DbError`] if [`mcp_list_rows`] fails.
pub fn mcp_list_counts(
    connection: &Connection,
    scope: Option<Scope>,
    project_id: Option<&str>,
) -> Result<McpListCounts, DbError> {
    let rows = mcp_list_rows(connection, scope, project_id)?;
    Ok(McpListCounts {
        all: rows.len(),
        not_in_agentuna: rows.iter().filter(|row| row.native_only).count(),
        failing: rows
            .iter()
            .filter(|row| row.latest_health.as_deref() == Some("failed") && row.locked_by.is_none())
            .count(),
    })
}
/// Queries MCP rows for a scope/project, with one row per server and one cell per agent.
///
/// Native-only definitions are included alongside canonical rows. The newest health check
/// is selected by an indexed correlated lookup, not by scanning history for every result row.
///
/// # Errors
/// Returns [`DbError`] if SQLite fails or a stored item/agent/scope identifier is invalid.
pub fn mcp_list_rows(
    connection: &Connection,
    scope: Option<Scope>,
    project_id: Option<&str>,
) -> Result<Vec<McpListRow>, DbError> {
    let mut statement = connection.prepare(
        "WITH servers AS ( \
           SELECT i.id AS item_id, i.name, i.scope, i.slug, i.project_id, 0 AS native_only, NULL AS native_path, NULL AS native_agent, \
                  (SELECT l.scope FROM locks l WHERE l.item_id=i.id ORDER BY CASE l.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END DESC LIMIT 1) AS locked_by, \
                  EXISTS (SELECT 1 FROM items higher WHERE higher.kind='mcp_server' AND higher.slug=i.slug AND higher.scope != i.scope \
                    AND (higher.scope NOT IN ('project','local') OR higher.project_id IS i.project_id) \
                    AND CASE higher.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END) AS shadowed, \
                  (SELECT h.status FROM health_checks h WHERE h.item_id=i.id ORDER BY h.checked_at DESC, h.id DESC LIMIT 1) AS health, i.source_path \
             FROM items i WHERE i.kind='mcp_server' AND (?1 IS NULL OR i.scope=?1) AND (?2 IS NULL OR i.project_id=?2 OR i.project_id IS NULL) \
           UNION ALL \
           SELECT NULL, n.name, n.scope, n.name, NULL, 1, n.path, n.agent_id, NULL, 0, \
                  (SELECT h.status FROM health_checks h WHERE h.item_id=n.id OR (h.item_id IS NULL AND h.agent_id=n.agent_id AND h.server_name=n.name) ORDER BY h.checked_at DESC, h.id DESC LIMIT 1), n.path \
             FROM native_items n WHERE n.kind='mcp_server' AND n.status='new' AND n.adopted_item_id IS NULL \
               AND (?1 IS NULL OR n.scope=?1) AND (?2 IS NULL OR n.scope NOT IN ('project','local') OR ?1 IN ('project','local')) \
           ) \
         SELECT s.item_id, s.name, s.scope, s.native_only, s.native_path, s.native_agent, s.locked_by, s.shadowed, s.health, \
                a.id, a.installed, d.state, d.target_path, s.source_path \
           FROM servers s CROSS JOIN agents a \
           LEFT JOIN deployments d ON d.item_id=s.item_id AND d.agent_id=a.id AND d.scope=s.scope AND d.project_id IS s.project_id \
          ORDER BY s.native_only, lower(s.name), s.name, a.id"
    )?;
    let rows = statement.query_map(params![scope.map(|s| s.wire_name()), project_id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, bool>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, bool>(10)?,
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, Option<String>>(13)?,
        ))
    })?;
    let mut result: Vec<McpListRow> = Vec::new();
    let mut indexes: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let (
            id,
            name,
            scope_text,
            native_only,
            native_path,
            native_agent,
            lock_text,
            shadowed,
            health,
            agent_text,
            installed,
            deployment_state,
            target_path,
            source_path,
        ) = row?;
        let row_scope = parse_scope(&scope_text, "items.scope")?;
        let item_id = id
            .map(|value| {
                value
                    .parse()
                    .map_err(|e: at_model::ItemIdError| DbError::InvalidValue {
                        field: "items.id",
                        reason: e.to_string(),
                    })
            })
            .transpose()?;
        let locked_by = lock_text
            .as_deref()
            .map(|v| parse_scope(v, "locks.scope"))
            .transpose()?;
        let agent_id: AgentId =
            agent_text
                .parse()
                .map_err(|e: at_model::AgentIdError| DbError::InvalidValue {
                    field: "agents.id",
                    reason: e.to_string(),
                })?;
        let native_agent_matches = native_agent.as_deref() == Some(agent_id.to_string().as_str());
        let cell_state = if native_only && native_agent_matches {
            McpCellState::Native {
                path: native_path.clone().unwrap_or_default(),
            }
        } else if native_only {
            if installed {
                McpCellState::None
            } else {
                McpCellState::NotInstalled
            }
        } else if locked_by.is_some() {
            McpCellState::Locked
        } else if shadowed {
            McpCellState::Shadowed
        } else {
            match deployment_state.as_deref() {
                Some("linked") | Some("native") => McpCellState::Linked,
                Some("off") => McpCellState::Off,
                Some("conflict") | Some("drifted") => McpCellState::Linked,
                Some(value) => {
                    return Err(DbError::InvalidValue {
                        field: "deployments.state",
                        reason: format!("unknown state `{value}`"),
                    });
                }
                None if installed => McpCellState::None,
                None => McpCellState::NotInstalled,
            }
        };
        let cell = McpAgentCell {
            agent_id,
            state: cell_state,
            config_path: if native_only && !native_agent_matches {
                None
            } else {
                target_path.or(native_path.clone())
            },
        };
        let key = item_id
            .as_ref()
            .map(ToString::to_string)
            .or(native_path.clone())
            .unwrap_or_else(|| name.clone());
        if let Some(index) = indexes.get(&key).copied() {
            result[index].agents.push(cell);
        } else {
            indexes.insert(key, result.len());
            result.push(McpListRow {
                item_id,
                display_name: name,
                scope: row_scope,
                source_path,
                transport_subtitle: None,
                locked_by,
                latest_health: health,
                agents: vec![cell],
                native_only,
            });
        }
    }
    Ok(result)
}

fn parse_scope(value: &str, field: &'static str) -> Result<Scope, DbError> {
    value
        .parse()
        .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
            field,
            reason: e.to_string(),
        })
}
