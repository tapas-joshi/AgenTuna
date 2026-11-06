//! Agent registry and configuration paths (TA-040) over the `agents` and
//! `agent_paths` tables created by migration v2 (TF-011).
//!
//! The repository is the only writer for these two tables: detection calls
//! [`upsert_agent`] once per pass and [`upsert_agent_path`] per discovered path,
//! the agents screen reads [`list_agents_on_mac`], and deployment filters with
//! [`managed_agent_ids`]. Because `agents.id` is the primary key, repeating a
//! detection pass over the four built-in agents updates their rows instead of
//! inserting duplicates.
//!
//! `managed` is first-class: [`set_agent_managed`] persists the user's "let
//! AgenTuna manage this agent" choice, and nothing else in the upsert path
//! overwrites it — a later detection pass keeps the opt-out.
//!
//! A path override replaces the agent-native location for one scope and kind:
//! [`set_path_override`] stores it with `verified = 0` and `is_override = 1`,
//! and [`mark_path_probed`] flips `verified` and records whether the path
//! exists. The row is kept (`"exists" = 0`) when the folder disappears so the
//! detection history survives.

use std::path::{Path, PathBuf};

use at_model::{Agent, AgentId, Scope};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::DbError;

/// Every column of `agent_paths`, in [`AgentPath`] order.
const PATH_COLUMNS: &str =
    "id, agent_id, scope, kind, path, format, is_primary, \"exists\", verified, is_override";

/// Columns of `agents` the agents screen reads, followed by the primary path.
const SUMMARY_SQL: &str = "\
SELECT a.id AS id, \
       a.display_name AS display_name, \
       a.version AS version, \
       a.managed AS managed, \
       CASE WHEN a.installed = 1 THEN 0 \
            WHEN EXISTS (SELECT 1 FROM agent_paths p WHERE p.agent_id = a.id) THEN 1 \
            ELSE 2 END AS presence_rank, \
       (SELECT p.path FROM agent_paths p \
         WHERE p.agent_id = a.id AND p.is_primary = 1 AND p.path IS NOT NULL \
         ORDER BY CASE p.scope \
                      WHEN 'local' THEN 0 \
                      WHEN 'project' THEN 1 \
                      WHEN 'user' THEN 2 \
                      WHEN 'global' THEN 3 \
                      ELSE 4 END, \
                  p.is_override DESC, p.id \
         LIMIT 1) AS primary_path \
  FROM agents a \
 ORDER BY presence_rank ASC, COALESCE(a.display_name, a.id) ASC";

/// How an agent is present on this machine.
///
/// The agents screen renders the variants in this order, so an installation is
/// never buried below an agent that is merely configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentPresence {
    /// The agent's executable or application was found.
    Installed,
    /// Not installed, but AgenTuna already stores configuration paths for it.
    ConfiguredOnly,
    /// Neither installed nor configured: offered in the "install" state only.
    NotInstalled,
}

impl AgentPresence {
    /// Rank used in SQL and in ordering; lower ranks render first.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Installed => 0,
            Self::ConfiguredOnly => 1,
            Self::NotInstalled => 2,
        }
    }

    /// Recent wire name, used in logs and diagnostics.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::ConfiguredOnly => "configured_only",
            Self::NotInstalled => "not_installed",
        }
    }

    const fn from_rank(rank: i64) -> Self {
        match rank {
            0 => Self::Installed,
            1 => Self::ConfiguredOnly,
            _ => Self::NotInstalled,
        }
    }
}

/// What a row of `agent_paths` describes.
///
/// The names match the `kind` CHECK constraint in migration v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentPathKind {
    /// A skills folder the agent scans.
    SkillsDir,
    /// The agent's MCP configuration file.
    McpFile,
    /// The agent's instruction or rules file.
    RulesFile,
    /// The agent's own settings file.
    SettingsFile,
    /// A folder AgenTuna owns for the agent.
    ManagedDir,
    /// Anything else an adapter reports.
    Other,
}

impl AgentPathKind {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::SkillsDir,
        Self::McpFile,
        Self::RulesFile,
        Self::SettingsFile,
        Self::ManagedDir,
        Self::Other,
    ];

    /// Value stored in `agent_paths.kind`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::SkillsDir => "skills_dir",
            Self::McpFile => "mcp_file",
            Self::RulesFile => "rules_file",
            Self::SettingsFile => "settings_file",
            Self::ManagedDir => "managed_dir",
            Self::Other => "other",
        }
    }

    /// Parses a stored `kind`; `None` when the value is not part of the CHECK.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.wire_name() == value)
    }
}

