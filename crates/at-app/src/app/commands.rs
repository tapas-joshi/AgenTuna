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
/// Serializable Recovery sheet state shown before normal app work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RecoverySheetView {
    /// Interrupted operations that still have pending journal state.
    pub operations: Vec<RecoveryOperationView>,
    /// Files classified for recovery.
    pub files: Vec<RecoveryFileView>,
    /// Human-review cases that recovery will not modify.
    pub manual_review: Vec<String>,
    /// Temporary files eligible for explicit discard.
    pub temp_files: Vec<String>,
}

/// One interrupted operation in the Recovery sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RecoveryOperationView {
    /// Stable operation id.
    pub id: String,
    /// Operation type.
    pub kind: String,
    /// Paths associated with the operation.
    pub paths: Vec<String>,
}

/// One file state in the Recovery sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RecoveryFileView {
    /// Owning operation id.
    pub operation_id: String,
    /// File path.
    pub path: String,
    /// Current state label.
    pub state: String,
    /// Whether a backup is available for restoration.
    pub can_restore: bool,
    /// Backup digest, when the journal contains one.
    pub before_hash: Option<String>,
}
/// Presentation-ready review of the exact ChangeSet planned for an MCP edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpChangeSetPreviewView {
    /// Identity of the planned ChangeSet.
    pub change_set_id: String,
    /// Content-bound identity of the reviewed files and current disk state.
    pub preview_id: String,
    /// Per-agent files and their exact proposed changes.
    pub files: Vec<McpFileChangeView>,
    /// Fields the selected agents cannot represent, grouped per agent.
    pub unsupported_fields: Vec<McpUnsupportedFieldView>,
    /// Policy warnings raised by the planner, such as a refused secret reference.
    pub warnings: Vec<String>,
    /// Store creation the same reviewed plan performs first, when the scope has no store yet.
    pub store_init: Option<super::store_setup::StoreSetupView>,
}

/// Fields one agent drops from the draft MCP server definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpUnsupportedFieldView {
    /// Human-readable target agent name.
    pub agent: String,
    /// Stable field names the agent cannot represent.
    pub fields: Vec<String>,
}

/// One selected agent target in an MCP ChangeSet preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpFileChangeView {
    /// Human-readable target agent name.
    pub agent: String,
    /// Scope whose resolved config file is being changed.
    pub scope: String,
    /// Resolved target path shown to the user.
    pub path: String,
    /// Number of added lines in this file's exact diff.
    pub additions: u64,
    /// Number of removed lines in this file's exact diff.
    pub deletions: u64,
    /// Unified diff produced from the planned operation.
    pub diff: String,
    /// Whether this target is included in the reviewed operation.
    pub selected: bool,
    /// Reason the selected target cannot be written, if any.
    pub cannot_write_reason: Option<String>,
}

/// Redacted, stable presentation of one connection-test attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpTestResultView {
    /// Whether initialization and tool listing completed.
    pub ok: bool,
    /// Server name reported by the peer or the draft fallback.
    pub server_name: String,
    /// Server version, when reported.
    pub server_version: Option<String>,
    /// Transport used for the attempt.
    pub transport: String,
    /// Elapsed milliseconds.
    pub latency_ms: u64,
    /// Number of returned tools.
    pub tools_count: u64,
    /// Names of all returned tools.
    pub tools: Vec<String>,
    /// Redacted failure information, when the attempt failed.
    pub error: Option<McpTestErrorView>,
    /// Bounded, redacted stderr excerpt for a stdio server.
    pub stderr_excerpt: Option<String>,
}

/// Redacted user-facing error from an MCP connection test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct McpTestErrorView {
    /// Stable error kind identifier.
    pub kind: String,
    /// Redacted explanation.
    pub message: String,
    /// Suggested recovery action.
    pub suggested_action: String,
}

