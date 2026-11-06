//! Typed commands for safely resolving refused filesystem overwrites.

use std::path::{Component, Path, PathBuf};

use super::commands::CommandError;
use super::state::AppState;
use at_fs::MoveAsideOutcome;
use serde::{Deserialize, Serialize};
use tauri::State;
use time::OffsetDateTime;
use ts_rs::TS;

/// Result of moving a refused path aside without copying or overwriting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct OverwriteMoveAsideView {
    /// Path that was moved.
    pub original: String,
    /// New path containing the preserved original bytes.
    pub moved_to: String,
}

impl From<MoveAsideOutcome> for OverwriteMoveAsideView {
    fn from(outcome: MoveAsideOutcome) -> Self {
        Self {
            original: outcome.original.display().to_string(),
            moved_to: outcome.moved_to.display().to_string(),
        }
    }
}

/// Moves a refused target aside after checking it is under a registered agent path.
///
/// # Errors
/// Returns a typed command error if the path is invalid, unregistered, or cannot be moved.
#[tauri::command]
pub fn overwrite_move_aside(
    state: State<'_, AppState>,
    path: String,
) -> Result<OverwriteMoveAsideView, CommandError> {
    if path.len() > 4096 {
        return Err(CommandError::invalid_argument(
            "path",
            "must be at most 4096 bytes",
        ));
    }
    let target = PathBuf::from(&path);
    if !target.is_absolute() || target.components().any(|part| part == Component::ParentDir) {
        return Err(CommandError::invalid_argument(
            "path",
            "must be an absolute normalized path",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let agents = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not verify the registered agent paths"))?;
    let mut registered_paths = Vec::new();
    for agent in agents {
        let paths = at_db::agents::list_agent_paths(database.connection(), &agent.id)
            .map_err(|_| CommandError::internal("could not verify the registered agent paths"))?;
        registered_paths.extend(paths.into_iter().filter_map(|entry| entry.path));
    }
    drop(database);
    if !is_registered_target(&target, &registered_paths) {
        return Err(CommandError::invalid_argument(
            "path",
            "is outside registered agent paths",
        ));
    }
    let outcome = at_fs::move_aside(&target, OffsetDateTime::now_utc())
        .map_err(|_| CommandError::unavailable("The refused item could not be moved aside"))?;
    Ok(outcome.into())
}

fn is_registered_target(target: &Path, registered_paths: &[PathBuf]) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    let Ok(canonical_parent) = parent.canonicalize() else {
        return false;
    };
    registered_paths.iter().any(|registered| {
        if registered == target {
            return true;
        }
        let Ok(canonical_root) = registered.canonicalize() else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&canonical_root) else {
            return false;
        };
        metadata.is_dir() && canonical_parent.starts_with(canonical_root)
    })
}

#[cfg(test)]
mod tests {
    use super::is_registered_target;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn accepts_only_existing_targets_under_registered_roots() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("agent-skills");
        std::fs::create_dir(&root).expect("create root");
        let target = root.join("native-skill");
        std::fs::create_dir(&target).expect("create target");
        let sibling = temp.path().join("unregistered");
        std::fs::create_dir(&sibling).expect("create sibling");
        assert!(is_registered_target(&target, std::slice::from_ref(&root)));
        assert!(!is_registered_target(&sibling, &[root]));
        assert!(!is_registered_target(&PathBuf::from("relative"), &[]));
    }
}