/// Format of the file a path points at, matching the `format` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentPathFormat {
    /// Plain JSON.
    Json,
    /// JSON with comments.
    Jsonc,
    /// TOML.
    Toml,
    /// Markdown.
    Markdown,
    /// A directory rather than a file.
    Dir,
    /// YAML.
    Yaml,
}

impl AgentPathFormat {
    /// Every variant, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Json,
        Self::Jsonc,
        Self::Toml,
        Self::Markdown,
        Self::Dir,
        Self::Yaml,
    ];

    /// Value stored in `agent_paths.format`.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Jsonc => "jsonc",
            Self::Toml => "toml",
            Self::Markdown => "markdown",
            Self::Dir => "dir",
            Self::Yaml => "yaml",
        }
    }

    /// Parses a stored `format`; `None` when the value is not part of the CHECK.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|format| format.wire_name() == value)
    }
}

/// One row of `agent_paths`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPath {
    /// Row id.
    pub id: i64,
    ///Agent the path belongs to.
    pub agent_id: AgentId,
    /// Scope the path serves; `None` for rows written before scopes were known.
    pub scope: Option<Scope>,
    /// What the path is.
    pub kind: Option<AgentPathKind>,
    /// Filesystem location; `None` when a row was recorded without one.
    pub path: Option<PathBuf>,
    /// Format of the file at `path`.
    pub format: Option<AgentPathFormat>,
    /// Whether this is the agent's primary location for its scope and kind.
    pub is_primary: Option<bool>,
    /// Whether the path existed at the last probe; `None` when never probed.
    pub exists: Option<bool>,
    /// Whether a probe has run for this row.
    pub verified: bool,
    /// Whether the user replaced the agent-native path with this one.
    pub is_override: bool,
}

/// One agent as the agents screen renders it (TA-040).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    /// Stable identity.
    pub id: AgentId,
    /// Name shown in the UI, when detection reported one.
    pub display_name: Option<String>,
    /// Version detection reported.
    pub version: Option<String>,
    /// Installed, configured-only or not installed; rows arrive in this order.
    pub presence: AgentPresence,
    /// Whether AgenTuna may deploy to this agent.
    pub managed: bool,
    /// The primary path in the nearest scope, when one is recorded.
    pub primary_path: Option<PathBuf>,
}

/// One path detection wants to record.
#[derive(Clone, Debug)]
pub struct AgentPathSpec<'a> {
    /// Agent the path belongs to.
    pub agent: &'a AgentId,
    /// Scope the path serves.
    pub scope: Scope,
    /// What the path is.
    pub kind: AgentPathKind,
    /// Absolute location on disk.
    pub path: &'a Path,
    /// Format of the file, when known.
    pub format: Option<AgentPathFormat>,
    /// Whether this is the agent's primary location for its scope and kind.
    pub is_primary: bool,
    /// Whether the path existed when detection probed it.
    pub exists: bool,
}

/// One candidate path probe, including whether it passed the usability checks.
#[derive(Clone, Debug)]
pub struct AgentPathProbeSpec<'a> {
    /// Agent the path belongs to.
    pub agent: &'a AgentId,
    /// Scope the path serves.
    pub scope: Scope,
    /// What the path is.
    pub kind: AgentPathKind,
    /// Candidate filesystem location.
    pub path: &'a Path,
    /// Format of the file, when known.
    pub format: Option<AgentPathFormat>,
    /// Whether this is the adapter's preferred candidate.
    pub is_primary: bool,
    /// Whether stat found the candidate.
    pub exists: bool,
    /// Whether the candidate passed stat, expected-kind and read-access checks.
    pub verified: bool,
    /// Whether this candidate is a user-saved override.
    pub is_override: bool,
}

