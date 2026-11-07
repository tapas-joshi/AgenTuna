//! Tauri application setup: builder, command registry and window lifecycle.

pub mod agent_connect;
pub mod bridge;
pub mod bulk_skill;
pub mod cli_install;
pub mod commands;
pub mod config;
pub mod deep_link;
pub mod detection;
pub mod diagnostics;
pub mod dialogs;
pub mod drift;
pub mod drift_commands;
#[cfg(test)]
pub(crate) mod drift_testkit;
pub mod effective;
pub mod external_link;
pub mod flow_harness;
pub mod instance;
pub mod jobs;
pub mod linux;
pub mod logs;
pub mod mcp_copy;
pub mod mcp_form;
pub mod mcp_launch;
pub mod mcp_servers;
pub mod menu;
pub mod migrate;
pub mod native_scan;
pub mod network;
pub mod overview;
pub mod overwrite;
pub mod paths;
pub mod preset_apply;
pub mod privilege;
pub mod project_sync;
pub mod projects;
pub mod recovery;
pub mod remote_backup;
pub mod rule_create;
pub mod secrets;
pub mod settings;
pub mod skill_agent;
pub mod skill_delete;
pub mod skill_detail;
pub mod skill_transfer;
pub mod skill_wizard;
pub mod skills_list;
pub mod source_updates;
pub mod startup;
pub mod state;
pub mod store_setup;
pub mod updater;
pub mod window_state;
pub mod workflows;

use at_store::config::LogLevel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};

