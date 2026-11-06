//! User-triggered, local-only diagnostics bundle export.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use at_fs::{BackupPolicy, ExpectedState, WriteRequest, apply};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, State};
use ts_rs::TS;
use zip::write::SimpleFileOptions;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Result of opening the diagnostics save dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum DiagnosticsExportOutcome {
    /// The redacted archive was atomically installed at the chosen path.
    Exported {
        /// Filesystem path selected by the user.
        path: String,
    },
    /// The user dismissed the native save dialog; no file was created.
    Cancelled,
}

/// Builds and saves the local diagnostics bundle after the user chooses a destination.
///
/// No arbitrary destination path is accepted from the webview. Cancellation returns a typed
/// outcome and never creates a temporary or partial file.
#[tauri::command]
pub async fn diagnostics_export(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DiagnosticsExportOutcome, CommandError> {
    let logging = state
        .logging()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application logging is unavailable"))?;
    let report = build_diagnostics_report(&state)?;
    let schema_version = report
        .get("schema_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| CommandError::internal("could not read database schema version"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = super::dialogs::choose_save_file(
            &app,
            "Export AgenTuna diagnostics",
            "agentuna-diagnostics.zip",
            "zip",
        )
        .map_err(|_| CommandError::unavailable("the save dialog could not return a local path"))?
        else {
            return Ok(DiagnosticsExportOutcome::Cancelled);
        };
        let log = read_redacted_log(logging.log_dir(), logging.redactor())?;
        let schema = logging
            .redactor()
            .redact(&format!("schema_version={schema_version}\n"));
        let bytes = build_bundle(&log, &report, &schema)?;
        install_bundle(path, bytes)
    })
    .await
    .map_err(|_| CommandError::internal("diagnostics export task failed"))?
}

/// Returns the redacted local diagnostics report for the copyable Doctor dialog.
///
/// # Errors
/// Returns [`CommandError`] when the database or diagnostics snapshot is unavailable.
#[tauri::command]
pub fn diagnostics_report_copy(state: State<'_, AppState>) -> Result<String, CommandError> {
    serde_json::to_string_pretty(&build_diagnostics_report(&state)?)
        .map_err(|_| CommandError::internal("could not serialize the diagnostics report"))
}

fn build_diagnostics_report(state: &AppState) -> Result<Value, CommandError> {
    let database = state
        .database()
        .map_err(|_| CommandError::unavailable("application database is unavailable"))?;
    let connection = database.connection();
    let settings = at_db::settings::export(connection)
        .map_err(|_| CommandError::internal("could not read effective settings"))?;
    let schema_version = database
        .schema_version()
        .map_err(|_| CommandError::internal("could not read database schema version"))?;
    let integrity = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map(|result| json!({ "ok": result.eq_ignore_ascii_case("ok"), "result": result }))
        .unwrap_or_else(|_| json!({ "ok": false, "result": "unavailable" }));
    let mut agents = Vec::new();
    for agent in at_db::agents::list_agents_on_mac(connection)
        .map_err(|_| CommandError::internal("could not read agent detection summary"))?
    {
        let paths = at_db::agents::list_agent_paths(connection, &agent.id)
            .map_err(|_| CommandError::internal("could not read agent path summary"))?;
        agents.push(json!({
            "id": agent.id.to_string(),
            "name": agent.display_name,
            "version": agent.version,
            "presence": format!("{:?}", agent.presence),
            "paths": paths.iter().map(|entry| json!({
                "path": entry.path.as_ref().map(|path| path.display().to_string()),
                "format": entry.format.map(|format| format!("{format:?}")),
                "kind": entry.kind.map(|kind| format!("{kind:?}")),
                "scope": entry.scope.map(|scope| format!("{scope:?}")),
                "exists": entry.exists,
            })).collect::<Vec<_>>(),
            "path_count": paths.len(),
        }));
    }
    let feature_flags = state
        .config()
        .map_or(Value::Null, |config| config.diagnostics_section());
    let report = json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "schema_version": schema_version,
        "agents": agents,
        "effective_settings": settings,
        "feature_flags": feature_flags,
        "database_integrity": integrity,
    });
    if let Some(logging) = state.logging() {
        let encoded = serde_json::to_string(&report)
            .map_err(|_| CommandError::internal("could not encode diagnostics"))?;
        let redacted = logging.redactor().redact(&encoded);
        return serde_json::from_str(&redacted)
            .map_err(|_| CommandError::internal("could not encode redacted diagnostics"));
    }
    Ok(report)
}