/// Records one detection result for `agent`.
///
/// Idempotent by `agents.id`: the row is inserted once and later passes update
/// `display_name`, `version`, `installed` and `detected_at` in place. The
/// `managed` flag, `kind` and `settings_json` are never overwritten here, so a
/// user's opt-out survives a rescan. A missing row is created with the schema
/// default (`managed = 1`).
///
/// # Errors
/// A SQLite failure, including a row that violates the `agents` constraints.
pub fn upsert_agent(connection: &Connection, agent: &Agent) -> Result<(), DbError> {
    connection.execute(
        "INSERT INTO agents (id, display_name, version, installed, detected_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(id) DO UPDATE SET \
             display_name = excluded.display_name, \
             version = excluded.version, \
             installed = excluded.installed, \
             detected_at = excluded.detected_at",
        params![
            agent.id.to_string(),
            agent.display_name,
            agent.version,
            i64::from(agent.detected),
        ],
    )?;
    Ok(())
}

/// Sets the user's "let AgenTuna manage this agent" choice.
///
/// Returns whether an agent row was updated; `false` means the agent was never
/// detected, so there is no deployment target either.
///
/// # Errors
/// A SQLite failure.
pub fn set_agent_managed(
    connection: &Connection,
    agent: &AgentId,
    managed: bool,
) -> Result<bool, DbError> {
    let changed = connection.execute(
        "UPDATE agents SET managed = ?2 WHERE id = ?1",
        params![agent.to_string(), i64::from(managed)],
    )?;
    Ok(changed == 1)
}