use bridge::{DatabaseOperationsLog, JobBridge, TauriEventSink, TracingOperationsLog};
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
    let startup = startup::open_database(&paths);
    if let Some(report) = &startup.rebuild {
        tracing::info!(
            items = report.counts.items,
            native_items = report.counts.native_items,
            duration_ms = report.duration_ms,
            "derived data rebuilt at startup"
        );
    }
    let setup_warnings = startup.warnings;
    let database = startup.database;
    let setup_updates_opt_in = database
        .as_ref()
        .and_then(|database| at_db::settings::load(database.connection()).ok())
        .is_some_and(|settings| settings.auto_update_skills);

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            create_main_window(app)?;
            let native_menu = menu::build(app.handle())?;
            app.set_menu(native_menu.menu)?;
            app.manage(native_menu.controls);
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
            // One database handle is shared by the app state and the bridge's operations log, so
            // a shutdown row and a command write go through the same connection.
            let database = database.map(|database| Arc::new(Mutex::new(database)));
            let operations: Arc<dyn bridge::OperationsLog> = match &database {
                Some(database) => Arc::new(DatabaseOperationsLog::new(Arc::clone(database))),
                None => Arc::new(TracingOperationsLog),
            };
            let bridge = Arc::new(JobBridge::new(sink, operations));
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
                .with_logging(setup_logging)
                .with_paths(setup_paths.clone())
                .with_startup_warnings(setup_warnings);
            let state = match database {
                Some(database) => state.with_database_handle(database),
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
            app.manage(deep_link::DeepLinkRelay::default());
            app.manage(state);
            if let Ok(database) = app.state::<AppState>().database_handle()
                && let Err(error) = detection::start(
                    app.handle().clone(),
                    setup_paths.clone(),
                    Arc::clone(&database),
                    app.state::<AppState>().recent_writes(),
                )
            {
                tracing::warn!(%error, "agent filesystem detection watcher unavailable");
            }
            if let Ok(database) = app.state::<AppState>().database_handle() {
                let watcher = drift::start(
                    app.handle().clone(),
                    setup_paths.clone(),
                    database,
                    app.state::<AppState>().recent_writes(),
                );
                app.manage(watcher);
            }
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
            // App-update state: staged archives, the persisted install queue and the journal of
            // the last bundle swap. The cache lives beside the other derived app data.
            app.manage(updater::commands::UpdaterState::system(
                setup_paths.cache_dir().join("updates"),
            ));
            {
                let updater_handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let updater = updater_handle.state::<updater::commands::UpdaterState>();
                    match updater.reconcile(env!("CARGO_PKG_VERSION")) {
                        Ok(updater::install::BundleSwapReconcile::Committed { version }) => {
                            tracing::info!(%version, "confirmed the updated bundle on first launch");
                        }
                        Ok(updater::install::BundleSwapReconcile::RolledBack { version }) => {
                            tracing::warn!(?version, "restored the previous bundle after an interrupted update");
                            match version {
                                Some(version) => {
                                    updater.set_status(updater::install::UpdateInstallStatus::RolledBack {
                                        version,
                                        reason: "The previous update did not start.".to_owned(),
                                        remedy: "AgenTuna is running the previous version again; retry the update or install the release manually.".to_owned(),
                                    });
                                }
                                None => tracing::warn!(
                                    "the restored bundle has no recorded version"
                                ),
                            }
                        }
                        Ok(updater::install::BundleSwapReconcile::ManualReview { reason }) => {
                            tracing::warn!(%reason, "update journal needs manual review");
                        }
                        Ok(
                            updater::install::BundleSwapReconcile::Nothing
                            | updater::install::BundleSwapReconcile::Disposed,
                        ) => {}
                        Err(error) => {
                            tracing::warn!(%error, "could not reconcile the update journal");
                        }
                    }
                });
            }
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
        external_link::open_external_link,
        deep_link::deep_link_take,
        commands::app_signing_status,
        updater::commands::app_update_status,
        updater::commands::app_update_check,
        updater::commands::app_update_install_request,
        updater::commands::app_update_install_resume,
        updater::commands::app_update_install_cancel,
        menu::app_menu_set_add_skill_enabled,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall,
        commands::settings_load,
        commands::app_paths,
        commands::settings_set,
        remote_backup::remote_backup_settings,
        remote_backup::remote_backup_configure,
        remote_backup::remote_backup_create_repo,
        remote_backup::actions::status,
        remote_backup::actions::backup_now,
        remote_backup::actions::sync_now,
        remote_backup::actions::restore_commits,
        remote_backup::actions::restore_confirmation,
        remote_backup::actions::restore,
        remote_backup::actions::conflicts_list,
        remote_backup::actions::conflicts_reveal,
        remote_backup::actions::conflicts_resolve_preview,
        remote_backup::actions::conflicts_resolve_apply,
        effective::effective_query,
        effective::effective_counts,
        effective::why_trace,
        effective::effective_lock,
        effective::effective_unlock,
        migrate::migrate_scan,
        migrate::migrate_scan_start,
        migrate::migrate_scan_result,
        overview::overview_load,
        drift_commands::drift_snapshot,
        drift_commands::drift_rescan,
        drift_commands::drift_watcher_set_paused,
        drift_commands::drift_preview,
        drift_commands::drift_adopt,
        drift_commands::drift_restore,
        drift_commands::drift_open_in_editor,
        drift_commands::backups_list,
        drift_commands::backup_preview,
        drift_commands::backup_restore,
        overview::overview_enable_global_writes,
        overview::overview_setup_preview,
        overview::overview_setup_apply,
        privilege::global_store_overview,
        privilege::global_enable_writes,
        commands::recovery_sheet,
        commands::recovery_discard,
        commands::recovery_restore,
        commands::item_lookup,
        skill_detail::skill_detail_load,
        skill_agent::skill_agent_preview,
        skill_agent::skill_agent_apply,
        skill_agent::skill_agent_discard,
        skills_list::skills_list,
        skill_detail::skill_auto_update_set,
        skill_detail::skill_update_ignore,
        source_updates::skill_source_check,
        source_updates::skill_update_preview,
        source_updates::skill_update_apply,
        skill_detail::skill_compare_copies,
        skill_detail::skill_version_compare,
        skill_detail::skill_reveal,
        skill_detail::reveal_item,
        skill_detail::skill_edit_preview,
        skill_detail::skill_edit_apply,
        skill_detail::skill_keep_copy_preview,
        skill_detail::skill_keep_copy_apply,
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
        mcp_form::mcp_test_draft,
        mcp_form::mcp_launch_prepare,
        mcp_servers::mcp_server_list,
        mcp_servers::mcp_server_disable,
        mcp_servers::mcp_server_delete,
        mcp_servers::mcp_server_test,
        mcp_servers::mcp_server_enable,
        mcp_servers::mcp_server_detail,
        mcp_copy::mcp_server_copy_preview,
        mcp_copy::mcp_server_copy_apply,
        bulk_skill::bulk_skill_apply,
        secrets::secrets_list,
        secrets::secrets_create,
        secrets::secrets_rotate,
        secrets::secrets_delete,
        overwrite::overwrite_move_aside,
        overwrite::overwrite_undo_move,
        overwrite::adopt_preview,
        overwrite::adopt_apply,
        overwrite::operation_undo,
        skill_wizard::skill_wizard_transition,
        skill_wizard::skill_scope_options,
        skill_wizard::skill_add_preview,
        skill_wizard::skill_template_preview,
        skill_wizard::skill_import_choose_folder,
        skill_wizard::skill_import_choose_zip,
        skill_wizard::skill_library_catalog,
        skill_wizard::skill_import_git_start,
        skill_wizard::skill_import_local_start,
        skill_wizard::skill_import_job_result,
        skill_wizard::skill_import_release,
        skill_wizard::skill_marketplace_search,
        skill_wizard::skill_add_apply_start,
        skill_wizard::skill_add_apply_result,
        skill_delete::skill_delete_preview,
        skill_delete::skill_delete_apply,
        skill_transfer::skill_transfer_options,
        skill_transfer::skill_transfer_preview,
        skill_transfer::skill_transfer_apply,
        skill_transfer::skill_disable_in_repo_preview,
        skill_transfer::skill_disable_in_repo_apply,
        skill_transfer::skill_transfer_bulk_preview,
        skill_transfer::skill_transfer_bulk_apply,
        migrate::migrate_conflicts,
        workflows::instructions_list,
        workflows::instruction_save,
        rule_create::instruction_create_options,
        rule_create::instruction_create_preview,
        rule_create::instruction_create,
        workflows::agents_list,
        workflows::agent_manage_set,
        workflows::agent_link_mode_set,
        workflows::agents_rescan,
        workflows::agent_path_set,
        workflows::agent_path_choose,
        agent_connect::agent_path_candidates,
        agent_connect::agent_path_validate,
        agent_connect::custom_agent_preview,
        agent_connect::custom_agent_create,
        workflows::presets_list,
        workflows::preset_create,
        workflows::preset_rename,
        workflows::preset_delete,
        workflows::preset_member_set,
        workflows::preset_member_candidates,
        preset_apply::preset_apply,
        preset_apply::preset_unapply,
        migrate::migrate_plan,
        migrate::migrate_apply,
        migrate::migrate_apply_result,
        migrate::operation_undo_check
    ]);
    #[cfg(not(debug_assertions))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::app_info,
        external_link::open_external_link,
        deep_link::deep_link_take,
        commands::app_signing_status,
        updater::commands::app_update_status,
        updater::commands::app_update_check,
        updater::commands::app_update_install_request,
        updater::commands::app_update_install_resume,
        updater::commands::app_update_install_cancel,
        menu::app_menu_set_add_skill_enabled,
        commands::jobs_list,
        commands::jobs_cancel,
        commands::cli_install_status,
        commands::cli_install,
        commands::cli_uninstall,
        commands::app_paths,
        commands::settings_load,
        effective::effective_query,
        effective::effective_counts,
        effective::why_trace,
        effective::effective_lock,
        effective::effective_unlock,
        migrate::migrate_scan,
        migrate::migrate_scan_start,
        migrate::migrate_scan_result,
        drift_commands::drift_snapshot,
        drift_commands::drift_rescan,
        drift_commands::drift_watcher_set_paused,
        drift_commands::drift_preview,
        drift_commands::drift_adopt,
        drift_commands::drift_restore,
        drift_commands::drift_open_in_editor,
        drift_commands::backups_list,
        drift_commands::backup_preview,
        drift_commands::backup_restore,
        migrate::migrate_conflicts,
        commands::settings_set,
        remote_backup::remote_backup_settings,
        remote_backup::remote_backup_configure,
        remote_backup::remote_backup_create_repo,
        remote_backup::actions::status,
        remote_backup::actions::backup_now,
        remote_backup::actions::sync_now,
        remote_backup::actions::restore_commits,
        remote_backup::actions::restore_confirmation,
        remote_backup::actions::restore,
        remote_backup::actions::conflicts_list,
        remote_backup::actions::conflicts_reveal,
        remote_backup::actions::conflicts_resolve_preview,
        remote_backup::actions::conflicts_resolve_apply,
        commands::recovery_sheet,
        commands::recovery_discard,
        commands::recovery_restore,
        overview::overview_load,
        overview::overview_enable_global_writes,
        overview::overview_setup_preview,
        overview::overview_setup_apply,
        privilege::global_store_overview,
        privilege::global_enable_writes,
        commands::item_lookup,
        skill_detail::skill_detail_load,
        skill_agent::skill_agent_preview,
        skill_agent::skill_agent_apply,
        skill_agent::skill_agent_discard,
        skills_list::skills_list,
        skill_detail::skill_auto_update_set,
        skill_detail::skill_update_ignore,
        source_updates::skill_source_check,
        source_updates::skill_update_preview,
        source_updates::skill_update_apply,
        skill_detail::skill_compare_copies,
        skill_detail::skill_version_compare,
        skill_detail::skill_reveal,
        skill_detail::reveal_item,
        skill_detail::skill_edit_preview,
        skill_detail::skill_edit_apply,
        skill_detail::skill_keep_copy_preview,
        skill_detail::skill_keep_copy_apply,
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
        network::network_clear_events,
        network::network_set_offline,
        config::config_report,
        mcp_form::mcp_plan_preview,
        mcp_form::mcp_save,
        mcp_form::mcp_test_draft,
        mcp_form::mcp_launch_prepare,
        mcp_servers::mcp_server_list,
        mcp_servers::mcp_server_disable,
        mcp_servers::mcp_server_delete,
        mcp_servers::mcp_server_test,
        mcp_servers::mcp_server_enable,
        mcp_servers::mcp_server_detail,
        mcp_copy::mcp_server_copy_preview,
        mcp_copy::mcp_server_copy_apply,
        bulk_skill::bulk_skill_apply,
        secrets::secrets_list,
        secrets::secrets_create,
        secrets::secrets_rotate,
        secrets::secrets_delete,
        overwrite::overwrite_move_aside,
        overwrite::overwrite_undo_move,
        overwrite::adopt_preview,
        overwrite::adopt_apply,
        overwrite::operation_undo,
        skill_wizard::skill_wizard_transition,
        skill_wizard::skill_scope_options,
        skill_wizard::skill_template_preview,
        skill_wizard::skill_add_preview,
        skill_wizard::skill_import_choose_folder,
        skill_wizard::skill_import_choose_zip,
        skill_wizard::skill_import_local_start,
        skill_wizard::skill_library_catalog,
        skill_wizard::skill_marketplace_search,
        skill_wizard::skill_import_git_start,
        skill_wizard::skill_import_job_result,
        skill_wizard::skill_import_release,
        skill_wizard::skill_add_apply_start,
        skill_wizard::skill_add_apply_result,
        skill_delete::skill_delete_preview,
        skill_delete::skill_delete_apply,
        skill_transfer::skill_transfer_options,
        skill_transfer::skill_transfer_preview,
        skill_transfer::skill_transfer_apply,
        skill_transfer::skill_disable_in_repo_preview,
        skill_transfer::skill_disable_in_repo_apply,
        skill_transfer::skill_transfer_bulk_preview,
        skill_transfer::skill_transfer_bulk_apply,
        workflows::instructions_list,
        workflows::instruction_save,
        rule_create::instruction_create_options,
        rule_create::instruction_create_preview,
        rule_create::instruction_create,
        workflows::agents_list,
        workflows::agent_manage_set,
        workflows::agent_link_mode_set,
        workflows::agents_rescan,
        workflows::agent_path_set,
        workflows::agent_path_choose,
        agent_connect::agent_path_candidates,
        agent_connect::agent_path_validate,
        agent_connect::custom_agent_preview,
        agent_connect::custom_agent_create,
        workflows::presets_list,
        workflows::preset_create,
        workflows::preset_rename,
        workflows::preset_delete,
        workflows::preset_member_set,
        workflows::preset_member_candidates,
        preset_apply::preset_apply,
        preset_apply::preset_unapply,
        migrate::migrate_plan,
        migrate::migrate_apply,
        migrate::migrate_apply_result,
        migrate::operation_undo_check
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
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &event {
            if let Some(relay) = handle.try_state::<deep_link::DeepLinkRelay>() {
                relay.deliver(handle, urls);
            } else {
                tracing::warn!("deep link arrived before the relay was managed");
            }
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

/// Creates the `main` webview from `tauri.conf.json` with the navigation guard attached.
///
/// The window declares `"create": false`, so Tauri leaves it to this function: the builder's
/// navigation and new-window hooks are the only seam where a webview can be confined to the app
/// origin, and a config-created window has none. Everything else in the window config (size,
/// title, label) still comes from the config file.
fn create_main_window<R: tauri::Runtime>(
    app: &mut tauri::App<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .cloned()
        .ok_or_else(|| std::io::Error::other("tauri.conf.json defines no `main` window"))?;
    tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
        .on_navigation(external_link::navigation_allowed)
        .on_new_window(|_url, _features| {
            tracing::warn!(
                "refused a new webview window; external links open in the system browser"
            );
            tauri::webview::NewWindowResponse::<R>::Deny
        })
        .build()?;
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
