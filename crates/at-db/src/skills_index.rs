//! Skills list projections and filters for the Skills screen and CLI.

use std::collections::{BTreeMap, HashMap};

use at_model::{AgentId, DeploymentState, ItemId, Scope};
use rusqlite::{Connection, params};

use crate::DbError;

/// Filters applied to [`list_skills`].
#[derive(Clone, Debug, Default)]
pub struct SkillFilter {
    /// Restrict rows to one scope.
    pub scope: Option<Scope>,
    /// Include native-only skill rows; disabled by default.
    pub include_native: bool,
    /// Include only items shadowed by a higher-precedence copy.
    pub shadowed_only: bool,
    /// Include only native-only skills that have not been adopted.
    pub not_in_agentuna: bool,
    /// Restrict deployment cells and matching to one agent.
    pub agent: Option<AgentId>,
    /// Case-insensitive substring matched against name, slug and tags.
    pub text: Option<String>,
}

/// One canonical or native skill row for the skills list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRow {
    /// Canonical item id; native-only rows have no canonical id.
    pub item_id: Option<ItemId>,
    /// Display name.
    pub name: String,
    /// Canonical scope, or the native discovery scope.
    pub scope: Scope,
    /// Stored item state, when canonical.
    pub state: Option<String>,
    /// Highest-precedence matching definition, when this row is shadowed.
    pub shadowed_by: Option<Scope>,
    /// Per-agent deployment state.
    pub deployed: BTreeMap<AgentId, DeploymentState>,
    /// Native path if the item exists only in an agent folder.
    pub native_path: Option<String>,
    /// Whether the source advertises an update.
    pub update_available: bool,
    /// Last item update timestamp or native scan timestamp.
    pub updated_at: String,
    /// One-line description parsed from SKILL.md at index time; native-only rows have none.
    pub description: Option<String>,
}

