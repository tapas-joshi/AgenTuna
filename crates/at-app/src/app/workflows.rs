//! Typed command projections for instructions, agents, and presets.
use super::{commands::CommandError, state::AppState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use ts_rs::TS;

/// One rule file available in the Instructions editor.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionFileView {
    /// Opaque rule item identity.
    pub item_id: String,
    /// Scope owning the file.
    pub scope: String,
    /// Filesystem path.
    pub path: String,
    /// Whether it contains AgenTuna's managed block.
    pub managed: bool,
    /// Exact UTF-8 source content.
    pub content: String,
    /// SHA-256 of the opened snapshot.
    pub hash: String,
    /// Consumer agents.
    pub agents: Vec<String>,
    /// Read failure that prevents editing this file, with the failing path.
    pub read_error: Option<String>,
}

/// Lists discovered instruction files without changing them.
#[tauri::command]
pub fn instructions_list(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<InstructionFileView>, CommandError> {
    let project = project_path(&state, project_id.as_deref())?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let env = at_adapters::adapter::AgentEnvironment {
        home: paths.home_dir().unwrap_or(Path::new("/")).to_path_buf(),
        project,
        variables: Default::default(),
    };
    let ids = [
        at_model::AgentId::ClaudeCode,
        at_model::AgentId::Codex,
        at_model::AgentId::Omp,
        at_model::AgentId::Cursor,
    ];
    let registry = at_adapters::adapter::AgentAdapterRegistry::register();
    let scan = at_engine::rules::scan(&registry, &env, &ids, at_model::Scope::ALL);
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut result = Vec::new();
    for source in scan.sources {
        let Ok(content) = String::from_utf8(source.bytes.clone()) else {
            continue;
        };
        let id = at_db::rules_index::upsert_on_scan(
            database.connection(),
            &at_db::rules_index::ScannedRule {
                scope: source.scope,
                project_id: project_id.clone(),
                agent: source
                    .agent_consumers
                    .first()
                    .map_or_else(|| String::from("unknown"), ToString::to_string),
                path: source.path.display().to_string(),
                link_target: source
                    .link_target
                    .as_ref()
                    .map(|path| path.display().to_string()),
                bytes: source.bytes.clone(),
                mtime: source
                    .mtime
                    .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|duration| i64::try_from(duration.as_millis()).ok()),
                managed: source.managed,
            },
        )
        .map_err(|_| CommandError::internal("could not index instruction file"))?;
        result.push(InstructionFileView {
            item_id: id.to_string(),
            scope: source.scope.to_string(),
            path: source.path.display().to_string(),
            managed: source.managed,
            content,
            hash: source.sha256,
            agents: source
                .agent_consumers
                .into_iter()
                .map(|agent| agent.to_string())
                .collect(),
            read_error: None,
        });
    }
    for diagnostic in scan.diagnostics {
        let (path, message) = match diagnostic {
            at_engine::rules::RuleDiagnostic::PermissionDenied { path } => (
                path,
                "Permission denied: the file cannot be read by AgenTuna".to_owned(),
            ),
            at_engine::rules::RuleDiagnostic::BrokenSymlink { path } => (
                path,
                "Broken symlink: the link target does not exist".to_owned(),
            ),
            at_engine::rules::RuleDiagnostic::Io { path, message } => {
                (path, format!("The file could not be read: {message}"))
            }
            // Adapter capability diagnostics describe an agent, not a file to edit.
            at_engine::rules::RuleDiagnostic::Unsupported { .. } => continue,
            _ => continue,
        };
        let scope = rule_scope_for_path(&registry, &env, &ids, &path);
        result.push(InstructionFileView {
            item_id: String::new(),
            scope: scope.map_or_else(|| "user".to_owned(), |scope| scope.to_string()),
            path: path.display().to_string(),
            managed: false,
            content: String::new(),
            hash: String::new(),
            agents: Vec::new(),
            read_error: Some(message),
        });
    }
    Ok(result)
}

/// Finds the adapter-declared scope that owns a rule path, including files inside a rules directory.
fn rule_scope_for_path(
    registry: &at_adapters::adapter::AgentAdapterRegistry,
    env: &at_adapters::adapter::AgentEnvironment,
    ids: &[at_model::AgentId],
    path: &Path,
) -> Option<at_model::Scope> {
    for agent in ids {
        let Ok(Some(adapter)) = registry.get(agent) else {
            continue;
        };
        for scope in at_model::Scope::ALL {
            if adapter
                .paths(at_model::ItemKind::Rule, *scope, env)
                .iter()
                .any(|candidate| path == candidate.path || path.starts_with(&candidate.path))
            {
                return Some(*scope);
            }
        }
    }
    None
}

