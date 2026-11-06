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

use at_store::config::LogLevel;
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
    let paths = match paths::resolve_process_paths(None) {
        Ok(paths) => paths,
        Err(error) => {
            // Logging is not installed yet (its directory lives under the support dir we
            // just failed to resolve); stderr is the only channel left.
            eprintln!("agentuna: cannot resolve the AgenTuna paths; the app cannot start: {error}");
            return Ok(());
        }
    };
    if let Err(error) = paths.create_dirs() {
        eprintln!("agentuna: some AgenTuna directories could not be created: {error}");
    }

    // Install logging before the config load so its warnings land in the file; the
    // settings `log_level` is applied through the reload handle right after. The initial
    // filter uses `info` (or `RUST_LOG`); the reload applies the configured level within
    // the same startup, well under the one-second budget.
    let logging_options = at_store::logging::LoggingOptions::for_paths(&paths, LogLevel::Info);
    let guard = match at_store::logging::init(&logging_options) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("agentuna: logging initialization failed: {error}");
            return Ok(());
        }
    };
    let logging = guard.logging().clone();

    let config = Arc::new(config::AppConfig::load(&paths, Vec::new()));
    let settings_level = config_level(&config);
    if settings_level != LogLevel::Info {
        // `set_level` is a no-op when RUST_LOG owns the filter.
        if let Err(error) = logging.set_level(settings_level) {
            tracing::warn!(%error, "could not apply the configured log level");
        }
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
    let instance_state = Arc::new(primary.into_state());
    let setup_instance = Arc::clone(&instance_state);
    let setup_config = Arc::clone(&config);
    let setup_logging = logging.clone();

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
                .with_instance(instance_state)
                .with_logging(setup_logging),
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

/// Reads `[advanced] log_level` out of the effective config report.
///
/// The report stores the level as its lowercase name (`"info"`); anything unexpected falls
/// back to [`LogLevel::Info`] rather than failing startup.
fn config_level(config: &config::AppConfig) -> LogLevel {
    config
        .report()
        .advanced
        .get("log_level")
        .and_then(|value| value.value.as_str())
        .and_then(LogLevel::parse)
        .unwrap_or(LogLevel::Info)
}