fn read_redacted_log(
    directory: &Path,
    redactor: &at_store::logging::RedactorSlot,
) -> Result<String, CommandError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(io_error(directory, &error)),
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with("agentuna-")
                        && name.to_string_lossy().ends_with(".log")
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    let Some(path) = paths.pop() else {
        return Ok(String::new());
    };
    let metadata = fs::metadata(&path).map_err(|error| io_error(&path, &error))?;
    let mut file = fs::File::open(&path).map_err(|error| io_error(&path, &error))?;
    let start = metadata.len().saturating_sub(MAX_LOG_BYTES);
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(start))
        .map_err(|error| io_error(&path, &error))?;
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_LOG_BYTES) as usize);
    file.take(MAX_LOG_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(&path, &error))?;
    Ok(redactor.redact(&String::from_utf8_lossy(&bytes)))
}

fn build_bundle(log: &str, diagnostics: &Value, schema: &str) -> Result<Vec<u8>, CommandError> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, contents) in [
        ("logs/app.log", log.as_bytes().to_vec()),
        (
            "diagnostics.json",
            serde_json::to_vec_pretty(diagnostics)
                .map_err(|_| CommandError::internal("could not encode diagnostics"))?,
        ),
        ("schema.txt", schema.as_bytes().to_vec()),
    ] {
        writer
            .start_file(name, options)
            .map_err(|_| CommandError::internal("could not create diagnostics archive"))?;
        writer
            .write_all(&contents)
            .map_err(|_| CommandError::internal("could not write diagnostics archive"))?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|_| CommandError::internal("could not finish diagnostics archive"))
}

fn install_bundle(path: PathBuf, bytes: Vec<u8>) -> Result<DiagnosticsExportOutcome, CommandError> {
    let (expected, mode) = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CommandError::new(
                CommandErrorCode::InvalidArgument,
                format!("destination is not a regular file: {}", path.display()),
            ));
        }
        Ok(_) => {
            let previous = fs::read(&path).map_err(|error| io_error(&path, &error))?;
            (
                ExpectedState::Hash(at_fs::sha256_hex(&previous)),
                at_fs::atomic::WriteMode::Replace,
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (ExpectedState::Absent, at_fs::atomic::WriteMode::Create)
        }
        Err(error) => return Err(io_error(&path, &error)),
    };
    let mut request = WriteRequest::replace(path.clone(), bytes, expected);
    request.mode = mode;
    request.backup = BackupPolicy::None;
    match apply(&request) {
        Ok(_) => Ok(DiagnosticsExportOutcome::Exported {
            path: path.display().to_string(),
        }),
        Err(error) => Err(CommandError::internal(format!(
            "could not export diagnostics to {}: {error}",
            path.display()
        ))),
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> CommandError {
    let reason = if error.kind() == std::io::ErrorKind::PermissionDenied {
        "permission denied"
    } else {
        "could not access file"
    };
    CommandError::new(
        CommandErrorCode::Unavailable,
        format!("{reason}: {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::redact::Redactor;
    use tempfile::tempdir;

    #[test]
    fn export_archive_contains_only_the_redacted_bundle_entries_and_is_atomic() {
        let secret = concat!("sentinel-", "private-value");
        let redactor = Redactor::new().with_values([secret]);
        let log = redactor.redact_text(&format!("INFO token={secret}\n"));
        let diagnostics =
            redactor.redact_json(&json!({"app_version":"test", "item_content":"omitted"}));
        let zip_bytes = build_bundle(&log, &diagnostics, "schema_version=9\n").expect("bundle");
        let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes.clone())).expect("zip opens");
        assert_eq!(archive.len(), 3);
        let mut log_file = archive.by_name("logs/app.log").expect("log entry");
        let mut exported_log = String::new();
        log_file
            .read_to_string(&mut exported_log)
            .expect("log read");
        assert!(!exported_log.contains(secret));
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("support.zip");
        assert!(matches!(
            install_bundle(path.clone(), zip_bytes),
            Ok(DiagnosticsExportOutcome::Exported { .. })
        ));
        let valid = zip::ZipArchive::new(fs::File::open(path).expect("archive file"));
        assert!(valid.is_ok());
    }
}
