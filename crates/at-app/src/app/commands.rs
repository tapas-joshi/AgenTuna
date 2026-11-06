//! Typed Tauri commands. Each command dispatches an engine intent; no UI logic lives here.

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

#[cfg(debug_assertions)]
use super::bridge::JobSpec;
use super::bridge::{JobId, JobSnapshot};
use super::cli_install::{
    CliInstallError, CliInstallLocation, CliInstallReport, CliInstallStatus, CliInstaller,
    CliUninstallReport,
};
use super::state::AppState;

/// Static application identity shown in the shell.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AppInfo {
    /// Product name.
    pub name: String,
    /// Workspace version.
    pub version: String,
}

/// Machine-readable class of a command failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum CommandErrorCode {
    /// An argument failed validation.
    InvalidArgument,
    /// The requested item does not exist.
    NotFound,
    /// The async bridge rejected the request.
    Bridge,
    /// The packaged CLI installer failed.
    CliInstall,
    /// A required service is unavailable in this build.
    Unavailable,
    /// An unexpected internal failure occurred.
    Internal,
}

/// Typed, redacted error returned by every Tauri command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CommandError {
    /// Stable error class for branching in the UI.
    pub code: CommandErrorCode,
    /// Human-readable, secret-free explanation.
    pub message: String,
}

impl CommandError {
    /// Builds an error with an explicit code.
    #[must_use]
    pub fn new(code: CommandErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// An argument failed validation.
    #[must_use]
    pub fn invalid_argument(field: &str, reason: &str) -> Self {
        Self::new(
            CommandErrorCode::InvalidArgument,
            format!("invalid `{field}`: {reason}"),
        )
    }

    /// A required service is unavailable.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::Unavailable, message)
    }

    /// An unexpected internal failure.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::Internal, message)
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<CliInstallError> for CommandError {
    fn from(error: CliInstallError) -> Self {
        Self::new(CommandErrorCode::CliInstall, error.to_string())
    }
}

impl From<super::bridge::BridgeError> for CommandError {
    fn from(error: super::bridge::BridgeError) -> Self {
        Self::new(CommandErrorCode::Bridge, error.to_string())
    }
}

/// Returns the product name and version.
#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "AgenTuna".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// Returns every job currently running in the async bridge.
#[tauri::command]
pub fn jobs_list(state: State<'_, AppState>) -> Vec<JobSnapshot> {
    state.bridge().jobs()
}

/// Cancels a running job. Returns `true` when a running job was cancelled.
///
/// # Errors
///
/// [`CommandError`] with [`CommandErrorCode::InvalidArgument`] for a malformed job id.
#[tauri::command]
pub fn jobs_cancel(state: State<'_, AppState>, job_id: String) -> Result<bool, CommandError> {
    let id: JobId = job_id.parse().map_err(|_| {
        CommandError::invalid_argument("job_id", "expected `job_` and 32 hex digits")
    })?;
    Ok(state.bridge().cancel_job(&id))
}

/// Reports both CLI link locations, their writability and any existing installation.
///
/// # Errors
///
/// [`CommandErrorCode::Unavailable`] when the running binary is not inside an app bundle
/// that contains the CLI.
#[tauri::command]
pub fn cli_install_status(state: State<'_, AppState>) -> Result<CliInstallStatus, CommandError> {
    Ok(installer(&state)?.status())
}

/// Installs the `agentuna` symlink at the chosen location.
///
/// # Errors
///
/// [`CommandError`] when the location is not writable, already holds a foreign file or
/// symlink, or the bundled binary is missing.
#[tauri::command]
pub async fn cli_install(
    state: State<'_, AppState>,
    location: CliInstallLocation,
) -> Result<CliInstallReport, CommandError> {
    let installer = installer(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || installer.install(location))
        .await
        .map_err(|error| CommandError::internal(format!("install task failed: {error}")))?
        .map_err(CommandError::from)
}

/// Removes the `agentuna` symlink when it points at this app's bundled binary.
///
/// # Errors
///
/// [`CommandErrorCode::Unavailable`] when the bundled installer cannot be resolved.
#[tauri::command]
pub async fn cli_uninstall(
    state: State<'_, AppState>,
    location: Option<CliInstallLocation>,
) -> Result<CliUninstallReport, CommandError> {
    let installer = installer(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || installer.uninstall(location))
        .await
        .map_err(|error| CommandError::internal(format!("uninstall task failed: {error}")))
}

/// Development-only job that exercises the bridge from the shell UI: it runs for
/// `seconds`, reports progress, and succeeds. Not registered in release builds.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn dev_run_job(state: State<'_, AppState>, seconds: f64) -> Result<JobSnapshot, CommandError> {
    if !(0.05..=300.0).contains(&seconds) {
        return Err(CommandError::invalid_argument(
            "seconds",
            "expected a value between 0.05 and 300",
        ));
    }
    let duration = std::time::Duration::from_secs_f64(seconds);
    let handle =
        state
            .bridge()
            .spawn_job(JobSpec::new("Simulated job", move |progress| async move {
                let steps: u32 = 40;
                for step in 1..=steps {
                    tokio::time::sleep(duration / steps).await;
                    progress.fraction(
                        f64::from(step) / f64::from(steps),
                        format!("Step {step} of {steps}"),
                    );
                }
                Ok(())
            }))?;
    Ok(handle.snapshot())
}

fn installer(state: &AppState) -> Result<&CliInstaller, CommandError> {
    state.cli().ok_or_else(|| {
        CommandError::unavailable("the bundled `agentuna` command-line tool was not found")
    })
}