/// Whether AgenTuna may deploy to `agent`.
///
/// An agent with no row reads as `false`: deployment and detection only handle
/// agents that were detected at least once.
///
/// # Errors
/// A SQLite failure.
pub fn is_agent_managed(connection: &Connection, agent: &AgentId) -> Result<bool, DbError> {
    let managed: Option<i64> = connection
        .query_row(
            "SELECT managed FROM agents WHERE id = ?1",
            [agent.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(managed.is_some_and(|value| value != 0))
}

/// Ids of the agents deployment may touch, in the order the agents screen shows
/// them. Unmanaged agents are skipped.
///
/// # Errors
/// A SQLite failure.
pub fn managed_agent_ids(connection: &Connection) -> Result<Vec<AgentId>, DbError> {
    list_agents_on_mac(connection).map(|agents| {
        agents
            .into_iter()
            .filter(|agent| agent.managed)
            .map(|agent| agent.id)
            .collect()
    })
}

/// Every agent AgenTuna knows about, ordered for the agents screen.
///
/// Installed agents come first, then agents that are only configured, then
/// agents that are neither; within a group the display name (or id) orders the
/// rows. Each row carries the primary path in the nearest scope
/// (Local > Project > User > Global) and the persisted `managed` flag.
///
/// The name is macOS-specific because that is the target platform, but the
/// query itself is platform-neutral and the CLI uses it as well.
///
/// # Errors
/// A SQLite failure, or a stored agent id that is not a valid [`AgentId`].
pub fn list_agents_on_mac(connection: &Connection) -> Result<Vec<AgentSummary>, DbError> {
    let mut statement = connection.prepare(SUMMARY_SQL)?;
    let rows = statement.query_map([], summary_from_row)?;
    let mut agents = Vec::new();
    for row in rows {
        agents.push(row?);
    }
    Ok(agents)
}

/// Every path recorded for `agent`, ordered by scope precedence and path id.
///
/// # Errors
/// A SQLite failure, or a stored value outside the schema's CHECK constraints.
pub fn list_agent_paths(
    connection: &Connection,
    agent: &AgentId,
) -> Result<Vec<AgentPath>, DbError> {
    let sql = format!(
        "SELECT {PATH_COLUMNS} FROM agent_paths WHERE agent_id = ?1 \
         ORDER BY CASE scope \
                      WHEN 'local' THEN 0 \
                      WHEN 'project' THEN 1 \
                      WHEN 'user' THEN 2 \
                      WHEN 'global' THEN 3 \
                      ELSE 4 END, id"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([agent.to_string()], path_from_row)?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row?);
    }
    Ok(paths)
}

/// Records a path detection found: insert, or refresh the row with the same
/// agent, scope, kind and location.
///
/// A refreshed row keeps `is_override`: detection never demotes a user override
/// to a plain native path. The row is marked `verified` because detection just
/// probed it.
///
/// # Errors
/// A SQLite failure, or a `path` that cannot be stored as text.
pub fn upsert_agent_path(
    connection: &Connection,
    spec: &AgentPathSpec<'_>,
) -> Result<AgentPath, DbError> {
    let sql = format!(
        "INSERT INTO agent_paths \
             (agent_id, scope, kind, path, format, is_primary, \"exists\", verified, is_override) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 0) \
         ON CONFLICT(agent_id, scope, kind, path) DO UPDATE SET \
             format = excluded.format, \
             is_primary = excluded.is_primary, \
             \"exists\" = excluded.\"exists\", \
             verified = 1 \
         RETURNING {PATH_COLUMNS}"
    );
    connection
        .query_row(
            &sql,
            params![
                spec.agent.to_string(),
                spec.scope.wire_name(),
                spec.kind.wire_name(),
                path_text(spec.path)?,
                spec.format.map(AgentPathFormat::wire_name),
                i64::from(spec.is_primary),
                i64::from(spec.exists),
            ],
            path_from_row,
        )
        .map_err(DbError::from)
}

/// Inserts or refreshes a path candidate with its actual usability state.
///
/// Unlike [`upsert_agent_path`], this records `verified = false` for missing,
/// wrong-kind or unreadable candidates. A persisted user override remains an override.
///
/// # Errors
/// A SQLite failure, or a `path` that cannot be stored as text.
pub fn record_agent_path_probe(
    connection: &Connection,
    spec: &AgentPathProbeSpec<'_>,
) -> Result<AgentPath, DbError> {
    let sql = format!(
        "INSERT INTO agent_paths \
             (agent_id, scope, kind, path, format, is_primary, \"exists\", verified, is_override) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(agent_id, scope, kind, path) DO UPDATE SET \
             format = excluded.format, \
             is_primary = excluded.is_primary, \
             \"exists\" = excluded.\"exists\", \
             verified = excluded.verified, \
             is_override = CASE WHEN agent_paths.is_override = 1 THEN 1 ELSE excluded.is_override END \
         RETURNING {PATH_COLUMNS}"
    );
    connection
        .query_row(
            &sql,
            params![
                spec.agent.to_string(),
                spec.scope.wire_name(),
                spec.kind.wire_name(),
                path_text(spec.path)?,
                spec.format.map(AgentPathFormat::wire_name),
                i64::from(spec.is_primary),
                i64::from(spec.exists),
                i64::from(spec.verified),
                i64::from(spec.is_override),
            ],
            path_from_row,
        )
        .map_err(DbError::from)
}

/// Replaces the agent-native location for one scope and kind with a user path.
///
/// The override becomes the primary row for its scope and kind (other rows there
/// stop being primary) and is stored with `verified = false` and
/// `is_override = true` until [`mark_path_probed`] runs. Repeating the call with
/// the same path resets `verified` and `exists`, so a moved folder is re-probed
/// instead of shown as valid.
///
/// # Errors
/// A SQLite failure, or a `path` that cannot be stored as text.
pub fn set_path_override(
    connection: &mut Connection,
    agent: &AgentId,
    scope: Scope,
    kind: AgentPathKind,
    path: &Path,
) -> Result<AgentPath, DbError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE agent_paths SET is_primary = 0 \
         WHERE agent_id = ?1 AND scope = ?2 AND kind = ?3",
        params![agent.to_string(), scope.wire_name(), kind.wire_name()],
    )?;
    let sql = format!(
        "INSERT INTO agent_paths \
             (agent_id, scope, kind, path, is_primary, \"exists\", verified, is_override) \
         VALUES (?1, ?2, ?3, ?4, 1, 0, 0, 1) \
         ON CONFLICT(agent_id, scope, kind, path) DO UPDATE SET \
             is_primary = 1, \
             verified = 0, \
             is_override = 1 \
         RETURNING {PATH_COLUMNS}"
    );
    let stored = transaction.query_row(
        &sql,
        params![
            agent.to_string(),
            scope.wire_name(),
            kind.wire_name(),
            path_text(path)?,
        ],
        path_from_row,
    )?;
    transaction.commit()?;
    Ok(stored)
}