pub(super) fn mcp_test_result_view(result: at_mcp::TestResult) -> McpTestResultView {
    let error = result.error.map(|error| {
        let kind = match error.kind() {
            at_mcp::McpErrorKind::UnresolvedCommand => "unresolved_command",
            at_mcp::McpErrorKind::NotExecutable => "not_executable",
            at_mcp::McpErrorKind::PermissionDenied => "permission_denied",
            at_mcp::McpErrorKind::StartupTimeout => "startup_timeout",
            at_mcp::McpErrorKind::HandshakeTimeout => "handshake_timeout",
            at_mcp::McpErrorKind::ToolListTimeout => "tool_list_timeout",
            at_mcp::McpErrorKind::ProtocolVersionMismatch => "protocol_version_mismatch",
            at_mcp::McpErrorKind::Unauthorized401 => "unauthorized_401",
            at_mcp::McpErrorKind::SignInRequired => "sign_in_required",
            at_mcp::McpErrorKind::TlsError => "tls_error",
            at_mcp::McpErrorKind::ServerError { .. } => "server_error",
            at_mcp::McpErrorKind::TransportUnsupported => "transport_unsupported",
            at_mcp::McpErrorKind::ConsentRequired => "consent_required",
            at_mcp::McpErrorKind::Cancelled => "cancelled",
            at_mcp::McpErrorKind::ConnectionFailed => "connection_failed",
            _ => "connection_failed",
        };
        McpTestErrorView {
            kind: kind.to_owned(),
            message: error.message().to_owned(),
            suggested_action: error.suggested_action().to_owned(),
        }
    });
    McpTestResultView {
        ok: result.ok,
        server_name: result.server_name,
        server_version: result.server_version,
        transport: result.transport,
        latency_ms: u64::try_from(result.latency_ms).unwrap_or(u64::MAX),
        tools_count: u64::try_from(result.tools_count).unwrap_or(u64::MAX),
        tools: result.tools.into_iter().map(|tool| tool.name).collect(),
        error,
        stderr_excerpt: result.stderr_excerpt,
    }
}

/// Runs a filesystem- or database-heavy command body off the IPC thread.
///
/// Commands stay `async` and keep their testable `_inner` functions; the blocking part runs on
/// the runtime's blocking pool so a large apply or hash pass cannot stall other commands.
pub(super) async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CommandError> + Send + 'static,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| CommandError::internal("the background command task failed"))?
}

use super::state::AppState;
// Placeholder

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
    /// An unmanaged filesystem target refused an overwrite.
    #[serde(rename = "refused_overwrite")]
    RefusedOverwrite,
    /// A reviewed plan conflicts with newer or divergent state.
    Conflict,
    /// A file changed outside AgenTuna after the reviewed preview was produced.
    #[serde(rename = "external_write_conflict")]
    ExternalWriteConflict,
    /// The operating system denied access to the path named in the message.
    PermissionDenied,
    /// A Global lock blocks the requested change.
    Locked,
}

/// Structured details for an unmanaged target refused by `at-fs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct RefusedOverwriteView {
    /// Refused filesystem path.
    pub path: String,
    /// Target kind (`file`, `directory`, `symlink`, or `entry`).
    pub kind: String,
    /// Existing file hash, when the target is a regular file.
    pub existing_hash: Option<String>,
    /// Remedies offered for this refusal.
    pub remedies: Vec<String>,
}

/// Typed, redacted error returned by every Tauri command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct CommandError {
    /// Stable error class for branching in the UI.
    pub code: CommandErrorCode,
    /// Human-readable, secret-free explanation.
    pub message: String,
    /// Structured refusal details when an unmanaged target blocked a write.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refusal: Option<Box<RefusedOverwriteView>>,
}