/// One detected agent summary.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AgentCardView {
    /// Stable agent identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Installation state.
    pub installed: bool,
    /// Detected version, if available.
    pub version: Option<String>,
    /// Whether AgenTuna manages this agent.
    pub managed: bool,
    /// Configuration paths.
    pub paths: Vec<String>,
    /// Health-line summary.
    pub health: String,
    /// Current deployment mode.
    pub link_mode: String,
    /// `installed`, `configured_only` or `not_installed`.
    pub presence: String,
    /// Native format summary derived from recorded configuration paths.
    pub format: String,
    /// Managed skill deployments.
    pub managed_skills: i32,
    /// Managed MCP server deployments.
    pub managed_servers: i32,
    /// Skills present only in the agent folder.
    pub native_skills: i32,
    /// MCP servers present only in the agent folder.
    pub native_servers: i32,
    /// Whether a copy deployment stands in for a requested link.
    pub copy_fallback: bool,
    /// Primary configuration path, when one is recorded.
    pub primary_path: Option<String>,
    /// Whether the primary path still needs a user-chosen replacement.
    pub needs_path: bool,
    /// Deployments whose sync label is `in_sync`.
    pub in_sync: i32,
    /// Total deployment rows for the agent.
    pub total: i32,
    /// Deployments currently materialised as links; switching to copy would migrate these.
    pub linked_deployments: i32,
}

/// Loads agents and their configuration paths from the database.
#[tauri::command]
pub fn agents_list(state: State<'_, AppState>) -> Result<Vec<AgentCardView>, CommandError> {
    agents_list_impl(&state)
}

pub(crate) fn agents_list_impl(state: &AppState) -> Result<Vec<AgentCardView>, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let records = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not load agents"))?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load settings"))?;
    let home = state
        .paths()
        .and_then(|paths| paths.home_dir())
        .unwrap_or(Path::new("/"));
    records
        .into_iter()
        .map(|agent| agent_card(database.connection(), &agent, &settings, home))
        .collect()
}

/// Builds one Agents-screen card from indexed agent, path and deployment rows.
fn agent_card(
    connection: &rusqlite::Connection,
    agent: &at_db::agents::AgentSummary,
    settings: &at_db::settings::Settings,
    home: &Path,
) -> Result<AgentCardView, CommandError> {
    let paths = at_db::agents::list_agent_paths(connection, &agent.id)
        .map_err(|_| CommandError::internal("could not load agent paths"))?;
    let mut formats = paths
        .iter()
        .filter(|path| path.is_primary == Some(true))
        .filter_map(|path| path.format)
        .collect::<Vec<_>>();
    formats.sort_by_key(|format| format.wire_name());
    formats.dedup();
    let path_strings: Vec<String> = paths
        .iter()
        .filter(|path| path.is_primary == Some(true))
        .filter_map(|path| path.path.as_ref().map(|p| p.display().to_string()))
        .collect();
    let primary = paths
        .iter()
        .find(|path| path.is_primary == Some(true))
        .and_then(|path| path.path.as_ref());
    let counts = at_db::deployments::item_kind_counts(connection, &agent.id)
        .map_err(|_| CommandError::internal("could not count agent deployments"))?;
    let health = at_db::deployments::health_counts(connection, &agent.id)
        .map_err(|_| CommandError::internal("could not compute agent health"))?;
    let deployments = at_db::deployments::list_deployments_for_agent(connection, &agent.id)
        .map_err(|_| CommandError::internal("could not read agent deployments"))?;
    // A copy deployment while the agent is configured for links is the recorded symlink fallback;
    // the engine owns that derivation so the CLI and the card cannot disagree.
    let linked_deployments = i32::try_from(
        deployments
            .iter()
            .filter(|cell| matches!(cell.mode.as_deref(), Some("symlink" | "junction")))
            .count(),
    )
    .unwrap_or(i32::MAX);
    let line = at_engine::health::line_with_connection(connection, &agent.id, home)
        .map_err(|_| CommandError::internal("could not compute agent health"))?;
    let copy_fallback = line.copy_fallback;
    let mode = settings.link_mode(&agent.id);
    Ok(AgentCardView {
        id: agent.id.to_string(),
        name: agent
            .display_name
            .clone()
            .unwrap_or_else(|| agent.id.to_string()),
        installed: agent.presence == at_db::agents::AgentPresence::Installed,
        version: agent.version.clone(),
        managed: agent.managed,
        paths: path_strings,
        health: line.text,
        link_mode: format!("{mode:?}").to_lowercase(),
        presence: match agent.presence {
            at_db::agents::AgentPresence::Installed => "installed",
            at_db::agents::AgentPresence::ConfiguredOnly => "configured_only",
            at_db::agents::AgentPresence::NotInstalled => "not_installed",
        }
        .to_owned(),
        format: format_summary(&formats),
        managed_skills: i32::try_from(counts.managed_skills).unwrap_or(i32::MAX),
        managed_servers: i32::try_from(counts.managed_servers).unwrap_or(i32::MAX),
        native_skills: i32::try_from(counts.native_skills).unwrap_or(i32::MAX),
        native_servers: i32::try_from(counts.native_servers).unwrap_or(i32::MAX),
        copy_fallback,
        primary_path: primary.map(|path| path.display().to_string()),
        needs_path: line.diagnostic == Some(at_engine::health::HealthDiagnostic::NeedsPath),
        in_sync: i32::try_from(health.in_sync).unwrap_or(i32::MAX),
        total: i32::try_from(health.total).unwrap_or(i32::MAX),
        linked_deployments,
    })
}

