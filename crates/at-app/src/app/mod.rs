//! Tauri application setup: builder, command registry and window lifecycle.

pub mod bridge;
pub mod cli_install;
pub mod commands;
pub mod config;
pub mod diagnostics;
pub mod dialogs;
pub mod instance;
pub mod jobs;
pub mod linux;
pub mod logs;
pub mod mcp_form;
pub mod menu;
pub mod network;
pub mod overview;
pub mod overwrite;
pub mod paths;
pub mod privilege;
pub mod project_sync;
pub mod projects;
pub mod recovery;
pub mod secrets;
pub mod settings;
pub mod skill_detail;
pub mod skill_wizard;
pub mod source_updates;
pub mod state;
pub mod updater;
pub mod window_state;

use at_store::config::LogLevel;
use std::sync::Arc;
use std::time::Duration;

use tauri::{Emitter, Manager};

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
    let instance_state = Arc::new(primary.into_state());
    let setup_instance = Arc::clone(&instance_state);
    let setup_config = Arc::clone(&config);
    let setup_logging = logging.clone();
    let setup_paths = paths.clone();
    let database = match at_db::Database::open_file(paths.db_path()) {
        Ok(database) => Some(database),
        Err(error) => {
            tracing::error!(%error, "application database unavailable");
            None
        }
    };
    let setup_updates_opt_in = database
        .as_ref()
        .and_then(|database| at_db::settings::load(database.connection()).ok())
        .is_some_and(|settings| settings.auto_update_skills);

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            app.set_menu(menu::build(app.handle())?)?;
            app.handle().on_menu_event(|handle, event| {
            if let Some(action) = menu::MenuAction::from_id(event.id().as_ref())
                && let Err(error) = handle.emit("menu-action", action.id())
            {
                tracing::warn!(%error, action = action.id(), "native menu action dispatch failed");
            }
        });
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
            let state = AppState::new(bridge, cli)
                .with_config(setup_config)
                .with_instance(instance_state)
                .with_logging(setup_logging);
            let state = match database {
                Some(database) => state.with_database(database),
                None => state,
            };
            let state = match state.database_handle() {
                Ok(database) => {
                    let service = Arc::new(recovery::RecoveryService::new(
                        recovery::RecoveryLocations::from_paths(&setup_paths),
                        Arc::new(
                            recovery::DatabaseRecoveryOperations::new(Arc::clone(&database))
                                .with_backup_root(setup_paths.backup_dir()),
                        ),
                        Arc::new(recovery::DatabaseIndexIntegrity::new(database)),
                    ));
                    match service.run() {
                        Ok(outcome) => state.with_recovery(service, outcome),
                        Err(error) => {
                            tracing::error!(%error, "startup recovery could not complete");
                            state
                        }
                    }
                }
                Err(_) => state,
            };
            app.manage(state);
            if let Some(window) = app.get_webview_window("main") {
                window_state::restore_window(&window, app.state::<AppState>().inner());
            }
            // One process-wide policy: nothing reaches the network without its permission.
            let jobs = jobs::JobsControl::default();
            app.manage(jobs.clone());
            let network = NetworkState::new(
                at_model::network::NetworkPolicy::default()
                    .with_updates_opt_in(setup_updates_opt_in),
            );
            app.manage(network.clone());
            if let Ok(update_database) = app.state::<AppState>().database_handle() {
                let update_network = network.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let clock = source_updates::system_update_clock();
                    if let Err(error) = source_updates::run_launch_auto_checks(
                        &update_database,
                        &update_network,
                        &clock,
                    ) {
                        tracing::warn!(%error, "skill update launch check failed");
                    }
                });
            }
            let event_sink = jobs::TauriHealthEventSink::new(app.handle().clone());
            if let Ok(database) = app.state::<AppState>().database_handle() {
                let paths = setup_paths.clone();
                let network = network.clone();
                let jobs = jobs.clone();
                tauri::async_runtime::spawn(async move {
                    let mut scheduler = jobs::HealthScheduler::new();
                    let runner = jobs::McpHealthRunner::default();
                    let clock = jobs::SystemClock;
                    let jitter = jobs::SystemJitter::default();
                    let timer = jobs::TokioSchedulerTimer;
                    let environment = jobs::SchedulerEnvironment {
                        paths: &paths,
                        database: &database,
                        network: &network,
                        clock: &clock,
                        jitter: &jitter,
                        runner: &runner,
                        timer: &timer,
                        event_sink: Some(&event_sink),
                    };
                    if let Err(error) =
                        jobs::run_scheduler(&mut scheduler, &jobs, &environment).await
                    {
                        tracing::warn!(%error, "MCP health scheduler stopped");
                    }
                });
            }
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
        commands::settings_load,
        commands::app_paths,
        commands::settings_set,
        overview::overview_load,
        overview::overview_enable_global_writes,
        commands::recovery_sheet,
        commands::recovery_discard,
        commands::recovery_restore,
        commands::item_lookup,
        skill_detail::skill_detail_load,
        skill_detail::skill_auto_update_set,
        skill_detail::skill_update_ignore,
        source_updates::skill_source_check,
        source_updates::skill_update_preview,
        source_updates::skill_update_apply,
        skill_detail::skill_compare_copies,
        skill_detail::skill_version_compare,
        skill_detail::skill_reveal,
        skill_detail::skill_edit_preview,
        skill_detail::skill_edit_apply,
        window_state::window_state_load,
        window_state::window_state_save,
        window_state::window_state_reset,
        logs::logs_tail,
        logs::logs_reveal,
        diagnostics::diagnostics_export,
        diagnostics::diagnostics_report_copy,
        projects::projects_list,
        projects::projects_add,
        projects::projects_choose_folder,
        projects::projects_scan,
        project_sync::project_sync_status,
        project_sync::project_sync_preview,
        project_sync::project_sync_apply,
        network::network_policy,
        network::network_events,
        network::network_clear_events,
        network::network_set_offline,
        mcp_form::mcp_plan_preview,
        mcp_form::mcp_save,
        commands::mcp_test_connection,
        secrets::secrets_list,
        secrets::secrets_create,
        secrets::secrets_rotate,
        secrets::secrets_delete,
        overwrite::overwrite_move_aside,
        skill_wizard::skill_wizard_transition,
        skill_wizard::skill_add_preview,
        skill_wizard::skill_template_preview,
        skill_wizard::skill_import_choose_folder,
        skill_wizard::skill_import_choose_zip,
        skill_wizard::skill_import_candidates,
        skill_wizard::skill_import_git_candidates,
        skill_wizard::skill_marketplace_search,
        skill_wizard::skill_add_apply
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall,
        commands::app_paths,
        commands::settings_load,
        commands::settings_set,
        commands::recovery_sheet,
        commands::recovery_discard,
        commands::recovery_restore,
        overview::overview_load,
        overview::overview_enable_global_writes,
        commands::item_lookup,
        skill_detail::skill_detail_load,
        skill_detail::skill_auto_update_set,
        skill_detail::skill_update_ignore,
        source_updates::skill_source_check,
        source_updates::skill_update_preview,
        source_updates::skill_update_apply,
        skill_detail::skill_compare_copies,
        skill_detail::skill_version_compare,
        skill_detail::skill_reveal,
        skill_detail::skill_edit_preview,
        skill_detail::skill_edit_apply,
        window_state::window_state_load,
        window_state::window_state_save,
        window_state::window_state_reset,
        logs::logs_tail,
        logs::logs_reveal,
        diagnostics::diagnostics_export,
        diagnostics::diagnostics_report_copy,
        network::network_policy,
        network::network_events,
        projects::projects_list,
        projects::projects_add,
        projects::projects_choose_folder,
        projects::projects_scan,
        project_sync::project_sync_status,
        project_sync::project_sync_preview,
        project_sync::project_sync_apply,
        network::network_set_offline,
        config::config_report,
        mcp_form::mcp_plan_preview,
        mcp_form::mcp_save,
        commands::mcp_test_connection,
        secrets::secrets_list,
        secrets::secrets_create,
        secrets::secrets_rotate,
        secrets::secrets_delete,
        overwrite::overwrite_move_aside,
        skill_wizard::skill_wizard_transition,
        skill_wizard::skill_template_preview,
        skill_wizard::skill_add_preview,
        skill_wizard::skill_import_choose_folder,
        skill_wizard::skill_import_choose_zip,
        skill_wizard::skill_import_candidates,
        skill_wizard::skill_marketplace_search,
        skill_wizard::skill_import_git_candidates,
        skill_wizard::skill_add_apply
    ]);

    let exit_paths = paths.clone();
    let app = builder.build(tauri::generate_context!())?;
    app.run(move |handle, event| {
        if let tauri::RunEvent::WindowEvent { label, event, .. } = &event
            && label == "main"
            && matches!(
                event,
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_)
            )
            && let (Some(window), Some(state)) = (
                handle.get_webview_window("main"),
                handle.try_state::<AppState>(),
            )
        {
            window_state::persist_window(&window, &state);
        }
        if let tauri::RunEvent::WindowEvent { label, .. } = &event
            && label == "main"
            && let (Some(window), Some(jobs)) = (
                handle.get_webview_window("main"),
                handle.try_state::<jobs::JobsControl>(),
            )
        {
            let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
            let paused = jobs.update_window(
                window.is_visible().unwrap_or(false),
                window.is_minimized().unwrap_or(false),
                window.is_focused().unwrap_or(false),
                now_ms as i64,
            );
            tracing::debug!(paused, "updated health scheduler visibility state");
        }
        if !matches!(event, tauri::RunEvent::Exit) {
            return;
        }
        if let Some(jobs) = handle.try_state::<jobs::JobsControl>() {
            jobs.shutdown();
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
