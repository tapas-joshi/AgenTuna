//! Connect agent / Set path dialog backend (TA-059).
//!
//! The dialog reads the persisted candidate probes from TA-032, validates a typed
//! path on change without writing, browses with the native folder picker and saves
//! through the existing [`super::workflows::agent_path_set`] command.

use at_model::Scope;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::AppState;
use super::commands::CommandError;

/// One recorded candidate path for an agent, with its probe results.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AgentPathCandidateView {
    /// Absolute native path as recorded by detection.
    pub path: String,
    /// Scope the candidate belongs to.
    pub scope: String,
    /// Path kind: `skills_dir`, `mcp_file` or `rules_file`.
    pub kind: String,
    /// Native document format, when the path is a file.
    pub format: Option<String>,
    /// Whether this is the adapter's preferred candidate.
    pub is_primary: bool,
    /// Whether the path exists on disk.
    pub exists: bool,
    /// Whether the path exists and has the expected shape.
    pub verified: bool,
    /// Whether a user chose this path explicitly.
    pub is_override: bool,
}

/// Result of validating one typed or browsed path against the agent's expectations.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AgentPathValidation {
    /// Whether the path exists at all.
    pub exists: bool,
    /// Whether the path is a directory (required for a skills location).
    pub is_directory: bool,
    /// Soft warning when the folder has no skills folder or config file.
    pub structure_warning: Option<String>,
    /// Hard reason that keeps Save disabled, when the path is invalid.
    pub reason: Option<String>,
}

/// Request payload to validate or register a custom agent (TA-060).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CustomAgentSpecRequest {
    /// Display name shown in UI (e.g. "Windsurf").
    pub display_name: String,
    /// Absolute root folder path.
    pub root: String,
    /// Subfolder within root for skills (defaults to "skills").
    pub skills_subfolder: Option<String>,
    /// Path to MCP configuration file (relative to root or absolute).
    pub mcp_file: Option<String>,
    /// Format of MCP file: "json", "toml", or "none".
    pub mcp_format: String,
    /// Comma-separated or list of rule file names (e.g. "AGENTS.md").
    pub rule_files: Vec<String>,
    /// Whether this agent supports symlinking skills.
    pub supports_symlink: bool,
}

/// Resolved path preview and validation for a custom agent (TA-060).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CustomAgentPreviewView {
    /// Resolved absolute path to skills directory.
    pub skills_path: Option<String>,
    /// Resolved absolute path to MCP config file, if supported.
    pub mcp_path: Option<String>,
    /// Resolved absolute paths to rule files.
    pub rule_paths: Vec<String>,
    /// Whether the specification is valid.
    pub valid: bool,
    /// Specific validation error message if invalid.
    pub reason: Option<String>,
    /// ID of existing agent if duplicate name was detected.
    pub existing_agent_id: Option<String>,
}