/// Renders the native format summary shown on an agent card.
fn format_summary(formats: &[at_db::agents::AgentPathFormat]) -> String {
    let labels = formats
        .iter()
        .filter(|format| **format != at_db::agents::AgentPathFormat::Dir)
        .map(|format| match format {
            at_db::agents::AgentPathFormat::Json => "JSON",
            at_db::agents::AgentPathFormat::Jsonc => "JSONC",
            at_db::agents::AgentPathFormat::Toml => "TOML",
            at_db::agents::AgentPathFormat::Markdown => "Markdown",
            at_db::agents::AgentPathFormat::Yaml => "YAML",
            at_db::agents::AgentPathFormat::Dir => "Directory",
        })
        .collect::<Vec<_>>();
    if labels.is_empty() {
        if formats.contains(&at_db::agents::AgentPathFormat::Dir) {
            return String::from("MCP not supported");
        }
        return String::from("—");
    }
    let has_skills = formats.contains(&at_db::agents::AgentPathFormat::Dir);
    if has_skills {
        format!("{} + Markdown skills", labels.join(" + "))
    } else {
        labels.join(" + ")
    }
}

/// Runs one detection pass and returns the refreshed agent cards.
#[tauri::command]
pub async fn agents_rescan(state: State<'_, AppState>) -> Result<Vec<AgentCardView>, CommandError> {
    let paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let database = state.database_handle().map_err(CommandError::unavailable)?;
    let environment = at_adapters::adapter::AgentEnvironment {
        home: paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
        project: paths.project_store().map(ToOwned::to_owned),
        variables: Default::default(),
    };
    let registry = std::sync::Arc::new(at_adapters::adapter::AgentAdapterRegistry::register());
    let refresh_paths = paths.clone();
    tokio::task::spawn_blocking(move || {
        let rescan = super::detection::rescan(&registry, &environment, &database);
        // A rescan is the user asking for the truth about agent folders, so the native rows the
        // MCP and Skills screens render are rewritten in the same pass instead of waiting for
        // the next startup rebuild.
        if rescan.is_ok()
            && let Ok(database) = database.lock()
        {
            super::native_scan::refresh_native_findings(&refresh_paths, database.connection());
        }
        rescan
    })
    .await
    .map_err(|_| CommandError::internal("agent detection worker stopped unexpectedly"))?
    .map_err(CommandError::internal)?;
    agents_list(state)
}

