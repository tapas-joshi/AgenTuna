//! Tauri application setup: builder, command registry and window lifecycle.

pub mod bridge;
pub mod cli_install;
pub mod commands;
pub mod menu;
pub mod paths;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use bridge::{JobBridge, TauriEventSink, TracingOperationsLog};
use cli_install::CliInstaller;
use state::AppState;

/// How long quitting waits for in-flight jobs before marking them interrupted.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Builds and runs the Tauri application until the last window closes.
pub fn run() -> tauri::Result<()> {
    init_logging();

    let builder = tauri::Builder::default().setup(|app| {
        let sink = Arc::new(TauriEventSink::new(app.handle().clone()));
        let bridge = Arc::new(JobBridge::new(sink, Arc::new(TracingOperationsLog)));
        let cli = match CliInstaller::for_current_app() {
            Ok(installer) => Some(installer),
            Err(error) => {
                tracing::warn!(error = %error, "packaged CLI installer unavailable");
                None
            }
        };
        app.manage(AppState::new(bridge, cli));
        Ok(())
    });

    #[cfg(debug_assertions)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall,
        commands::dev_run_job
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall
    ]);

    let app = builder.build(tauri::generate_context!())?;
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit)
            && let Some(state) = handle.try_state::<AppState>()
        {
            let report = state.bridge().shutdown(SHUTDOWN_GRACE);
            tracing::info!(
                interrupted = report.interrupted,
                "job bridge stopped on app exit"
            );
        }
    });
    Ok(())
}

fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
}