/// Lists canonical skills and optionally native-only discoveries.
///
/// All data is read from SQLite; this query never accesses the filesystem.
///
/// # Errors
/// Returns [`DbError`] for invalid stored identifiers, agent ids, scopes or SQL.
pub fn list_skills(
    connection: &Connection,
    filter: &SkillFilter,
) -> Result<Vec<SkillRow>, DbError> {
    let mut rows = Vec::new();
    let mut statement = connection.prepare(
        "SELECT i.id, i.name, i.scope, i.state, i.updated_at, \
                (SELECT i2.scope FROM items i2 WHERE i2.kind='skill' AND i2.slug=i.slug \
                   AND i2.scope IN ('global','user','project','local') \
                   AND i2.scope != i.scope \
                   AND (i2.scope NOT IN ('project','local') OR i2.project_id IS i.project_id) \
                   AND (i2.scope NOT IN ('global','user') OR i2.project_id IS NULL) \
                   AND CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > \
                       CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END \
                   ORDER BY CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END DESC LIMIT 1), \
                (SELECT s.update_available FROM sources s WHERE s.item_id=i.id AND s.deleted_at IS NULL LIMIT 1), \
                (SELECT n.path FROM native_items n WHERE n.kind='skill' AND n.status='new' AND n.adopted_item_id IS NULL \
                   AND (n.name=i.name OR n.name=i.slug) ORDER BY n.path LIMIT 1), a.id, a.installed, d.state, \
                json_extract(i.tags_json,'$.description') \
           FROM items i \
           LEFT JOIN agents a ON (?4 IS NULL OR a.id=?4) \
           LEFT JOIN deployments d ON d.item_id=i.id AND d.agent_id=a.id AND d.scope=i.scope AND d.project_id IS i.project_id \
          WHERE i.kind='skill' \
             AND (?1 IS NULL OR i.scope=?1) \
             AND (?2 IS NULL OR lower(i.name) LIKE '%' || lower(?2) || '%' OR lower(i.slug) LIKE '%' || lower(?2) || '%' OR lower(COALESCE(i.tags_json,'')) LIKE '%' || lower(?2) || '%' \
                  OR lower(COALESCE(json_extract(i.tags_json,'$.description'),'')) LIKE '%' || lower(?2) || '%') \
             AND (?3=0 OR EXISTS (SELECT 1 FROM items i2 WHERE i2.kind='skill' AND i2.slug=i.slug AND i2.scope != i.scope \
                   AND (i2.scope NOT IN ('project','local') OR i2.project_id IS i.project_id) \
                   AND CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > \
                       CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END)) \
           ORDER BY lower(i.name), i.scope, i.id, a.id"
    )?;
    let canonical = statement.query_map(
        params![
            filter.scope.map(|s| s.wire_name()),
            filter.text,
            filter.shadowed_only,
            filter.agent.as_ref().map(ToString::to_string)
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<bool>>(6)?.unwrap_or(false),
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<bool>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        },
    )?;
    let mut indexes: HashMap<String, usize> = HashMap::new();
    for row in canonical {
        let (
            id_text,
            name,
            scope_text,
            state,
            updated_at,
            shadowed_by,
            update_available,
            native_path,
            agent_text,
            installed,
            deployment_state,
            description,
        ) = row?;
        let id: ItemId = id_text.parse()?;
        let scope: Scope =
            scope_text
                .parse()
                .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
                    field: "items.scope",
                    reason: e.to_string(),
                })?;
        let shadowed_by = shadowed_by
            .map(|s| {
                s.parse()
                    .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
                        field: "items.scope",
                        reason: e.to_string(),
                    })
            })
            .transpose()?;
        let index = if let Some(index) = indexes.get(&id_text) {
            *index
        } else {
            let index = rows.len();
            indexes.insert(id_text, index);
            rows.push(SkillRow {
                item_id: Some(id),
                name,
                scope,
                state,
                shadowed_by,
                deployed: BTreeMap::new(),
                native_path,
                update_available,
                updated_at,
                description,
            });
            index
        };
        if let Some(agent) = agent_text {
            let agent_id =
                agent
                    .parse()
                    .map_err(|e: at_model::AgentIdError| DbError::InvalidValue {
                        field: "agents.id",
                        reason: e.to_string(),
                    })?;
            let projected = match deployment_state.as_deref() {
                Some("linked") => DeploymentState::Linked,
                Some("off") => DeploymentState::Off,
                Some("native") => DeploymentState::Native,
                Some("conflict") => DeploymentState::Conflict,
                Some("drifted") => DeploymentState::Drifted,
                Some(value) => {
                    return Err(DbError::InvalidValue {
                        field: "deployments.state",
                        reason: format!("unknown state `{value}`"),
                    });
                }
                None if installed.unwrap_or(false) => DeploymentState::None,
                None => DeploymentState::NotInstalled,
            };
            rows[index].deployed.insert(agent_id, projected);
        }
    }
    if filter.include_native || filter.not_in_agentuna {
        let mut native = connection.prepare(
            "SELECT n.name, n.scope, n.path, n.last_seen_at, n.agent_id \
               FROM native_items n WHERE n.kind='skill' AND n.status='new' AND n.adopted_item_id IS NULL \
                 AND (?1 IS NULL OR n.scope=?1) \
                 AND (?2 IS NULL OR lower(n.name) LIKE '%' || lower(?2) || '%' OR lower(n.path) LIKE '%' || lower(?2) || '%') \
                 AND (?3 IS NULL OR n.agent_id=?3) \
               ORDER BY lower(n.name), n.path"
        )?;
        let native_rows = native.query_map(
            params![
                filter.scope.map(|s| s.wire_name()),
                filter.text,
                filter.agent.as_ref().map(ToString::to_string)
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?;
        for row in native_rows {
            let (name, scope, path, updated_at, agent) = row?;
            let scope =
                scope
                    .parse()
                    .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
                        field: "native_items.scope",
                        reason: e.to_string(),
                    })?;
            let agent =
                agent
                    .parse()
                    .map_err(|e: at_model::AgentIdError| DbError::InvalidValue {
                        field: "native_items.agent_id",
                        reason: e.to_string(),
                    })?;
            let mut deployed = BTreeMap::new();
            deployed.insert(agent, DeploymentState::Native);
            rows.push(SkillRow {
                item_id: None,
                name,
                scope,
                state: Some("native".into()),
                shadowed_by: None,
                deployed,
                native_path: Some(path),
                update_available: false,
                updated_at,
                description: None,
            });
        }
        rows.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.scope.cmp(&b.scope))
        });
    }
    if filter.not_in_agentuna {
        rows.retain(|row| row.item_id.is_none());
    }
    Ok(rows)
}

/// Counts canonical skills in each scope.
///
/// # Errors
/// Returns [`DbError`] when SQLite cannot run the query.
pub fn skill_counts_by_scope(connection: &Connection) -> Result<BTreeMap<Scope, u64>, DbError> {
    let mut counts = Scope::ALL
        .iter()
        .copied()
        .map(|scope| (scope, 0))
        .collect::<BTreeMap<_, _>>();
    let mut stmt = connection
        .prepare("SELECT scope, COUNT(*) FROM items WHERE kind='skill' GROUP BY scope")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (scope, count) = row?;
        let scope = scope
            .parse()
            .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
                field: "items.scope",
                reason: e.to_string(),
            })?;
        counts.insert(scope, count as u64);
    }
    Ok(counts)
}

/// Counts skills matching a filter.
///
/// # Errors
/// Returns [`DbError`] when the list projection query fails.
pub fn skill_counts_by_filter(
    connection: &Connection,
    filter: &SkillFilter,
) -> Result<usize, DbError> {
    Ok(list_skills(connection, filter)?.len())
}

/// Searches skills; name matches rank before tag matches, with alphabetical tie-breaking.
///
/// # Errors
/// Returns [`DbError`] when the list projection query fails.
pub fn search_skills(connection: &Connection, query: &str) -> Result<Vec<SkillRow>, DbError> {
    let mut rows = list_skills(
        connection,
        &SkillFilter {
            text: Some(query.to_owned()),
            ..SkillFilter::default()
        },
    )?;
    let needle = query.to_lowercase();
    rows.sort_by_key(|row| {
        let name = row.name.to_lowercase();
        (name != needle, name)
    });
    Ok(rows)
}
