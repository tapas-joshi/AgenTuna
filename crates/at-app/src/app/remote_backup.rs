//! Remote-backup settings commands. Secret values are accepted only as command input and
//! written directly to the OS credential store; views expose credential presence only.

use std::sync::{Arc, Mutex};

use at_db::Database;
use at_model::network::{NetworkAction, NetworkEvent, NetworkHost, NetworkTrigger};
use time::OffsetDateTime;

use super::bridge::{JobError, JobId, JobSpec};
use super::network::NetworkState;
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;
/// Remote actions, cancellable jobs and conflict-review commands.
#[path = "remote_backup_actions.rs"]
pub mod actions;

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

/// Repository creation request. It deliberately has no credential field: the command reads the
/// saved token from Keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupCreateRepoRequest {
    /// GitHub repository name to create under the authenticated user's account.
    pub repo_name: String,
}

/// Handle for a cancellable private-repository creation job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RemoteBackupCreateRepoView {
    /// Cancellable bridge job that creates and configures the repository.
    pub job_id: JobId,
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

fn create_failure(
    code: actions::RemoteBackupFailureCode,
    message: &'static str,
    remedy: actions::RemoteBackupRemedy,
) -> actions::RemoteBackupFailure {
    actions::RemoteBackupFailure {
        code,
        message: message.to_owned(),
        remedy,
    }
}

fn action_failed() -> actions::RemoteBackupFailure {
    create_failure(
        actions::RemoteBackupFailureCode::ActionFailed,
        "Private repository creation did not finish. Check the connection and retry.",
        actions::RemoteBackupRemedy::Retry,
    )
}

fn github_failure(error: at_git::github::GitHubError) -> actions::RemoteBackupFailure {
    use actions::{RemoteBackupFailureCode as Code, RemoteBackupRemedy as Remedy};
    use at_git::github::GitHubError;

    match error {
        GitHubError::MissingRepoScope | GitHubError::RepoScopeUnreported => create_failure(
            Code::RepoScopeMissing,
            "The saved GitHub token does not report the repo scope required to create a private repository.",
            Remedy::AddRepoScope,
        ),
        GitHubError::AuthRejected => create_failure(
            Code::TokenRejected,
            "GitHub rejected the saved token. Replace it in Preferences and retry.",
            Remedy::ReplaceToken,
        ),
        GitHubError::Offline => create_failure(
            Code::Offline,
            "GitHub could not be reached. Check offline mode and retry explicitly.",
            Remedy::TurnNetworkOn,
        ),
        GitHubError::InvalidRepoName => create_failure(
            Code::RepoNameInvalid,
            "Use a repository name with letters, digits, dots, hyphens or underscores.",
            Remedy::EditRepoName,
        ),
        GitHubError::CreateRefused => create_failure(
            Code::ActionFailed,
            "GitHub refused to create that repository. Choose another name and retry.",
            Remedy::EditRepoName,
        ),
        _ => action_failed(),
    }
}

fn network_failure(denied: &at_model::network::NetworkDenied) -> actions::RemoteBackupFailure {
    match denied.reason {
        at_model::network::DenialReason::Offline => create_failure(
            actions::RemoteBackupFailureCode::Offline,
            "Offline mode is on; turn it off before connecting to GitHub.",
            actions::RemoteBackupRemedy::TurnNetworkOn,
        ),
        _ => create_failure(
            actions::RemoteBackupFailureCode::NetworkDenied,
            "The network policy refused this private-repository request.",
            actions::RemoteBackupRemedy::ReviewNetworkPolicy,
        ),
    }
}

fn secret_failure(error: at_secrets::SecretsError) -> actions::RemoteBackupFailure {
    if matches!(error, at_secrets::SecretsError::NotFound { .. }) {
        create_failure(
            actions::RemoteBackupFailureCode::TokenMissing,
            "No GitHub token is saved; add one in Preferences.",
            actions::RemoteBackupRemedy::SaveToken,
        )
    } else {
        action_failed()
    }
}

/// Reads the saved GitHub token from secrets storage.
///
/// # Errors
///
/// Returns [`actions::RemoteBackupFailure`] when the token is missing or unreadable.
pub fn create_repo_token(
    secrets: &dyn at_secrets::SecretStore,
) -> Result<at_secrets::SecretString, actions::RemoteBackupFailure> {
    at_secrets::read_token(secrets).map_err(secret_failure)
}