/// Records a user-chosen native path for one agent and re-probes it.
#[tauri::command]
pub fn agent_path_set(
    state: State<'_, AppState>,
    agent_id: String,
    path: String,
    kind: Option<String>,
    scope: Option<String>,
) -> Result<AgentCardView, CommandError> {
    let id = agent_id
        .parse::<at_model::AgentId>()
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let kind = match kind.as_deref() {
        None => at_db::agents::AgentPathKind::SkillsDir,
        Some(raw) => at_db::agents::AgentPathKind::parse(raw)
            .ok_or_else(|| CommandError::invalid_argument("kind", "unknown agent path kind"))?,
    };
    let scope = match scope.as_deref() {
        None => at_model::Scope::User,
        Some(raw) => raw
            .parse::<at_model::Scope>()
            .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?,
    };
    let chosen = PathBuf::from(&path);
    if !chosen.is_absolute() {
        return Err(CommandError::invalid_argument(
            "path",
            "choose an absolute path",
        ));
    }
    let metadata = std::fs::metadata(&chosen)
        .map_err(|_| CommandError::invalid_argument("path", "the selected path does not exist"))?;
    if kind == at_db::agents::AgentPathKind::SkillsDir && !metadata.is_dir() {
        return Err(CommandError::invalid_argument(
            "path",
            "the skills location must be a directory",
        ));
    }
    let app_paths = state
        .paths()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let home = app_paths
        .home_dir()
        .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
        .to_path_buf();
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_db::agents::set_path_override(database.connection_mut(), &id, scope, kind, &chosen)
        .map_err(|_| CommandError::internal("could not record the agent path"))?;
    let registry = at_adapters::adapter::AgentAdapterRegistry::register();
    let environment = at_adapters::adapter::AgentEnvironment {
        home: home.clone(),
        project: app_paths.project_store().map(ToOwned::to_owned),
        variables: Default::default(),
    };
    at_engine::agent_paths::resolve_and_record(
        database.connection(),
        &registry,
        &id,
        match kind {
            at_db::agents::AgentPathKind::SkillsDir => at_model::ItemKind::Skill,
            at_db::agents::AgentPathKind::McpFile => at_model::ItemKind::McpServer,
            _ => at_model::ItemKind::Rule,
        },
        scope,
        &environment,
    )
    .map_err(|_| {
        CommandError::invalid_argument(
            "path",
            "the selected path could not be verified for this agent",
        )
    })?;
    let settings = at_db::settings::load(database.connection())
        .map_err(|_| CommandError::internal("could not load settings"))?;
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not load agents"))?;
    let agent = agents
        .into_iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| CommandError::internal("the agent row disappeared"))?;
    agent_card(database.connection(), &agent, &settings, &home)
}

/// Opens the native folder picker for a missing agent's configuration location.
#[tauri::command]
pub async fn agent_path_choose(
    app: tauri::AppHandle,
    title: Option<String>,
) -> Result<Option<String>, CommandError> {
    let title = title.unwrap_or_else(|| String::from("Choose the agent configuration folder"));
    tokio::task::spawn_blocking(move || super::dialogs::choose_folder(&app, &title))
        .await
        .map_err(|_| CommandError::internal("the folder picker stopped unexpectedly"))?
        .map(|selected| selected.map(|path| path.display().to_string()))
        .map_err(|_| CommandError::unavailable("the folder picker is unavailable"))
}

/// Changes whether AgenTuna manages one detected agent.
#[tauri::command]
pub fn agent_manage_set(
    state: State<'_, AppState>,
    agent_id: String,
    managed: bool,
) -> Result<(), CommandError> {
    let id = agent_id
        .parse::<at_model::AgentId>()
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    at_db::agents::set_agent_managed(database.connection(), &id, managed)
        .map(|_| ())
        .map_err(|_| CommandError::internal("could not update agent management"))
}

/// Changes future deployment mode for one agent.
#[tauri::command]
pub fn agent_link_mode_set(
    state: State<'_, AppState>,
    agent_id: String,
    mode: String,
) -> Result<(), CommandError> {
    let id = agent_id
        .parse::<at_model::AgentId>()
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let mode = match mode.as_str() {
        "link" => at_model::LinkMode::Link,
        "copy" => at_model::LinkMode::Copy,
        _ => {
            return Err(CommandError::invalid_argument(
                "mode",
                "expected link or copy",
            ));
        }
    };
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_db::settings::set_link_mode(
        database.connection_mut(),
        &id,
        mode,
        at_db::ops::OperationActor::Ui,
    )
    .map_err(|_| CommandError::internal("could not update deployment mode"))
}

/// Preset row and its current members.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetView {
    /// Stable preset identifier.
    pub id: String,
    /// Name.
    pub name: String,
    /// Scope.
    pub scope: String,
    /// Creation timestamp.
    pub created_at: String,
    /// Per-agent database ratios.
    pub ratios: Vec<PresetRatioView>,
    /// Members.
    pub members: Vec<PresetMemberView>,
    /// Recorded applications, newest first; unapply takes an application id.
    pub applications: Vec<PresetApplicationView>,
    /// Deleted members excluded from every ratio.
    pub missing_members: Vec<String>,
    /// Whether the preset has at least one existing member to apply.
    pub applicable: bool,
}

