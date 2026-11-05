//! Tauri application setup: builder, command registry and window lifecycle.

pub mod bridge;
pub mod commands;
pub mod menu;
pub mod state;

/// Builds and runs the Tauri application until the last window closes.
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![commands::app_info])
        .run(tauri::generate_context!())
}
