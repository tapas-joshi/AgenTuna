//! Connect agent / Set path dialog backend (TA-059).
//!
//! The dialog reads the persisted candidate probes from TA-032, validates a typed
//! path on change without writing, browses with the native folder picker and saves
//! through the existing [`super::workflows::agent_path_set`] command.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::CommandError;
use super::AppState;

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

/// Lists every recorded candidate path for one agent with its probe markers.
#[tauri::command]
pub fn agent_path_candidates(
    state: State<'_, AppState>,
    agent_id: String,
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
    let _ = agent_id; // kept in the signature so the dialog validates one agent at a time
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
            reason: Some(String::from(
                "The skills location must be a directory",
            )),
        });
    }
    let looks_like_agent = looks_like_agent_folder(Path::new(trimmed));
    Ok(AgentPathValidation {
        exists: true,
        is_directory: true,
        structure_warning: (!looks_like_agent).then(|| {
            String::from("No skills folder or config file found here")
        }),
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