/// Records the outcome of probing a path.
///
/// `verified` becomes true and `"exists"` records the result; `is_override` is
/// left as it was. Returns `None` when no row has `path_id`.
///
/// # Errors
/// A SQLite failure, or stored values outside the schema's CHECK constraints.
pub fn mark_path_probed(
    connection: &Connection,
    path_id: i64,
    exists: bool,
) -> Result<Option<AgentPath>, DbError> {
    let sql = format!(
        "UPDATE agent_paths SET verified = 1, \"exists\" = ?2 WHERE id = ?1 \
         RETURNING {PATH_COLUMNS}"
    );
    connection
        .query_row(&sql, params![path_id, i64::from(exists)], path_from_row)
        .optional()
        .map_err(DbError::from)
}

/// The per-agent settings blob stored in `agents.settings_json`.
///
/// Settings that apply to every agent live in the `settings` table
/// ([`crate::settings`]); this column holds agent-specific values an adapter
/// reports, and is `None` when detection stored none.
///
/// # Errors
/// A SQLite failure, or a stored blob that is not valid UTF-8 text.
pub fn agent_settings_json(
    connection: &Connection,
    agent: &AgentId,
) -> Result<Option<String>, DbError> {
    connection
        .query_row(
            "SELECT settings_json FROM agents WHERE id = ?1",
            [agent.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(DbError::from)
        .map(Option::flatten)
}

/// Replaces the per-agent settings blob. `None` clears the column.
///
/// The value must be a JSON object; an invalid document is rejected before
/// anything is written.
///
/// # Errors
/// [`DbError::Json`] when the blob is not a JSON object, or a SQLite failure.
/// Returns `false` when no agent row has `agent`.
pub fn set_agent_settings_json(
    connection: &Connection,
    agent: &AgentId,
    settings_json: Option<&str>,
) -> Result<bool, DbError> {
    if let Some(document) = settings_json {
        let _object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(document)?;
    }
    let changed = connection.execute(
        "UPDATE agents SET settings_json = ?2 WHERE id = ?1",
        params![agent.to_string(), settings_json],
    )?;
    Ok(changed == 1)
}

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<AgentSummary> {
    let id: String = row.get(0)?;
    let rank: i64 = row.get(4)?;
    Ok(AgentSummary {
        id: parse_agent_id(id, 0)?,
        display_name: row.get(1)?,
        version: row.get(2)?,
        managed: row.get::<_, i64>(3)? != 0,
        presence: AgentPresence::from_rank(rank),
        primary_path: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
    })
}

fn path_from_row(row: &Row<'_>) -> rusqlite::Result<AgentPath> {
    let agent_id: String = row.get(1)?;
    Ok(AgentPath {
        id: row.get(0)?,
        agent_id: parse_agent_id(agent_id, 1)?,
        scope: parse_text(row, 2, parse_scope)?,
        kind: parse_text(row, 3, AgentPathKind::parse)?,
        path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
        format: parse_text(row, 5, AgentPathFormat::parse)?,
        is_primary: parse_bool(row, 6)?,
        exists: parse_bool(row, 7)?,
        verified: row.get::<_, i64>(8)? != 0,
        is_override: row.get::<_, i64>(9)? != 0,
    })
}

fn parse_scope(value: &str) -> Option<Scope> {
    Scope::ALL
        .iter()
        .copied()
        .find(|scope| scope.wire_name() == value)
}

fn parse_agent_id(value: String, index: usize) -> rusqlite::Result<AgentId> {
    value.parse::<AgentId>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

/// Reads a nullable text column that must map to `parse`.
fn parse_text<T>(
    row: &Row<'_>,
    index: usize,
    parse: fn(&str) -> Option<T>,
) -> rusqlite::Result<Option<T>> {
    let stored: Option<String> = row.get(index)?;
    match stored {
        None => Ok(None),
        Some(value) => parse(&value).map(Some).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                Type::Text,
                Box::new(InvalidStoredText {
                    value: value.clone(),
                }),
            )
        }),
    }
}

fn parse_bool(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<bool>> {
    row.get::<_, Option<i64>>(index)
        .map(|value| value.map(|flag| flag != 0))
}

fn path_text(path: &Path) -> Result<String, DbError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| DbError::PathUnresolved {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "path is not valid UTF-8"),
        })
}

/// Error for a stored value outside the schema's CHECK constraints.
#[derive(Debug, thiserror::Error)]
#[error("stored value `{value}` is not one of the allowed names")]
struct InvalidStoredText {
    value: String,
}