/// One recorded preset application offered for unapply.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetApplicationView {
    /// Application id used by `preset_unapply`.
    pub id: i64,
    /// `agent` or `project`.
    pub target_kind: String,
    /// Agent id or project id.
    pub target: String,
    /// When the application was recorded.
    pub applied_at: String,
    /// Members the application changed.
    pub member_count: i64,
}
/// A skill or MCP server in a preset.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetMemberView {
    /// Item identifier.
    pub item_id: String,
    /// Item name.
    pub name: String,
    /// Item kind.
    pub kind: String,
    /// Whether the preset enables it.
    pub enabled: bool,
    /// Whether the referenced item still exists.
    pub exists: bool,
}
/// Applied preset member ratio for one target.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetRatioView {
    /// Target label.
    pub target: String,
    /// Enabled existing members.
    pub enabled: i64,
    /// Existing members in the denominator.
    pub total: i64,
    /// Whether every existing member is enabled.
    pub complete: bool,
}

/// Lists preset records and their members.
#[tauri::command]
pub fn presets_list(state: State<'_, AppState>) -> Result<Vec<PresetView>, CommandError> {
    let mut database = state.database().map_err(CommandError::unavailable)?;
    let presets = at_engine::presets::PresetService::new(&mut database)
        .list()
        .map_err(|_| CommandError::internal("could not load presets"))?;
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not load agents"))?;
    presets
        .into_iter()
        .map(|preset| {
            let members = at_db::presets::preset_members(database.connection(), &preset.id)
                .map_err(|_| CommandError::internal("could not load preset members"))?;
            let ratios = agents
                .iter()
                .map(|agent| {
                    let ratio = at_db::presets::applied_ratio(
                        database.connection(),
                        &preset.id,
                        at_db::presets::PresetTargetKind::Agent,
                        &agent.id.to_string(),
                    )
                    .map_err(|_| {
                        CommandError::internal("could not load preset application ratios")
                    })?;
                    Ok(PresetRatioView {
                        target: agent.id.to_string(),
                        enabled: ratio.enabled,
                        total: ratio.total,
                        complete: ratio.complete,
                    })
                })
                .collect::<Result<Vec<_>, CommandError>>()?;
            Ok(PresetView {
                id: preset.id.clone(),
                name: preset.name,
                scope: preset.scope.to_string(),
                created_at: preset.created_at,
                ratios,
                applications: at_db::presets::list_applications(database.connection(), &preset.id)
                    .map_err(|_| CommandError::internal("could not load preset applications"))?
                    .into_iter()
                    .map(|application| PresetApplicationView {
                        id: application.id,
                        target_kind: application.target_kind.as_str().to_owned(),
                        target: application.target_id,
                        applied_at: application.applied_at,
                        member_count: application.member_count,
                    })
                    .collect(),
                missing_members: members
                    .iter()
                    .filter(|member| !member.exists)
                    .map(|member| {
                        member
                            .item_name
                            .clone()
                            .unwrap_or_else(|| member.item_id.clone())
                    })
                    .collect(),
                applicable: members.iter().any(|member| member.exists && member.enabled),
                members: members
                    .into_iter()
                    .map(|member| PresetMemberView {
                        item_id: member.item_id,
                        name: member.item_name.unwrap_or_else(|| "Missing item".into()),
                        kind: member.kind.to_string(),
                        enabled: member.enabled,
                        exists: member.exists,
                    })
                    .collect(),
            })
        })
        .collect()
}
/// Creates a user preset.
#[tauri::command]
pub fn preset_create(
    state: State<'_, AppState>,
    name: String,
    scope: String,
) -> Result<String, CommandError> {
    let scope = scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_engine::presets::PresetService::new(&mut database)
        .create(&name, None, scope, &[])
        .map_err(|_| {
            CommandError::invalid_argument("name", "preset name is invalid or already used")
        })
}
/// Renames a preset.
#[tauri::command]
pub fn preset_rename(
    state: State<'_, AppState>,
    preset_id: String,
    name: String,
) -> Result<(), CommandError> {
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_engine::presets::PresetService::new(&mut database)
        .rename(&preset_id, &name)
        .map(|_| ())
        .map_err(|_| {
            CommandError::invalid_argument("name", "preset name is invalid or already used")
        })
}
/// Deletes a user preset.
#[tauri::command]
pub fn preset_delete(state: State<'_, AppState>, preset_id: String) -> Result<(), CommandError> {
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_engine::presets::PresetService::new(&mut database)
        .delete(&preset_id)
        .map(|_| ())
        .map_err(|_| CommandError::internal("could not delete preset"))
}
/// Enables or disables one preset member.
#[tauri::command]
pub fn preset_member_set(
    state: State<'_, AppState>,
    preset_id: String,
    item_id: String,
    kind: String,
    enabled: bool,
) -> Result<(), CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identifier"))?;
    let kind = kind
        .parse::<at_model::ItemKind>()
        .map_err(|_| CommandError::invalid_argument("kind", "unknown item kind"))?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_engine::presets::PresetService::new(&mut database)
        .set_member(&preset_id, &id, kind, enabled)
        .map_err(|_| CommandError::internal("could not update preset member"))
}
fn project_path(state: &AppState, id: Option<&str>) -> Result<Option<PathBuf>, CommandError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let database = state.database().map_err(CommandError::unavailable)?;
    Ok(Some(PathBuf::from(
        at_db::projects::find_project_by_id(database.connection(), id)
            .map_err(|_| CommandError::internal("could not query project"))?
            .ok_or_else(|| {
                CommandError::invalid_argument("project_id", "project is not registered")
            })?
            .path,
    )))
}
/// Request to save a reviewed editor buffer against its opened file snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionSaveRequest {
    /// Stable item id assigned by the rule index.
    pub item_id: String,
    /// Absolute path shown by the file-list command; verified against a fresh adapter scan.
    pub path: String,
    /// Scope shown by the file-list command.
    pub scope: String,
    /// Hash from the opened snapshot.
    pub expected_hash: String,
    /// Edited full-file buffer.
    pub content: String,
    /// Registered project id when editing Project or Local scope.
    pub project_id: Option<String>,
    /// Whether the user explicitly chose Overwrite after reviewing an external edit.
    #[serde(default)]
    pub force: bool,
}