impl CommandError {
    /// Builds an error with an explicit code.
    #[must_use]
    pub fn new(code: CommandErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            refusal: None,
        }
    }

    /// Converts a filesystem overwrite refusal into the typed UI contract.
    #[must_use]
    pub fn refused_overwrite(
        path: &std::path::Path,
        kind: at_fs::TargetKind,
        existing_hash: Option<String>,
    ) -> Self {
        let kind = match kind {
            at_fs::TargetKind::File => "file",
            at_fs::TargetKind::Dir => "directory",
            at_fs::TargetKind::Symlink => "symlink",
            at_fs::TargetKind::Entry => "entry",
            _ => "unknown",
        };
        Self {
            code: CommandErrorCode::RefusedOverwrite,
            message: "AgenTuna refused to overwrite an unmanaged item.".to_owned(),
            refusal: Some(Box::new(RefusedOverwriteView {
                path: path.display().to_string(),
                kind: kind.to_owned(),
                existing_hash,
                remedies: vec![
                    "adopt".to_owned(),
                    "move_aside".to_owned(),
                    "cancel".to_owned(),
                ],
            })),
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

    /// A reviewed file changed outside AgenTuna after its preview was produced.
    #[must_use]
    pub fn external_write_conflict(reason: &str) -> Self {
        Self::new(
            CommandErrorCode::ExternalWriteConflict,
            format!("external write conflict: {reason}"),
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
impl From<at_fs::FsError> for CommandError {
    fn from(error: at_fs::FsError) -> Self {
        match error {
            at_fs::FsError::RefusedOverwrite {
                path,
                kind,
                existing_hash,
                ..
            } => Self::refused_overwrite(&path, kind, existing_hash),
            _ => Self::internal("filesystem operation failed"),
        }
    }
}
impl From<at_fs::changeset::ChangeSetApplyError> for CommandError {
    fn from(error: at_fs::changeset::ChangeSetApplyError) -> Self {
        match error {
            at_fs::changeset::ChangeSetApplyError::Fs(at_fs::FsError::RefusedOverwrite {
                path,
                kind,
                existing_hash,
                ..
            }) => Self::refused_overwrite(&path, kind, existing_hash),
            _ => Self::internal("reviewed filesystem change could not be applied"),
        }
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

/// How the running binary is signed, used by the About dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum SigningStatus {
    /// Signed with a Developer ID Application identity for distribution.
    DeveloperId,
    /// Ad-hoc signed by `scripts/sign.sh` without credentials.
    AdHoc,
    /// Not signed, or the signature could not be read.
    Unsigned,
}

impl SigningStatus {
    /// Classifies the output of `codesign -dv --verbose=4` (which prints to stderr).
    #[must_use]
    pub fn from_codesign_output(output: &str) -> Self {
        if output.contains("Authority=Developer ID Application") {
            Self::DeveloperId
        } else if output.to_ascii_lowercase().contains("adhoc") {
            Self::AdHoc
        } else {
            Self::Unsigned
        }
    }
}

/// Reports how the running binary is signed so About can label a local build.
///
/// A release build signed by `scripts/sign.sh` with a Developer ID prints the distribution
/// identity; an ad-hoc local build or an unsigned development binary is reported as such.
#[tauri::command]
pub fn app_signing_status() -> SigningStatus {
    let Ok(executable) = std::env::current_exe() else {
        return SigningStatus::Unsigned;
    };
    let tool_path = if std::path::Path::new("/usr/bin/codesign").exists() {
        "/usr/bin/codesign"
    } else {
        "codesign"
    };
    let mut child = match std::process::Command::new(tool_path)
        .args(["-dv", "--verbose=4"])
        .arg(&executable)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return SigningStatus::Unsigned,
    };
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SigningStatus::Unsigned;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return SigningStatus::Unsigned;
            }
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return SigningStatus::Unsigned;
    };
    let mut details = String::from_utf8_lossy(&output.stderr).into_owned();
    details.push_str(&String::from_utf8_lossy(&output.stdout));
    SigningStatus::from_codesign_output(&details)
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
/// Loads the durable settings document from the shared application database.
#[tauri::command]
pub fn settings_load(state: State<'_, AppState>) -> Result<serde_json::Value, CommandError> {
    settings_load_inner(&state)
}

/// Loads the durable settings document without requiring a Tauri state wrapper.
///
/// # Errors
/// Returns [`CommandError`] if the database cannot be opened or exported.
pub fn settings_load_inner(state: &AppState) -> Result<serde_json::Value, CommandError> {
    let database = state.database().map_err(CommandError::unavailable)?;
    let document = at_db::settings::export(database.connection())
        .map_err(|error| CommandError::internal(error.to_string()))?;
    serde_json::to_value(document).map_err(|error| CommandError::internal(error.to_string()))
}

/// Validates and stores one canonical preference through `at-db`.
#[tauri::command]
pub fn settings_set(
    state: State<'_, AppState>,
    network: State<'_, super::network::NetworkState>,
    key: String,
    value: serde_json::Value,
) -> Result<(), CommandError> {
    if key.len() > 256 {
        return Err(CommandError::invalid_argument(
            "key",
            "must be at most 256 bytes",
        ));
    }
    let setting_key = at_db::settings::SettingsKey::parse(&key)
        .map_err(|error| CommandError::invalid_argument("key", &error.to_string()))?;
    let updates_opt_in = (setting_key == at_db::settings::SettingsKey::AutoUpdateSkills)
        .then(|| value.as_bool())
        .flatten();
    let value = serde_json::to_string(&value)
        .map_err(|error| CommandError::invalid_argument("value", &error.to_string()))?;
    let mut database = state.database().map_err(CommandError::unavailable)?;
    at_db::settings::set_value(
        database.connection_mut(),
        &setting_key,
        &value,
        at_db::ops::OperationActor::Ui,
    )
    .map_err(|error| CommandError::invalid_argument("value", &error.to_string()))?;
    if let Some(enabled) = updates_opt_in {
        network.set_updates_opt_in(enabled);
    }
    Ok(())
}
/// Returns the resolved application paths without exposing filesystem access to the webview.
#[tauri::command]
pub fn app_paths() -> Result<super::paths::AppPathsView, CommandError> {
    let paths = super::paths::resolve_process_paths(None)
        .map_err(|_| CommandError::unavailable("application paths are unavailable"))?;
    Ok((&paths).into())
}
/// Returns the startup recovery sheet when an unclean exit needs user review.
#[tauri::command]
pub fn recovery_sheet(
    state: State<'_, AppState>,
) -> Result<Option<RecoverySheetView>, CommandError> {
    let Some(sheet) = state.recovery_outcome().and_then(|outcome| outcome.sheet()) else {
        return Ok(None);
    };
    Ok(Some(RecoverySheetView {
        operations: sheet
            .interrupted
            .iter()
            .map(|operation| RecoveryOperationView {
                id: operation.id.clone(),
                kind: operation.kind.clone(),
                paths: operation
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            })
            .collect(),
        files: sheet
            .files
            .iter()
            .map(|file| RecoveryFileView {
                operation_id: file.operation_id.clone(),
                path: file.path.display().to_string(),
                state: file.state.label().to_owned(),
                can_restore: matches!(file.state, at_fs::recovery::EntryState::After)
                    && file.before_hash.is_some(),
                before_hash: file.before_hash.clone(),
            })
            .collect(),
        manual_review: sheet
            .manual_review
            .iter()
            .map(|entry| {
                entry.path.as_ref().map_or_else(
                    || entry.reason.clone(),
                    |path| format!("{}: {}", path.display(), entry.reason),
                )
            })
            .collect(),
        temp_files: sheet
            .temp_files
            .iter()
            .map(|file| file.path.display().to_string())
            .collect(),
    }))
}
/// Discards one operation's pending journal without changing the destination files.
#[tauri::command]
pub fn recovery_discard(
    state: State<'_, AppState>,
    operation_id: String,
) -> Result<(), CommandError> {
    if operation_id.len() > 64 {
        return Err(CommandError::invalid_argument(
            "operation_id",
            "must be at most 64 bytes",
        ));
    }
    let present = state
        .recovery_outcome()
        .and_then(|outcome| outcome.sheet())
        .is_some_and(|sheet| sheet.interrupted.iter().any(|op| op.id == operation_id));
    if !present {
        return Err(CommandError::invalid_argument(
            "operation_id",
            "operation is not pending recovery",
        ));
    }
    let service = state
        .recovery()
        .ok_or_else(|| CommandError::unavailable("recovery service is unavailable"))?;
    service
        .discard(&operation_id)
        .map(|_| ())
        .map_err(|error| CommandError::internal(error.to_string()))
}

/// Restores one file from its recorded backup after validating it against startup recovery state.
#[tauri::command]
pub fn recovery_restore(
    state: State<'_, AppState>,
    operation_id: String,
    path: String,
) -> Result<String, CommandError> {
    if operation_id.len() > 64 || path.len() > 4_096 {
        return Err(CommandError::invalid_argument(
            "recovery_restore",
            "operation id or path exceeds its size limit",
        ));
    }
    let outcome = state
        .recovery_outcome()
        .and_then(|outcome| outcome.sheet())
        .ok_or_else(|| CommandError::unavailable("no recovery review is active"))?;
    let file = outcome
        .files
        .iter()
        .find(|file| file.operation_id == operation_id && file.path.to_string_lossy() == path)
        .ok_or_else(|| CommandError::invalid_argument("path", "file is not pending recovery"))?;
    if !matches!(file.state, at_fs::recovery::EntryState::After) || file.before_hash.is_none() {
        return Err(CommandError::invalid_argument(
            "path",
            "file is not eligible for automatic restoration",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let backups = at_db::backups::backups_for_operation(database.connection(), &operation_id)
        .map_err(|error| CommandError::internal(error.to_string()))?;
    drop(database);
    let path_text = file.path.to_string_lossy();
    let backup = backups
        .into_iter()
        .find(|backup| {
            backup.original_path.as_deref() == Some(path_text.as_ref())
                && backup.content_hash.as_deref() == file.before_hash.as_deref()
        })
        .and_then(|backup| backup.backup_path)
        .ok_or_else(|| CommandError::unavailable("the recorded before-image is unavailable"))?;
    let service = state
        .recovery()
        .ok_or_else(|| CommandError::unavailable("recovery service is unavailable"))?;
    service
        .restore_with_registry(file, std::path::Path::new(&backup), &state.recent_writes())
        .map(|report| report.operation_id)
        .map_err(|error| CommandError::internal(error.to_string()))
}

/// Looks up an opaque item id before the frontend opens its detail route.
#[tauri::command]
pub fn item_lookup(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<Option<at_db::shared::ItemRow>, CommandError> {
    if item_id.len() > 64 {
        return Err(CommandError::invalid_argument(
            "item_id",
            "must be at most 64 bytes",
        ));
    }
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item id"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let mut rows = at_db::shared::items_by_ids(database.connection(), &[id])
        .map_err(|error| CommandError::internal(error.to_string()))?;
    Ok(rows.pop())
}

#[cfg(test)]
mod command_error_tests {
    use super::{CommandError, CommandErrorCode};

    #[test]
    fn refused_overwrite_serializes_typed_remedies_and_hash() {
        let error = CommandError::refused_overwrite(
            std::path::Path::new("/sandbox/agent/skill"),
            at_fs::TargetKind::Dir,
            Some("sha256:fixture".to_owned()),
        );
        let value = serde_json::to_value(error).expect("serialize command error");
        assert_eq!(value["code"], "refused_overwrite");
        assert_eq!(value["refusal"]["path"], "/sandbox/agent/skill");
        assert_eq!(value["refusal"]["kind"], "directory");
        assert_eq!(value["refusal"]["existing_hash"], "sha256:fixture");
        assert_eq!(
            value["refusal"]["remedies"],
            serde_json::json!(["adopt", "move_aside", "cancel"])
        );
        let plain = CommandError::new(CommandErrorCode::Internal, "failure");
        assert!(
            serde_json::to_value(plain)
                .expect("serialize plain error")
                .get("refusal")
                .is_none()
        );
    }
}

#[cfg(test)]
mod signing_tests {
    use super::SigningStatus;

    #[test]
    fn developer_id_output_is_distribution_signed() {
        let output = "Executable=/Applications/AgenTuna.app/Contents/MacOS/agentuna-app\n\
            Identifier=com.agentuna.app\n\
            Authority=Developer ID Application: Example Corp (TEAM123456)\n\
            Authority=Developer ID Certification Authority\n\
            Authority=Apple Root CA\n\
            Timestamp=Nov  6, 2026 at 10:00:00\n\
            Runtime Version=14.0.0\n\
            flags=0x10000(runtime)\n";
        assert_eq!(
            SigningStatus::from_codesign_output(output),
            SigningStatus::DeveloperId
        );
    }

    #[test]
    fn ad_hoc_output_is_a_local_build() {
        let output = "Executable=target/release/agentuna-app\n\
            Identifier=agentuna-app\n\
            Signature=adhoc\n\
            flags=0x2(adhoc)\n";
        assert_eq!(
            SigningStatus::from_codesign_output(output),
            SigningStatus::AdHoc
        );
    }

    #[test]
    fn unsigned_or_unreadable_output_is_unsigned() {
        assert_eq!(
            SigningStatus::from_codesign_output("code object is not signed at all"),
            SigningStatus::Unsigned
        );
        assert_eq!(
            SigningStatus::from_codesign_output(""),
            SigningStatus::Unsigned
        );
    }
}
