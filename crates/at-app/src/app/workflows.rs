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
        });
    }
    Ok(result)
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
}

/// Loads agents and their configuration paths from the database.
#[tauri::command]
pub fn agents_list(state: State<'_, AppState>) -> Result<Vec<AgentCardView>, CommandError> {
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
        .map(|agent| {
            let paths = at_db::agents::list_agent_paths(database.connection(), &agent.id)
                .map_err(|_| CommandError::internal("could not load agent paths"))?;
            let path_strings: Vec<String> = paths
                .into_iter()
                .filter_map(|path| path.path.map(|p| p.display().to_string()))
                .collect();
            let line = at_engine::health::line_with_connection(
                database.connection(),
                &agent.id,
                home,
                false,
            )
            .map_err(|_| CommandError::internal("could not compute agent health"))?;
            let mode = settings.link_mode(&agent.id);
            Ok(AgentCardView {
                id: agent.id.to_string(),
                name: agent.display_name.unwrap_or_else(|| agent.id.to_string()),
                installed: agent.presence == at_db::agents::AgentPresence::Installed,
                version: agent.version,
                managed: agent.managed,
                paths: path_strings,
                health: line.text,
                link_mode: format!("{mode:?}").to_lowercase(),
            })
        })
        .collect()
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
                id: preset.id,
                name: preset.name,
                scope: preset.scope.to_string(),
                created_at: preset.created_at,
                ratios,
                members: members
                    .into_iter()
                    .map(|member| PresetMemberView {
                        item_id: member.item_id,
                        name: member.item_name.unwrap_or_else(|| "Missing item".into()),
                        kind: member.kind.to_string(),
                        enabled: member.enabled,
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
/// Refuses incomplete application requests until a reviewed deployment plan is supplied.
#[tauri::command]
pub fn preset_apply(
    _state: State<'_, AppState>,
    _preset_id: String,
    _target: String,
) -> Result<(), CommandError> {
    Err(CommandError::unavailable(
        "preset application requires a reviewed deployment plan",
    ))
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
}

/// Saves an instruction file through the engine's managed-block planner and guarded fs pipeline.
#[tauri::command]
pub fn instruction_save(
    state: State<'_, AppState>,
    request: InstructionSaveRequest,
) -> Result<(), CommandError> {
    let item_id = request
        .item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid rule identifier"))?;
    let project = project_path(&state, request.project_id.as_deref())?;
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
    if source.sha256 != request.expected_hash {
        return Err(CommandError::invalid_argument(
            "expected_hash",
            "instruction file changed after it was opened; reload before saving",
        ));
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
    at_engine::rule_editor::save_rule(
        &source,
        request.content.as_bytes(),
        &item_id,
        database.connection(),
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map(|_| ())
    .map_err(|_| {
        CommandError::invalid_argument(
            "content",
            "could not safely save instruction file; reload and review the file before retrying",
        )
    })
}
/// Creates a new native AGENTS.md using the adapter-resolved create-only intent.
#[tauri::command]
pub fn instruction_create_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<String, CommandError> {
    let project = project_path(&state, Some(&project_id))?
        .ok_or_else(|| CommandError::invalid_argument("project_id", "select a project first"))?;
    let paths = state
        .paths()
        .ok_or_else(|| CommandError::unavailable("application paths are unavailable"))?;
    let env = at_adapters::adapter::AgentEnvironment {
        home: paths.home_dir().unwrap_or(Path::new("/")).to_path_buf(),
        project: Some(project),
        variables: Default::default(),
    };
    let registry = at_adapters::adapter::AgentAdapterRegistry::register();
    let intent = at_engine::rule_editor::new_rule(
        at_engine::rule_editor::RuleFileType::Agents,
        at_model::Scope::Project,
        &at_model::AgentId::Codex,
        &registry,
        &env,
        false,
    )
    .map_err(|_| {
        CommandError::invalid_argument(
            "project_id",
            "AGENTS.md already exists or its target is unavailable",
        )
    })?;
    let item_id = at_model::ItemId::new();
    let database = state.database().map_err(CommandError::unavailable)?;
    let support = paths.support_dir();
    let backup = paths.backup_dir();
    let roots = at_fs::changeset::ApplyRoots::new(vec![
        intent.path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        support.to_path_buf(),
    ])
    .map_err(|_| CommandError::unavailable("authorized instruction paths are unavailable"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: support,
        backup_folder: backup,
        recorder: &mut recorder,
    };
    let (source, _) = at_engine::rule_editor::apply_new_rule(
        &intent,
        &item_id,
        database.connection(),
        &mut context,
        &roots,
        &state.recent_writes(),
    )
    .map_err(|_| {
        CommandError::invalid_argument("project_id", "could not create the instruction file")
    })?;
    Ok(source.path.display().to_string())
}