/// External edit detected between opening and saving one instruction file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionConflictView {
    /// File that changed on disk.
    pub path: String,
    /// Unified diff from the current disk bytes to the editor buffer.
    pub diff: String,
    /// Current on-disk content, offered as the Reload choice.
    pub disk_content: String,
    /// SHA-256 of the current disk content.
    pub disk_hash: String,
    /// Editor buffer that would overwrite the file.
    pub buffer_content: String,
}

/// Result of saving an instruction buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct InstructionSaveOutcome {
    /// `saved`, `unchanged`, `conflict`, or `unreadable`.
    pub status: String,
    /// Conflict details when `status` is `conflict`.
    pub conflict: Option<InstructionConflictView>,
    /// Backup written by the safe write pipeline, when the file was overwritten.
    pub backup_path: Option<String>,
    /// SHA-256 of the content now on disk after a successful save.
    pub hash: Option<String>,
    /// Path that could not be read or written, when `status` is `unreadable`.
    pub unreadable_path: Option<String>,
}

/// Saves an instruction file through the engine's managed-block planner and guarded fs pipeline.
///
/// A save against an externally edited file returns [`InstructionSaveOutcome::conflict`] with the
/// unified diff instead of writing. Repeating the request with `force` after the user chose
/// Overwrite saves the buffer over the current disk bytes through the same backed-up pipeline.
#[tauri::command]
pub fn instruction_save(
    state: State<'_, AppState>,
    request: InstructionSaveRequest,
) -> Result<InstructionSaveOutcome, CommandError> {
    instruction_save_inner(&state, request)
}

