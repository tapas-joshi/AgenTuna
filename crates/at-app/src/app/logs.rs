//! Read-only access to the newest application log for the support screen.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;

/// One parsed, already-redacted log row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct LogLineView {
    /// Severity label, when recognized.
    pub level: String,
    /// Tracing target/module label when present.
    pub target: String,
    /// Human-readable, redacted complete line.
    pub text: String,
}

/// The newest log file and its bounded tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct LogTailView {
    /// Resolved current log file path.
    pub path: String,
    /// At most 1000 newest rows, oldest first.
    pub lines: Vec<LogLineView>,
}

/// Reads the newest log file tail. No filesystem path is accepted from the webview.
#[tauri::command]
pub async fn logs_tail(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<LogTailView, CommandError> {
    let logging = state
        .logging()
        .cloned()
        .ok_or_else(|| CommandError::unavailable("application logging is unavailable"))?;
    let limit = limit.unwrap_or(1000).clamp(1, 1000) as usize;
    tauri::async_runtime::spawn_blocking(move || {
        let mut tail = read_tail(logging.log_dir(), limit)?;
        for line in &mut tail.lines {
            line.text = logging.redactor().redact(&line.text);
            line.target = logging.redactor().redact(&line.target);
        }
        Ok(tail)
    })
    .await
    .map_err(|_| CommandError::internal("log reader task failed"))?
}
/// Reveals the current log file in the operating system's file manager.
#[tauri::command]
pub async fn logs_reveal(state: State<'_, AppState>) -> Result<(), CommandError> {
    let directory = state
        .logging()
        .ok_or_else(|| CommandError::unavailable("application logging is unavailable"))?
        .log_dir()
        .to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let target = newest_log(&directory).unwrap_or_else(|| directory.clone());
        reveal_path(&target)
    })
    .await
    .map_err(|_| CommandError::internal("file manager task failed"))?
}

fn newest_log(directory: &Path) -> Option<PathBuf> {
    let mut paths = fs::read_dir(directory)
        .ok()?
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
    paths.pop()
}

#[cfg(target_os = "macos")]
fn reveal_path(path: &Path) -> Result<(), CommandError> {
    let status = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|_| CommandError::unavailable("could not open Finder"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| CommandError::unavailable("Finder could not reveal the log"))
}

#[cfg(target_os = "windows")]
fn reveal_path(path: &Path) -> Result<(), CommandError> {
    let status = std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .status()
        .map_err(|_| CommandError::unavailable("could not open File Explorer"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| CommandError::unavailable("File Explorer could not reveal the log"))
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn reveal_path(path: &Path) -> Result<(), CommandError> {
    let target = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let status = std::process::Command::new("xdg-open")
        .arg(target)
        .status()
        .map_err(|_| CommandError::unavailable("could not open the file manager"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| CommandError::unavailable("file manager could not reveal the log"))
}
fn read_tail(directory: &Path, limit: usize) -> Result<LogTailView, CommandError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LogTailView {
                path: directory.join("agentuna-*.log").display().to_string(),
                lines: Vec::new(),
            });
        }
        Err(error) => return Err(log_error(directory, &error)),
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
        return Ok(LogTailView {
            path: directory.join("agentuna-*.log").display().to_string(),
            lines: Vec::new(),
        });
    };
    let bytes = fs::read(&path).map_err(|error| log_error(&path, &error))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines = text
        .lines()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(parse_line)
        .collect();
    Ok(LogTailView {
        path: path.display().to_string(),
        lines,
    })
}

fn parse_line(line: &str) -> LogLineView {
    let upper = line.to_ascii_uppercase();
    let level = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"]
        .into_iter()
        .find(|level| upper.contains(level))
        .unwrap_or("INFO")
        .to_owned();
    let target = line
        .split_whitespace()
        .find(|part| part.contains("::") || part.starts_with("target=") || part.starts_with("svc="))
        .unwrap_or("app")
        .trim_end_matches(':')
        .to_owned();
    LogLineView {
        level,
        target,
        text: line.to_owned(),
    }
}

fn log_error(path: &Path, error: &std::io::Error) -> CommandError {
    let reason = match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::NotFound => "file not found",
        _ => "could not read log file",
    };
    CommandError::new(
        CommandErrorCode::NotFound,
        format!("{reason}: {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_only_last_thousand_lines_from_latest_daily_log() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("agentuna-2026-11-06.log");
        let contents = (0..1002)
            .map(|index| format!("INFO svc=fs line-{index}\n"))
            .collect::<String>();
        fs::write(&path, contents).expect("write fixture");
        let tail = read_tail(dir.path(), 1000).expect("read tail");
        assert_eq!(tail.lines.len(), 1000);
        assert!(tail.lines[0].text.contains("line-2"));
        assert!(tail.lines[999].text.contains("line-1001"));
        assert_eq!(tail.lines[0].target, "svc=fs");
    }

    #[test]
    fn missing_and_empty_directories_return_an_empty_tail_with_a_path() {
        let dir = tempdir().expect("temp dir");
        let missing =
            read_tail(&dir.path().join("missing"), 10).expect("missing log directory is empty");
        assert!(missing.lines.is_empty());
        assert!(missing.path.contains("missing"));
        let empty = read_tail(dir.path(), 10).expect("empty log directory is empty");
        assert!(empty.lines.is_empty());
        assert!(empty.path.contains("agentuna-*.log"));
    }
}
