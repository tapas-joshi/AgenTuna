//! Skills list projections and filters for the Skills screen and CLI.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use std::path::PathBuf;

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
    /// Native folder each agent reads this row from, one entry per native cell.
    ///
    /// Empty for canonical rows. Two agents that share a discovery root (Codex and
    /// omp over `.agents/skills`) carry the same path here, which is what lets the
    /// Skills screen render one folder block instead of independent switches.
    pub native_paths: BTreeMap<AgentId, String>,
    /// Whether the source advertises an update the user has not ignored.
    pub update_available: bool,
    /// Last item update timestamp or native scan timestamp.
    pub updated_at: String,
    /// One-line description parsed from SKILL.md at index time; native-only rows have none.
    pub description: Option<String>,
    /// Folder a manager-installed link resolved to, when the native row is a link.
    pub native_link_target: Option<String>,
    /// What the scan observed about the native entry (`valid`, `invalid`, `broken_link`, ...).
    pub native_state: Option<String>,
    /// Short reason a link is broken or its content refused, when the scan reported one.
    pub native_problem: Option<String>,
}

/// How one native skill is identified across agents.
///
/// Two agents that read the same physical folder — shared discovery roots, or two links into one
/// skill-manager library — must render as one row. The resolved target is that identity when the
/// scan validated one; otherwise the link or folder path is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NativeIdentity {
    /// Canonical folder a validated link resolves to.
    Resolved(PathBuf),
    /// The entry path, for real folders and links that do not resolve.
    Path(PathBuf),
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
    // Materialize once so both shadow lookups can use an automatic slug index
    // instead of rescanning every skill for each item and agent.
    let mut statement = connection.prepare(
        "WITH skills_by_slug AS MATERIALIZED ( \
             SELECT scope, slug, project_id FROM items WHERE kind='skill' \
         ) \
         SELECT i.id, i.name, i.scope, i.state, i.updated_at, \
                (SELECT i2.scope FROM skills_by_slug i2 WHERE i2.slug=i.slug \
                   AND i2.scope IN ('global','user','project','local') \
                   AND i2.scope != i.scope \
                   AND (i2.scope NOT IN ('project','local') OR i2.project_id IS i.project_id) \
                   AND (i2.scope NOT IN ('global','user') OR i2.project_id IS NULL) \
                   AND CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END > \
                       CASE i.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END \
                   ORDER BY CASE i2.scope WHEN 'global' THEN 0 WHEN 'user' THEN 1 WHEN 'project' THEN 2 ELSE 3 END DESC LIMIT 1), \
                (SELECT s.update_available AND (s.ignored_sha IS NULL OR s.ignored_sha != s.remote_sha) \
                   FROM sources s WHERE s.item_id=i.id AND s.deleted_at IS NULL LIMIT 1), \
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
             AND (?3=0 OR EXISTS (SELECT 1 FROM skills_by_slug i2 WHERE i2.slug=i.slug AND i2.scope != i.scope \
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
                native_paths: BTreeMap::new(),
                update_available,
                updated_at,
                description,
                native_link_target: None,
                native_state: None,
                native_problem: None,
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
        // Native discovery stores one row per physical path. When several agents share
        // a Skills root, recover every reader from the path registry rather than
        // presenting the scanner's representative agent as the sole owner.
        let mut roots = BTreeMap::<(Scope, PathBuf), Vec<AgentId>>::new();
        let mut root_statement = connection.prepare(
            "SELECT agent_id, scope, path FROM agent_paths \
             WHERE kind='skills_dir' AND path IS NOT NULL",
        )?;
        let root_rows = root_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for root_row in root_rows {
            let (agent, scope, path) = root_row?;
            let agent = agent
                .parse::<AgentId>()
                .map_err(|error| DbError::InvalidValue {
                    field: "agent_paths.agent_id",
                    reason: error.to_string(),
                })?;
            let scope = scope
                .parse::<Scope>()
                .map_err(|error: at_model::ParseWireError| DbError::InvalidValue {
                    field: "agent_paths.scope",
                    reason: error.to_string(),
                })?;
            roots
                .entry((scope, PathBuf::from(path)))
                .or_default()
                .push(agent);
        }
        for agents in roots.values_mut() {
            agents.sort();
            agents.dedup();
        }
        let mut native = connection.prepare(
            "SELECT n.name, n.scope, n.path, n.last_seen_at, n.agent_id, \
                    n.link_target, n.discovery_state, n.discovery_note \
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
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )?;
        let mut native_indexes = BTreeMap::<(String, Scope, NativeIdentity), usize>::new();
        for row in native_rows {
            let (name, scope, path, updated_at, agent, link_target, discovery_state, note) = row?;
            let scope =
                scope
                    .parse()
                    .map_err(|e: at_model::ParseWireError| DbError::InvalidValue {
                        field: "native_items.scope",
                        reason: e.to_string(),
                    })?;
            let agent = agent
                .parse::<AgentId>()
                .map_err(|e: at_model::AgentIdError| DbError::InvalidValue {
                    field: "native_items.agent_id",
                    reason: e.to_string(),
                })?;
            // A native skill's identity is the folder it actually reads. A manager-installed
            // skill is a link, so claude, codex and omp link to one library folder: grouping by
            // the target shows one row with every agent that loads it, whether the content is
            // readable or was refused. An entry that resolved nothing (a link whose target is
            // missing or escapes the roots) is named by its own path.
            let identity = match link_target.as_deref() {
                Some(target) => NativeIdentity::Resolved(PathBuf::from(target)),
                None => NativeIdentity::Path(PathBuf::from(&path)),
            };
            let native_path = PathBuf::from(&path);
            let key = (at_model::normalized_name(&name), scope, identity);
            let readers = native_path
                .parent()
                .and_then(|root| roots.get(&(scope, root.to_path_buf())))
                .cloned()
                .unwrap_or_else(|| vec![agent.clone()]);
            let index = if let Some(index) = native_indexes.get(&key).copied() {
                // The first row of a group carries the state; a later entry can only add the
                // reason its own link failed, which never contradicts a validated sibling.
                let existing = &mut rows[index];
                if existing.native_problem.is_none() {
                    existing.native_problem = note.clone();
                }
                index
            } else {
                native_indexes.insert(key, rows.len());
                rows.push(SkillRow {
                    item_id: None,
                    name,
                    scope,
                    state: Some("native".into()),
                    shadowed_by: None,
                    deployed: BTreeMap::new(),
                    native_path: Some(path.clone()),
                    native_paths: BTreeMap::new(),
                    update_available: false,
                    updated_at,
                    description: None,
                    native_link_target: link_target,
                    native_state: discovery_state,
                    native_problem: note,
                });
                rows.len() - 1
            };
            for reader in readers {
                rows[index]
                    .deployed
                    .insert(reader.clone(), DeploymentState::Native);
                rows[index].native_paths.insert(reader, path.clone());
            }
        }
        apply_native_shadowing(&mut rows);
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

/// Marks native-only rows a nearer scope overrides for at least one agent.
///
/// A native copy loads only for the agents that read its folder. It is shadowed
/// when a copy of the same skill name at a nearer scope loads for at least one of
/// those agents, so the list can say `Shadowed by Project` instead of counting the
/// unmanaged copy as active. Same-scope copies never shadow each other: precedence
/// is the only shadow rule here; identity and deployment state provide the rest.
fn apply_native_shadowing(rows: &mut [SkillRow]) {
    struct Winner {
        scope: Scope,
        agents: BTreeSet<AgentId>,
    }

    let mut winners = BTreeMap::<String, Vec<Winner>>::new();
    for row in rows.iter() {
        let agents = row
            .deployed
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state,
                    DeploymentState::Linked
                        | DeploymentState::Native
                        | DeploymentState::Conflict
                        | DeploymentState::Drifted
                )
            })
            .map(|(agent, _)| agent.clone())
            .collect::<BTreeSet<_>>();
        if agents.is_empty() {
            continue;
        }
        winners
            .entry(at_model::normalized_name(&row.name))
            .or_default()
            .push(Winner {
                scope: row.scope,
                agents,
            });
    }
    let picks = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.item_id.is_none())
        .filter_map(|(index, row)| {
            let own = row.deployed.keys().cloned().collect::<BTreeSet<_>>();
            let winner = winners
                .get(&at_model::normalized_name(&row.name))?
                .iter()
                .filter(|winner| winner.scope.precedence() > row.scope.precedence())
                .filter(|winner| !winner.agents.is_disjoint(&own))
                .max_by_key(|winner| winner.scope.precedence())?;
            Some((index, winner.scope))
        })
        .collect::<Vec<_>>();
    for (index, scope) in picks {
        rows[index].shadowed_by = Some(scope);
    }
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