/// Body of [`instruction_save`] over injected state, callable from tests.
///
/// # Errors
/// See [`instruction_save`].
pub(crate) fn instruction_save_inner(
    state: &AppState,
    request: InstructionSaveRequest,
) -> Result<InstructionSaveOutcome, CommandError> {
    let item_id = request
        .item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid rule identifier"))?;
    let project = project_path(state, request.project_id.as_deref())?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let env = at_adapters::adapter::AgentEnvironment {
        home: paths.home_dir().unwrap_or(Path::new("/")).to_path_buf(),
        project,
        variables: Default::default(),
    };
    let ids = [
        at_model::AgentId::ClaudeCode,
        at_model::AgentId::Codex,
        at_model::AgentId::Omp,
        at_model::AgentId::Cursor,
    ];
    let registry = at_adapters::adapter::AgentAdapterRegistry::register();
    let scan = at_engine::rules::scan(&registry, &env, &ids, at_model::Scope::ALL);
    let source = scan
        .sources
        .into_iter()
        .find(|source| {
            source.path == Path::new(&request.path) && source.scope.to_string() == request.scope
        })
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "path",
                "instruction file is no longer available; rescan",
            )
        })?;
    if !request.force && source.sha256 != request.expected_hash {
        return Ok(InstructionSaveOutcome {
            status: String::from("conflict"),
            conflict: Some(conflict_view(&source, &request.content)?),
            backup_path: None,
            hash: None,
            unreadable_path: None,
        });
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let support = paths.support_dir();
    let backup = paths.backup_dir();
    let roots = at_fs::changeset::ApplyRoots::new(vec![
        source.path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        support.to_path_buf(),
    ])
    .map_err(|_| CommandError::unavailable("authorized instruction paths are unavailable"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: support,
        backup_folder: backup,
        recorder: &mut recorder,
    };
    match at_engine::rule_editor::save_rule(
        &source,
        request.content.as_bytes(),
        &item_id,
        database.connection(),
        &mut context,
        &roots,
        &state.recent_writes(),
    ) {
        Ok(outcome) => Ok(InstructionSaveOutcome {
            status: if outcome.changed {
                String::from("saved")
            } else {
                String::from("unchanged")
            },
            conflict: None,
            backup_path: outcome.apply.and_then(|apply| {
                at_db::backups::backups_for_operation(database.connection(), &apply.operation_id)
                    .ok()
                    .and_then(|backups| backups.into_iter().find_map(|backup| backup.backup_path))
            }),
            hash: std::fs::read(&source.path)
                .ok()
                .map(|bytes| at_fs::sha256_hex(&bytes)),
            unreadable_path: None,
        }),
        Err(at_engine::rule_editor::RuleEditorError::Conflict { path, diff }) => {
            let mut view = conflict_view(&source, &request.content)?;
            view.path = path.display().to_string();
            view.diff = diff;
            Ok(InstructionSaveOutcome {
                status: String::from("conflict"),
                conflict: Some(view),
                backup_path: None,
                hash: None,
                unreadable_path: None,
            })
        }
        Err(at_engine::rule_editor::RuleEditorError::PermissionDenied { path, .. }) => {
            Ok(InstructionSaveOutcome {
                status: String::from("unreadable"),
                conflict: None,
                backup_path: None,
                hash: None,
                unreadable_path: Some(path.display().to_string()),
            })
        }
        Err(error) => Err(CommandError::new(
            super::commands::CommandErrorCode::Conflict,
            format!("could not safely save {}: {error}", source.path.display()),
        )),
    }
}

/// Builds the reviewed diff between the current disk bytes and the editor buffer.
fn conflict_view(
    source: &at_engine::rules::RuleSource,
    buffer: &str,
) -> Result<InstructionConflictView, CommandError> {
    let disk = std::fs::read(&source.path).map_err(|_| {
        CommandError::new(
            super::commands::CommandErrorCode::Conflict,
            "the file changed on disk and could not be re-read; reload it",
        )
    })?;
    let disk_content = String::from_utf8_lossy(&disk).into_owned();
    Ok(InstructionConflictView {
        path: source.path.display().to_string(),
        diff: similar::TextDiff::from_lines(&disk_content, buffer)
            .unified_diff()
            .to_string(),
        disk_hash: at_fs::sha256_hex(&disk),
        disk_content,
        buffer_content: buffer.to_owned(),
    })
}
/// One indexed item offered for preset membership.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct PresetCandidateView {
    /// Item identifier.
    pub item_id: String,
    /// Item name.
    pub name: String,
    /// `skill` or `mcp_server`.
    pub kind: String,
    /// Scope holding the canonical copy.
    pub scope: String,
}

