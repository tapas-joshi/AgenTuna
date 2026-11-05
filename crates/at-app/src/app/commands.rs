//! Typed Tauri commands. Each command dispatches an engine intent; no UI logic lives here.

use serde::Serialize;
use ts_rs::TS;

/// Static application identity shown in the shell.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AppInfo {
    /// Product name.
    pub name: String,
    /// Workspace version.
    pub version: String,
}

/// Returns the product name and version.
#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "AgenTuna".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}