/// Lists every recorded candidate path for one agent with its probe markers.
#[tauri::command]
pub fn agent_path_candidates(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<Vec<AgentPathCandidateView>, CommandError> {
    agent_path_candidates_impl(&state, &agent_id)
}

pub(crate) fn agent_path_candidates_impl(
    state: &AppState,
    agent_id: &str,
) -> Result<Vec<AgentPathCandidateView>, CommandError> {
    let id = agent_id
        .parse::<at_model::AgentId>()
        .map_err(|_| CommandError::invalid_argument("agent_id", "unknown agent"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let paths = at_db::agents::list_agent_paths(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not load agent path candidates"))?;
    Ok(paths
        .into_iter()
        .map(|path| AgentPathCandidateView {
            path: path
                .path
                .as_ref()
                .map(|value| value.display().to_string())
                .unwrap_or_default(),
            scope: path
                .scope
                .map(|scope| scope.wire_name().to_owned())
                .unwrap_or_else(|| String::from("user")),
            kind: path
                .kind
                .map(|kind| kind.wire_name().to_owned())
                .unwrap_or_else(|| String::from("skills_dir")),
            format: path.format.map(|format| format.wire_name().to_owned()),
            is_primary: path.is_primary.unwrap_or(false),
            exists: path.exists.unwrap_or(false),
            verified: path.verified,
            is_override: path.is_override,
        })
        .filter(|candidate| !candidate.path.is_empty())
        .collect())
}

/// Validates one candidate path without persisting anything.
///
/// A missing path or a non-directory for a skills location is a hard reason that keeps
/// Save disabled. An existing directory without any skills folder or config file yields
/// only a soft warning; the user may still point AgenTuna at it.
#[tauri::command]
pub fn agent_path_validate(
    state: State<'_, AppState>,
    agent_id: String,
    path: String,
) -> Result<AgentPathValidation, CommandError> {
    agent_path_validate_impl(&state, &agent_id, &path)
}

pub(crate) fn agent_path_validate_impl(
    state: &AppState,
    agent_id: &str,
    path: &str,
) -> Result<AgentPathValidation, CommandError> {
    let _ = agent_id;
    let _ = state;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(AgentPathValidation {
            exists: false,
            is_directory: false,
            structure_warning: None,
            reason: Some(String::from("Choose a folder to continue")),
        });
    }
    if !Path::new(trimmed).is_absolute() {
        return Ok(AgentPathValidation {
            exists: false,
            is_directory: false,
            structure_warning: None,
            reason: Some(String::from("Enter an absolute path")),
        });
    }
    let metadata = match std::fs::symlink_metadata(trimmed) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok(AgentPathValidation {
                exists: false,
                is_directory: false,
                structure_warning: None,
                reason: Some(String::from("This path does not exist")),
            });
        }
    };
    if metadata.is_symlink() {
        return Ok(AgentPathValidation {
            exists: true,
            is_directory: false,
            structure_warning: None,
            reason: Some(String::from("Choose the real folder, not a symlink to it")),
        });
    }
    if !metadata.is_dir() {
        return Ok(AgentPathValidation {
            exists: true,
            is_directory: false,
            structure_warning: None,
            reason: Some(String::from("The skills location must be a directory")),
        });
    }
    let looks_like_agent = looks_like_agent_folder(Path::new(trimmed));
    Ok(AgentPathValidation {
        exists: true,
        is_directory: true,
        structure_warning: (!looks_like_agent)
            .then(|| String::from("No skills folder or config file found here")),
        reason: None,
    })
}

/// Heuristic the dialog uses for the soft structure warning.
fn looks_like_agent_folder(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut saw_any_entry = false;
    for entry in entries.flatten() {
        saw_any_entry = true;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name == "skills" && entry.path().is_dir() {
            return true;
        }
        if name.ends_with(".json") || name.ends_with(".jsonc") || name.ends_with(".toml") {
            return true;
        }
    }
    saw_any_entry
}

/// Previews the resolved paths and validates a custom agent specification (TA-060).
#[tauri::command]
pub fn custom_agent_preview(
    state: State<'_, AppState>,
    request: CustomAgentSpecRequest,
) -> Result<CustomAgentPreviewView, CommandError> {
    custom_agent_preview_impl(&state, request)
}

pub(crate) fn custom_agent_preview_impl(
    state: &AppState,
    request: CustomAgentSpecRequest,
) -> Result<CustomAgentPreviewView, CommandError> {
    let trimmed_name = request.display_name.trim();
    if trimmed_name.is_empty() {
        return Ok(CustomAgentPreviewView {
            skills_path: None,
            mcp_path: None,
            rule_paths: Vec::new(),
            valid: false,
            reason: Some(String::from("Enter a display name")),
            existing_agent_id: None,
        });
    }

    if let Ok(database) = state.database()
        && let Ok(agents) = at_db::agents::list_agents_on_mac(database.connection())
        && let Some(existing) = agents.iter().find(|a| {
            a.display_name
                .as_deref()
                .unwrap_or("")
                .eq_ignore_ascii_case(trimmed_name)
        })
    {
        return Ok(CustomAgentPreviewView {
            skills_path: None,
            mcp_path: None,
            rule_paths: Vec::new(),
            valid: false,
            reason: Some(String::from("An agent with this name exists")),
            existing_agent_id: Some(existing.id.to_string()),
        });
    }

    let root_str = request.root.trim();
    if root_str.is_empty() {
        return Ok(CustomAgentPreviewView {
            skills_path: None,
            mcp_path: None,
            rule_paths: Vec::new(),
            valid: false,
            reason: Some(String::from("Choose a root folder")),
            existing_agent_id: None,
        });
    }

    let root = Path::new(root_str);
    if !root.is_absolute() || root_str == "/" {
        return Ok(CustomAgentPreviewView {
            skills_path: None,
            mcp_path: None,
            rule_paths: Vec::new(),
            valid: false,
            reason: Some(String::from("Relative or '/' root is not allowed")),
            existing_agent_id: None,
        });
    }

    let skills_sub = request.skills_subfolder.as_deref().unwrap_or("").trim();
    let skills_sub = if skills_sub.is_empty() {
        "skills"
    } else {
        skills_sub
    };
    let skills_path = root.join(skills_sub);

    let mcp_format = request.mcp_format.trim().to_lowercase();
    let mcp_path = if mcp_format == "none" {
        None
    } else {
        let mcp_file = request.mcp_file.as_deref().unwrap_or("").trim();
        if mcp_file.is_empty() {
            return Ok(CustomAgentPreviewView {
                skills_path: Some(skills_path.display().to_string()),
                mcp_path: None,
                rule_paths: Vec::new(),
                valid: false,
                reason: Some(String::from(
                    "Enter an MCP file path or select format 'none'",
                )),
                existing_agent_id: None,
            });
        }
        let p = Path::new(mcp_file);
        if p.is_absolute() {
            Some(p.display().to_string())
        } else {
            Some(root.join(p).display().to_string())
        }
    };

    let rule_paths = request
        .rule_files
        .iter()
        .map(|r| {
            let p = Path::new(r.trim());
            if p.is_absolute() {
                p.display().to_string()
            } else {
                root.join(p).display().to_string()
            }
        })
        .collect();

    Ok(CustomAgentPreviewView {
        skills_path: Some(skills_path.display().to_string()),
        mcp_path,
        rule_paths,
        valid: true,
        reason: None,
        existing_agent_id: None,
    })
}

/// Creates a new custom agent, registers its paths and returns its card (TA-060).
#[tauri::command]
pub fn custom_agent_create(
    state: State<'_, AppState>,
    request: CustomAgentSpecRequest,
) -> Result<super::workflows::AgentCardView, CommandError> {
    custom_agent_create_impl(&state, request)
}

pub(crate) fn custom_agent_create_impl(
    state: &AppState,
    request: CustomAgentSpecRequest,
) -> Result<super::workflows::AgentCardView, CommandError> {
    let preview = custom_agent_preview_impl(state, request.clone())?;
    if !preview.valid {
        let reason = preview
            .reason
            .unwrap_or_else(|| String::from("Invalid custom agent specification"));
        return Err(CommandError::invalid_argument("request", &reason));
    }

    let slug: String = request
        .display_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    let slug = if slug.is_empty() {
        String::from("custom-agent")
    } else {
        slug
    };
    let agent_id = at_model::AgentId::Custom(slug);

    {
        let database = state.database().map_err(CommandError::unavailable)?;
        let conn = database.connection();
        at_db::agents::upsert_agent(
            conn,
            &at_model::Agent {
                id: agent_id.clone(),
                display_name: request.display_name.clone(),
                detected: true,
                version: None,
            },
        )
        .map_err(|e| CommandError::internal(format!("could not create agent: {e}")))?;
        at_db::agents::set_agent_managed(conn, &agent_id, true)
            .map_err(|e| CommandError::internal(format!("could not set agent managed: {e}")))?;

        if let Some(skills_path) = preview.skills_path {
            let path = PathBuf::from(&skills_path);
            let exists = path.exists();
            at_db::agents::upsert_agent_path(
                conn,
                &at_db::agents::AgentPathSpec {
                    agent: &agent_id,
                    scope: Scope::User,
                    kind: at_db::agents::AgentPathKind::SkillsDir,
                    path: &path,
                    format: Some(at_db::agents::AgentPathFormat::Dir),
                    is_primary: true,
                    exists,
                },
            )
            .map_err(|e| CommandError::internal(format!("could not set skills path: {e}")))?;
        }

        if let Some(mcp_path) = preview.mcp_path {
            let path = PathBuf::from(&mcp_path);
            let exists = path.exists();
            let fmt = match request.mcp_format.trim().to_lowercase().as_str() {
                "toml" => at_db::agents::AgentPathFormat::Toml,
                _ => at_db::agents::AgentPathFormat::Json,
            };
            at_db::agents::upsert_agent_path(
                conn,
                &at_db::agents::AgentPathSpec {
                    agent: &agent_id,
                    scope: Scope::User,
                    kind: at_db::agents::AgentPathKind::McpFile,
                    path: &path,
                    format: Some(fmt),
                    is_primary: true,
                    exists,
                },
            )
            .map_err(|e| CommandError::internal(format!("could not set mcp path: {e}")))?;
        }

        for rule_path_str in preview.rule_paths {
            let path = PathBuf::from(&rule_path_str);
            at_db::agents::upsert_agent_path(
                conn,
                &at_db::agents::AgentPathSpec {
                    agent: &agent_id,
                    scope: Scope::User,
                    kind: at_db::agents::AgentPathKind::RulesFile,
                    path: &path,
                    format: Some(at_db::agents::AgentPathFormat::Markdown),
                    is_primary: false,
                    exists: path.exists(),
                },
            )
            .map_err(|e| CommandError::internal(format!("could not set rule path: {e}")))?;
        }
    }

    let agents = super::workflows::agents_list_impl(state)?;
    agents
        .into_iter()
        .find(|a| a.id == agent_id.to_string())
        .ok_or_else(|| CommandError::internal("created custom agent was not found in registry"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use std::sync::Arc;

    struct NullSink;
    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    fn test_state() -> AppState {
        AppState::new(
            Arc::new(JobBridge::new(
                Arc::new(NullSink),
                Arc::new(TracingOperationsLog),
            )),
            None,
        )
    }
    #[test]
    fn validate_rejects_empty_and_relative() {
        let state = test_state();

        let empty = agent_path_validate_impl(&state, "cursor", "").expect("validate");
        assert!(!empty.exists);
        assert_eq!(empty.reason.as_deref(), Some("Choose a folder to continue"));

        let relative =
            agent_path_validate_impl(&state, "cursor", "some/rel/path").expect("validate");
        assert!(!relative.exists);
        assert_eq!(relative.reason.as_deref(), Some("Enter an absolute path"));
    }

    #[test]
    fn validate_missing_and_directory_heuristics() {
        let state = test_state();
        let temp = tempfile::tempdir().expect("tempdir");

        let missing = agent_path_validate_impl(
            &state,
            "cursor",
            &temp.path().join("nonexistent").display().to_string(),
        )
        .expect("validate");
        assert!(!missing.exists);
        assert_eq!(missing.reason.as_deref(), Some("This path does not exist"));

        let empty_dir = temp.path().join("empty-folder");
        std::fs::create_dir_all(&empty_dir).expect("mkdir");
        let validated_empty =
            agent_path_validate_impl(&state, "cursor", &empty_dir.display().to_string())
                .expect("validate");
        assert!(validated_empty.exists);
        assert!(validated_empty.is_directory);
        assert_eq!(
            validated_empty.structure_warning.as_deref(),
            Some("No skills folder or config file found here")
        );
        assert!(validated_empty.reason.is_none());

        let with_skills = temp.path().join("cursor-folder");
        std::fs::create_dir_all(with_skills.join("skills")).expect("mkdir skills");
        let validated_skills =
            agent_path_validate_impl(&state, "cursor", &with_skills.display().to_string())
                .expect("validate");
        assert!(validated_skills.exists);
        assert!(validated_skills.is_directory);
        assert!(validated_skills.structure_warning.is_none());
        assert!(validated_skills.reason.is_none());
    }
    #[test]
    fn custom_agent_preview_rejects_relative_or_root_folder() {
        let state = test_state();

        let rel = custom_agent_preview_impl(
            &state,
            CustomAgentSpecRequest {
                display_name: "Test Agent".to_owned(),
                root: "relative/path".to_owned(),
                skills_subfolder: None,
                mcp_file: None,
                mcp_format: "none".to_owned(),
                rule_files: vec![],
                supports_symlink: true,
            },
        )
        .expect("preview");
        assert!(!rel.valid);
        assert_eq!(
            rel.reason.as_deref(),
            Some("Relative or '/' root is not allowed")
        );

        let root_slash = custom_agent_preview_impl(
            &state,
            CustomAgentSpecRequest {
                display_name: "Test Agent".to_owned(),
                root: "/".to_owned(),
                skills_subfolder: None,
                mcp_file: None,
                mcp_format: "none".to_owned(),
                rule_files: vec![],
                supports_symlink: true,
            },
        )
        .expect("preview");
        assert!(!root_slash.valid);
        assert_eq!(
            root_slash.reason.as_deref(),
            Some("Relative or '/' root is not allowed")
        );
    }

    #[test]
    fn custom_agent_create_persists_row_and_returns_card() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("index.db");
        let database = at_db::Database::open_file(&db_path).expect("db");
        let state = AppState::new(
            Arc::new(JobBridge::new(
                Arc::new(NullSink),
                Arc::new(TracingOperationsLog),
            )),
            None,
        )
        .with_database(database);

        let root_dir = temp.path().join("my-custom-agent");
        std::fs::create_dir_all(&root_dir).expect("mkdir root");

        let card = custom_agent_create_impl(
            &state,
            CustomAgentSpecRequest {
                display_name: "My Custom Agent".to_owned(),
                root: root_dir.display().to_string(),
                skills_subfolder: Some("custom-skills".to_owned()),
                mcp_file: None,
                mcp_format: "none".to_owned(),
                rule_files: vec!["AGENTS.md".to_owned()],
                supports_symlink: true,
            },
        )
        .expect("create");

        assert_eq!(card.name, "My Custom Agent");
        assert_eq!(card.format, "MCP not supported");
        assert!(card.installed);

        // Preview with duplicate name must reject
        let dup = custom_agent_preview_impl(
            &state,
            CustomAgentSpecRequest {
                display_name: "My Custom Agent".to_owned(),
                root: root_dir.display().to_string(),
                skills_subfolder: None,
                mcp_file: None,
                mcp_format: "none".to_owned(),
                rule_files: vec![],
                supports_symlink: true,
            },
        )
        .expect("preview");
        assert!(!dup.valid);
        assert_eq!(
            dup.reason.as_deref(),
            Some("An agent with this name exists")
        );
    }
}
