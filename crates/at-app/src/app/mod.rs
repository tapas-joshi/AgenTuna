//! Tauri application setup: builder, command registry and window lifecycle.

pub mod bridge;
pub mod cli_install;
pub mod commands;
pub mod config;
pub mod instance;
pub mod linux;
pub mod menu;
pub mod network;
pub mod paths;
pub mod privilege;
pub mod recovery;
pub mod settings;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use bridge::{JobBridge, TauriEventSink, TracingOperationsLog};
use cli_install::CliInstaller;
use network::NetworkState;
use state::AppState;

/// How long quitting waits for in-flight jobs before marking them interrupted.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Builds and runs the Tauri application until the last window closes.
pub fn run() -> tauri::Result<()> {
    init_logging();

    let paths = match paths::resolve_process_paths(None) {
        Ok(paths) => paths,
        Err(error) => {
            tracing::error!(%error, "cannot resolve the AgenTuna paths; the app cannot start");
            return Ok(());
        }
    };
    if let Err(error) = paths.create_dirs() {
        tracing::warn!(%error, "some AgenTuna directories could not be created");
    }
    let primary = match instance::begin(&paths) {
        instance::Startup::Primary(primary) => primary,
        instance::Startup::AlreadyRunning { holder } => {
            let pid = holder.as_ref().map(|record| record.pid);
            let focused = instance::activate_existing(&paths);
            tracing::info!(
                ?pid,
                focused,
                "another AgenTuna instance is running; this launch exits without a window"
            );
            return Ok(());
        }
    };
    // Record this session before anything else can crash: `clean_exit: false` in `session.json` is
    // what makes the next start run recovery. This happens after the single-instance gate (only
    // the primary instance owns the marker) and before the window exists, so recovery is always
    // decided before the UI is interactive.
    match recovery::SessionFile::new(paths.support_dir()).start() {
        Ok(recovery::PreviousExit::Unclean(previous)) => tracing::warn!(
            pid = previous.pid,
            started_at = %previous.started_at,
            "the previous session did not exit cleanly; startup recovery will run"
        ),
        Ok(recovery::PreviousExit::Running(previous)) => tracing::warn!(
            pid = previous.pid,
            "another live instance owns the session marker; recovery will not run"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not write the session marker"),
    }
    let config = Arc::new(config::AppConfig::load(&paths, Vec::new()));
    let instance_state = Arc::new(primary.into_state());
    let setup_instance = Arc::clone(&instance_state);
    let setup_config = Arc::clone(&config);

    let builder = tauri::Builder::default().setup(move |app| {
        let focus_handle = app.handle().clone();
        if let Err(error) =
            setup_instance.start_activation(move || focus_main_window(&focus_handle))
        {
            tracing::warn!(
                %error,
                "activation listener unavailable; a second launch cannot focus this window"
            );
        }
        let sink = Arc::new(TauriEventSink::new(app.handle().clone()));
        let bridge = Arc::new(JobBridge::new(sink, Arc::new(TracingOperationsLog)));
        let cli = match CliInstaller::for_current_app() {
            Ok(installer) => Some(installer),
            Err(error) => {
                tracing::warn!(error = %error, "packaged CLI installer unavailable");
                None
            }
        };
        app.manage(
            AppState::new(bridge, cli)
                .with_config(setup_config)
                .with_instance(instance_state),
        );
        // One process-wide policy: nothing reaches the network without its permission.
        app.manage(NetworkState::default());
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
        network::network_policy,
        network::network_events,
        network::network_clear_events,
        network::network_set_offline,
        config::config_report,
        commands::dev_run_job
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall,
        network::network_policy,
        network::network_events,
        network::network_clear_events,
        network::network_set_offline,
        config::config_report
    ]);

    let exit_paths = paths.clone();
    let app = builder.build(tauri::generate_context!())?;
    app.run(move |handle, event| {
        if !matches!(event, tauri::RunEvent::Exit) {
            return;
        }
        if let Some(state) = handle.try_state::<AppState>() {
            if let Some(instance) = state.instance() {
                instance.shutdown();
            }
            let report = state.bridge().shutdown(SHUTDOWN_GRACE);
            tracing::info!(
                interrupted = report.interrupted,
                "job bridge stopped on app exit"
            );
        }
        // Last thing on the way out: anything that crashes before this point stays an unclean
        // exit and is repaired on the next start.
        if let Err(error) = recovery::SessionFile::new(exit_paths.support_dir()).mark_clean() {
            tracing::warn!(%error, "could not mark the session as a clean exit");
        }
    });
    Ok(())
}

/// Raises the main window, the Tauri equivalent of `NSApplication activate`.
fn focus_main_window(handle: &tauri::AppHandle) {
    if let Some(window) = handle.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
}
