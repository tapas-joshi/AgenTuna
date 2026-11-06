//! Remote-backup settings commands. Secret values are accepted only as command input and
//! written directly to the OS credential store; views expose credential presence only.

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;
/// Remote actions, cancellable jobs and conflict-review commands.
#[path = "remote_backup_actions.rs"]
pub(super) mod actions;

/// Settings and status visible to the remote-backup screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupSettingsView {
    /// Local device name used in backup metadata.
    pub device_name: String,
    /// Configured remote repository URL, if any.
    pub repo_url: Option<String>,
    /// Configured branch name, if a repository is connected.
    pub branch: Option<String>,
    /// Time of the most recent successful push, if any.
    pub last_push_at: Option<String>,
    /// Time of the most recent successful pull, if any.
    pub last_pull_at: Option<String>,
    /// Commit identifier for the most recent backup, if available.
    pub last_commit: Option<String>,
    /// Current remote-backup status.
    pub status: String,
    /// Kind of the most recent error, if one occurred.
    pub error_kind: Option<String>,
    /// Whether a credential is stored in the OS credential store.
    pub token_saved: bool,
}

fn repo_url_has_credentials_or_sensitive_suffix(repo_url: &str) -> bool {
    url::Url::parse(repo_url).is_ok_and(|parsed| {
        parsed.password().is_some()
            || (matches!(parsed.scheme(), "http" | "https") && !parsed.username().is_empty())
            || parsed.query().is_some()
            || parsed.fragment().is_some()
    })
}

fn remote_error() -> CommandError {
    CommandError::new(
        CommandErrorCode::Internal,
        "Remote backup settings are unavailable.",
    )
}

/// Reads remote backup settings without reading or returning the stored token.
#[tauri::command]
pub fn remote_backup_settings(
    state: State<'_, AppState>,
) -> Result<RemoteBackupSettingsView, CommandError> {
    let database = state.database().map_err(|_| remote_error())?;
    let db = database.connection();
    let remote = at_db::remote_backup::remote_backup_state(db).map_err(|_| remote_error())?;
    let device_name =
        at_store::remote_backup::device_name(&database).map_err(|_| remote_error())?;
    let token_saved =
        at_secrets::token_exists(&at_secrets::KeychainStore::new()).map_err(|_| remote_error())?;
    Ok(RemoteBackupSettingsView {
        device_name,
        repo_url: remote.repo_url,
        branch: remote.branch,
        last_push_at: remote.last_push_at,
        last_pull_at: remote.last_pull_at,
        last_commit: remote.last_commit,
        status: remote.status.as_str().to_owned(),
        error_kind: remote.error_kind,
        token_saved,
    })
}

/// Saves a device name and token-free GitHub repository URL. A supplied token is stored in
/// Keychain and is never included in the result or any error.
#[tauri::command]
pub fn remote_backup_configure(
    state: State<'_, AppState>,
    device_name: String,
    repo_url: String,
    token: Option<String>,
) -> Result<RemoteBackupSettingsView, CommandError> {
    if device_name.trim() != device_name
        || at_store::remote_backup::validate_device_name(&device_name).is_err()
    {
        return Err(CommandError::invalid_argument(
            "device_name",
            "use 1-64 characters without slashes or control characters",
        ));
    }
    at_git::remote::validate_repo_url(&repo_url).map_err(|_| {
        CommandError::invalid_argument("repo_url", "enter a valid token-free GitHub repository URL")
    })?;
    if repo_url_has_credentials_or_sensitive_suffix(&repo_url) {
        return Err(CommandError::invalid_argument(
            "repo_url",
            "remove credentials, query parameters or fragments from the repository URL",
        ));
    }
    let mut database = state.database().map_err(|_| remote_error())?;
    at_store::remote_backup::set_device_name(&mut database, Some(&device_name))
        .map_err(|_| remote_error())?;
    let old = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|_| remote_error())?;
    let next = at_db::remote_backup::RemoteBackupState {
        repo_url: Some(repo_url),
        status: if old.status == at_db::remote_backup::RemoteBackupStatus::Unconfigured {
            at_db::remote_backup::RemoteBackupStatus::Idle
        } else {
            old.status
        },
        ..old
    };
    at_db::remote_backup::save_remote_backup_state(database.connection_mut(), &next)
        .map_err(|_| remote_error())?;
    if let Some(token) = token.filter(|value| !value.is_empty()) {
        at_secrets::save_token(&at_secrets::KeychainStore::new(), &token).map_err(|_| {
            CommandError::invalid_argument(
                "token",
                "the GitHub token was rejected or could not be stored",
            )
        })?;
    }
    drop(database);
    remote_backup_settings(state)
}