fn configure_remote_backup_database(
    database: &mut Database,
    device_name: &str,
    repo_url: &str,
) -> Result<(), CommandError> {
    if device_name.trim() != device_name
        || at_store::remote_backup::validate_device_name(device_name).is_err()
    {
        return Err(CommandError::invalid_argument(
            "device_name",
            "use 1-64 characters without slashes or control characters",
        ));
    }
    at_git::remote::validate_repo_url(repo_url).map_err(|_| {
        CommandError::invalid_argument("repo_url", "enter a valid token-free GitHub repository URL")
    })?;
    if repo_url_has_credentials_or_sensitive_suffix(repo_url) {
        return Err(CommandError::invalid_argument(
            "repo_url",
            "remove credentials, query parameters or fragments from the repository URL",
        ));
    }
    at_store::remote_backup::set_device_name(database, Some(device_name))
        .map_err(|_| remote_error())?;
    let old = at_db::remote_backup::remote_backup_state(database.connection())
        .map_err(|_| remote_error())?;
    let next = at_db::remote_backup::RemoteBackupState {
        repo_url: Some(repo_url.to_owned()),
        status: if old.status == at_db::remote_backup::RemoteBackupStatus::Unconfigured {
            at_db::remote_backup::RemoteBackupStatus::Idle
        } else {
            old.status
        },
        ..old
    };
    at_db::remote_backup::save_remote_backup_state(database.connection_mut(), &next)
        .map_err(|_| remote_error())
}

/// Creates the private GitHub backup repository and persists the configuration.
///
/// # Errors
///
/// Returns [`actions::RemoteBackupFailure`] on failure.
pub async fn create_private_repo_and_configure(
    database: Arc<Mutex<Database>>,
    client: at_git::github::GitHubClient,
    token: at_secrets::SecretString,
    repo_name: String,
    device_name: String,
    network: NetworkState,
    host: NetworkHost,
) -> Result<(), actions::RemoteBackupFailure> {
    let created = client
        .create_private_repo(token.expose_secret(), &repo_name, None)
        .await;
    network.record(NetworkEvent::new(
        OffsetDateTime::now_utc(),
        NetworkAction::RemoteBackupPush,
        Some(host),
        0,
    ));
    let created = created.map_err(github_failure)?;
    let mut database = database.lock().map_err(|_| action_failed())?;
    configure_remote_backup_database(&mut database, &device_name, &created.url)
        .map_err(|_| action_failed())
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
    let mut database = state.database().map_err(|_| remote_error())?;
    configure_remote_backup_database(&mut database, &device_name, &repo_url)?;
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

/// Starts private GitHub repository creation using the token already saved in Keychain.
///
/// The request never accepts a credential. Creation checks the `repo` scope before its POST,
/// and saves the resulting URL through the same settings path as
/// [`remote_backup_configure`].
///
/// # Errors
///
/// Returns a secret-free typed failure for invalid names, missing credentials, network-policy
/// refusal, missing token scope or inability to start the background job.
#[tauri::command]
pub fn remote_backup_create_repo(
    state: State<'_, AppState>,
    network: State<'_, NetworkState>,
    request: RemoteBackupCreateRepoRequest,
) -> Result<RemoteBackupCreateRepoView, actions::RemoteBackupFailure> {
    at_git::github::validate_repo_name(&request.repo_name).map_err(github_failure)?;
    let host = network
        .authorize(
            NetworkAction::RemoteBackupPush,
            "api.github.com",
            NetworkTrigger::User,
        )
        .map_err(|denied| network_failure(&denied))?;
    let database = state.database_handle().map_err(|_| action_failed())?;
    let device_name = {
        let database = database.lock().map_err(|_| action_failed())?;
        at_store::remote_backup::device_name(&database).map_err(|_| action_failed())?
    };
    at_mcp::ensure_crypto_provider();
    let token = create_repo_token(&at_secrets::KeychainStore::new())?;
    let client = at_git::github::GitHubClient::new().map_err(github_failure)?;
    let network = network.inner().clone();
    let repo_name = request.repo_name;
    let spec = JobSpec::new_cancellable(
        "Create private repository",
        move |progress, cancel| async move {
            progress.fraction(0.1, "Checking the saved GitHub token");
            let operation = create_private_repo_and_configure(
                database,
                client,
                token,
                repo_name,
                device_name,
                network,
                host,
            );
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                result = operation => result.map_err(|failure| JobError::RemoteBackup { failure })?,
            };
            progress.fraction(1.0, "Private repository created and connected");
            Ok(())
        },
    );
    let job = state
        .bridge()
        .spawn_job(spec)
        .map_err(|_| action_failed())?;
    Ok(RemoteBackupCreateRepoView {
        job_id: job.id().clone(),
    })
}