/// Lists skills and MCP servers that can become preset members for one scope.
#[tauri::command]
pub fn preset_member_candidates(
    state: State<'_, AppState>,
    scope: String,
) -> Result<Vec<PresetCandidateView>, CommandError> {
    let scope = scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("scope", "unknown scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    at_db::items::preset_member_candidates(database.connection(), scope)
        .map(|items| {
            items
                .into_iter()
                .map(|item| PresetCandidateView {
                    item_id: item.id.to_string(),
                    name: item.name,
                    kind: item.kind.to_string(),
                    scope: item.scope.to_string(),
                })
                .collect()
        })
        .map_err(|_| CommandError::internal("could not load preset member candidates"))
}

#[cfg(test)]
mod instruction_drift_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use at_db::deployments::DeploymentSpec;
    use at_model::{AgentId, ItemId, ItemState, Scope};
    use at_store::paths::{AppPaths, PathEnvironment};

    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use crate::app::drift_testkit::{assert_external_edit_detected, assert_no_drift};

    const ORIGINAL: &str = "hand\n<!-- agentuna:begin -->\nold body\n<!-- agentuna:end -->\ntail\n";
    const EDITED: &str = "hand\n<!-- agentuna:begin -->\nnew body\n<!-- agentuna:end -->\ntail\n";
    const EXTERNAL: &str =
        "hand\n<!-- agentuna:begin -->\nexternal body\n<!-- agentuna:end -->\ntail\n";

    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    /// One sandboxed home with a managed rule file, its index row and a deployment row.
    struct RuleSandbox {
        _home: at_fs::testkit::TestHome,
        state: AppState,
        path: PathBuf,
        item_id: ItemId,
        expected_hash: String,
    }

    impl RuleSandbox {
        fn new() -> Self {
            let home = at_fs::testkit::TestHome::new().expect("test home");
            let paths =
                AppPaths::resolve(&PathEnvironment::default().with_home(home.home().to_path_buf()))
                    .expect("app paths");
            let database_path = home.home().join("agentuna").join("index.db");
            std::fs::create_dir_all(database_path.parent().expect("db parent"))
                .expect("database directory");
            std::fs::create_dir_all(paths.support_dir()).expect("support directory");
            std::fs::create_dir_all(paths.backup_dir()).expect("backup directory");
            let database = at_db::Database::open_file(&database_path).expect("database");

            let path = home.home().join(".claude/CLAUDE.md");
            std::fs::create_dir_all(path.parent().expect("rule directory"))
                .expect("rule directory");
            std::fs::write(&path, ORIGINAL).expect("rule file");
            let expected_hash = at_fs::sha256_hex(ORIGINAL.as_bytes());

            let item_id = ItemId::new();
            database
                .connection()
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name, source_path, managed) \
                     VALUES (?1, 'rule', 'user', 'claude', 'CLAUDE.md', ?2, 1)",
                    rusqlite::params![item_id.to_string(), path.to_string_lossy()],
                )
                .expect("rule item");
            let target = path.to_string_lossy().into_owned();
            let spec = DeploymentSpec::applied(
                &item_id,
                &AgentId::ClaudeCode,
                Scope::User,
                ItemState::Linked,
                Some("native"),
                Some(&target),
                at_db::deployments::DeploymentHashes::in_sync(&expected_hash),
            );
            at_db::deployments::upsert_deployment(database.connection(), &spec)
                .expect("deployment row");

            let state = AppState::new(
                Arc::new(JobBridge::new(
                    Arc::new(NullSink),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(paths);

            Self {
                _home: home,
                state,
                path,
                item_id,
                expected_hash,
            }
        }

        fn save_request(&self) -> InstructionSaveRequest {
            InstructionSaveRequest {
                item_id: self.item_id.to_string(),
                path: self.path.display().to_string(),
                scope: Scope::User.to_string(),
                expected_hash: self.expected_hash.clone(),
                content: EDITED.to_owned(),
                project_id: None,
                force: false,
            }
        }

        fn latest_operation_id(&self) -> String {
            let database = self.state.database().expect("database");
            database
                .connection()
                .query_row(
                    "SELECT id FROM operations ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .expect("operation row")
        }
    }

    #[test]
    fn saving_a_rule_file_refreshes_its_drift_baseline() {
        let sandbox = RuleSandbox::new();
        assert_no_drift(&sandbox.state, "before instruction_save");
        let outcome =
            instruction_save_inner(&sandbox.state, sandbox.save_request()).expect("rule save");
        assert_eq!(outcome.status, "saved", "{outcome:?}");
        assert_no_drift(&sandbox.state, "after instruction_save");
        assert_external_edit_detected(&sandbox.state, &sandbox.path, EXTERNAL.as_bytes());
    }

    #[test]
    fn undoing_a_rule_save_refreshes_the_restored_baseline() {
        let sandbox = RuleSandbox::new();
        instruction_save_inner(&sandbox.state, sandbox.save_request()).expect("rule save");
        let operation_id = sandbox.latest_operation_id();
        crate::app::overwrite::operation_undo_inner(&sandbox.state, &operation_id, false)
            .expect("undo");
        assert_no_drift(&sandbox.state, "after undoing the rule save");
        assert_external_edit_detected(&sandbox.state, &sandbox.path, EXTERNAL.as_bytes());
    }
}
