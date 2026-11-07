//! Tauri commands for checking, inspecting, installing and cancelling app updates.
//!
//! The webview receives only typed gates and statuses. Archives are addressed by version and
//! resolved under the update cache, never by a path the webview supplies.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use at_db::ops::{NewOperation, OperationActor, OperationKind, OperationStatus};
use tauri::{Emitter, Manager, State};

use super::install::{
    BundleInstaller, BundleSwapReconcile, Clock, CodesignVerifier, DeferredInstall,
    InstallDeferReason, InstallError, InstallOutcome, InstallRequest, OpenRelauncher, Relauncher,
    RunningApp, SignatureError, SignatureVerifier, StaticIdle, SystemClock, UpdateInstallGate,
    UpdateInstallStatus, format_queue_time, install_gate, reconcile_bundle_swap, validate_version,
};
use super::{UpdateError, UpdateManifestError, UpdateOutcome, check_and_stage, manifest_url};
use crate::app::bridge::JobState;
use crate::app::commands::{CommandError, CommandErrorCode};
use crate::app::network::NetworkState;
use crate::app::state::AppState;

/// Webview event emitted once an install attempt reaches a terminal status.
pub const EVENT_UPDATE_INSTALL_FINISHED: &str = "agentuna:update-install-finished";
/// File name of the persisted install queue inside the update cache.
pub const QUEUE_FILE: &str = "install-queue.json";
/// File name of the bundle-swap journal inside the update cache.
pub const JOURNAL_FILE: &str = "bundle-swap.json";
/// Directory that keeps the previous bundle until the new one reports healthy.
pub const ROLLBACK_DIR: &str = "rollback";

/// Update-install state shared by the webview commands and the startup health check.
pub struct UpdaterState {
    updates_dir: PathBuf,
    status: Mutex<UpdateInstallStatus>,
    queue: DeferredInstall,
    installer: BundleInstaller,
    verifier: Arc<dyn SignatureVerifier>,
}

impl UpdaterState {
    /// Builds state around explicit seams, for tests and non-default tools.
    #[must_use]
    pub fn with_seams(
        updates_dir: PathBuf,
        verifier: Arc<dyn SignatureVerifier>,
        relauncher: Arc<dyn Relauncher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let queue = DeferredInstall::load(updates_dir.join(QUEUE_FILE), Arc::clone(&clock));
        let installer = BundleInstaller::new(
            Arc::clone(&verifier),
            Arc::clone(&relauncher),
            Arc::clone(&clock),
        );
        Self {
            updates_dir,
            status: Mutex::new(UpdateInstallStatus::Idle),
            queue,
            installer,
            verifier,
        }
    }

    /// Builds production state: `/usr/bin/codesign`, `/usr/bin/open` and the system clock.
    #[must_use]
    pub fn system(updates_dir: PathBuf) -> Self {
        Self::with_seams(
            updates_dir,
            Arc::new(CodesignVerifier::system()),
            Arc::new(OpenRelauncher::system()),
            Arc::new(SystemClock),
        )
    }

    /// The update cache directory (`<cache>/updates`).
    #[must_use]
    pub fn updates_dir(&self) -> &Path {
        &self.updates_dir
    }

    /// The current install status.
    #[must_use]
    pub fn status(&self) -> UpdateInstallStatus {
        lock(&self.status).clone()
    }

    /// Replaces the current install status.
    pub fn set_status(&self, status: UpdateInstallStatus) {
        *lock(&self.status) = status;
    }

    /// The persisted deferred queue.
    #[must_use]
    pub fn queue(&self) -> &DeferredInstall {
        &self.queue
    }

    /// The bundle installer.
    #[must_use]
    pub fn installer(&self) -> &BundleInstaller {
        &self.installer
    }

    /// The signature seam, also used to classify the running bundle.
    #[must_use]
    pub fn verifier(&self) -> &dyn SignatureVerifier {
        self.verifier.as_ref()
    }

    /// Journal file of the last swap.
    #[must_use]
    pub fn journal_path(&self) -> PathBuf {
        self.updates_dir.join(JOURNAL_FILE)
    }

    /// Directory holding retained previous bundles.
    #[must_use]
    pub fn rollback_root(&self) -> PathBuf {
        self.updates_dir.join(ROLLBACK_DIR)
    }

    /// Resolves a staged archive by version and refuses anything outside the update cache.
    ///
    /// # Errors
    ///
    /// [`InstallError::InvalidVersion`] for an unsafe version, [`InstallError::ArchiveMissing`]
    /// when no archive was staged for it.
    pub fn artifact_path(&self, version: &str) -> Result<PathBuf, InstallError> {
        let path = self.updates_dir.join(format!("{version}.zip"));
        validate_version(version)?;
        if !path.is_file() {
            return Err(InstallError::ArchiveMissing(path));
        }
        Ok(path)
    }

    /// Reconciles a journal left by a previous swap against the running version.
    ///
    /// # Errors
    ///
    /// [`at_fs::bundle::BundleSwapError`] when the reconciliation cannot finish; the journal stays
    /// on disk for a retry.
    pub fn reconcile(
        &self,
        running_version: &str,
    ) -> Result<BundleSwapReconcile, at_fs::bundle::BundleSwapError> {
        reconcile_bundle_swap(&self.journal_path(), running_version)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reports the install gate, merging the persisted queue into the reported status.
#[tauri::command]
pub fn app_update_status(updater: State<'_, UpdaterState>) -> UpdateInstallGate {
    install_gate(effective_status(&updater), None)
}

/// Checks the signed release manifest and stages a newer verified archive.
///
/// # Errors
///
/// [`CommandError`] when the build has no update source, the network policy refuses the request,
/// the manifest or archive fails verification, or staging fails.
#[tauri::command]
pub async fn app_update_check(
    app: State<'_, AppState>,
    network: State<'_, NetworkState>,
    updater: State<'_, UpdaterState>,
) -> Result<UpdateInstallGate, CommandError> {
    let Some(endpoint) = manifest_url() else {
        return Err(CommandError::unavailable(
            "this build has no update source configured",
        ));
    };
    let network = network.inner().clone();
    let updates_dir = updater.updates_dir().to_path_buf();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| CommandError::internal(format!("update client: {error}")))?;
    at_mcp::ensure_crypto_provider();
    let outcome = match check_and_stage(
        &network,
        &client,
        endpoint,
        env!("CARGO_PKG_VERSION"),
        false,
        &updates_dir,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            record_operation(&app, OperationStatus::Failed, "Update check failed");
            return Err(map_update_error(error));
        }
    };
    let status = match outcome {
        UpdateOutcome::Staged { version, .. } => {
            record_operation(
                &app,
                OperationStatus::Succeeded,
                &format!("Staged update {version}"),
            );
            UpdateInstallStatus::Ready { version }
        }
        UpdateOutcome::Current => {
            record_operation(&app, OperationStatus::Succeeded, "No update available");
            UpdateInstallStatus::Idle
        }
    };
    updater.set_status(status.clone());
    Ok(install_gate(status, None))
}

/// Requests an install of the staged `version`, queueing it when the app is busy.
///
/// `editor_dirty` and `modal_open` come from the webview because only it knows its own state; jobs
/// and the operation-wide database lock are checked here.
///
/// # Errors
///
/// [`CommandError`] when no archive is staged for `version`, an install is already running, or the
/// queue record cannot be written.
#[tauri::command]
pub fn app_update_install_request(
    app: State<'_, AppState>,
    updater: State<'_, UpdaterState>,
    app_handle: tauri::AppHandle,
    version: String,
    editor_dirty: bool,
    modal_open: bool,
) -> Result<UpdateInstallGate, CommandError> {
    updater.artifact_path(&version).map_err(map_install_error)?;
    if matches!(
        updater.status(),
        UpdateInstallStatus::Installing { .. } | UpdateInstallStatus::Installed { .. }
    ) {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "an update install is already running",
        ));
    }
    let reason = activity(&app, editor_dirty, modal_open);
    if let Some(reason) = reason {
        let queued = updater
            .queue()
            .enqueue(&version, reason)
            .map_err(map_install_error)?;
        return Ok(install_gate(queued_status(&queued), None));
    }
    start_install(&app_handle, &version);
    Ok(install_gate(
        UpdateInstallStatus::Installing { version },
        None,
    ))
}

/// Re-evaluates the deferred queue now that the webview may be idle.
///
/// # Errors
///
/// [`CommandError`] when the queue record cannot be read or cleared.
#[tauri::command]
pub fn app_update_install_resume(
    app: State<'_, AppState>,
    updater: State<'_, UpdaterState>,
    app_handle: tauri::AppHandle,
    editor_dirty: bool,
    modal_open: bool,
) -> Result<UpdateInstallGate, CommandError> {
    let reason = activity(&app, editor_dirty, modal_open);
    if matches!(
        updater.status(),
        UpdateInstallStatus::Installing { .. } | UpdateInstallStatus::Installed { .. }
    ) {
        return Ok(install_gate(effective_status(&updater), reason));
    }
    let idle = StaticIdle::new(reason);
    let queued = updater
        .queue()
        .take_if_idle(&idle)
        .map_err(map_install_error)?;
    match queued {
        Some(queued) => {
            start_install(&app_handle, &queued.version);
            Ok(install_gate(
                UpdateInstallStatus::Installing {
                    version: queued.version,
                },
                None,
            ))
        }
        None => Ok(install_gate(effective_status(&updater), reason)),
    }
}

/// Cancels a queued install request.
///
/// # Errors
///
/// [`CommandError`] when the install is already running or the queue record cannot be removed.
#[tauri::command]
pub fn app_update_install_cancel(
    updater: State<'_, UpdaterState>,
) -> Result<UpdateInstallGate, CommandError> {
    if matches!(updater.status(), UpdateInstallStatus::Installing { .. }) {
        return Err(CommandError::new(
            CommandErrorCode::Conflict,
            "the update is already installing",
        ));
    }
    updater.queue().cancel().map_err(map_install_error)?;
    Ok(install_gate(effective_status(&updater), None))
}

fn effective_status(updater: &UpdaterState) -> UpdateInstallStatus {
    let current = updater.status();
    // A pending request wins over every resting status: it is about to be retried, so the UI must
    // show the queue rather than a stale failure or a previous ready state.
    if matches!(
        current,
        UpdateInstallStatus::Installing { .. } | UpdateInstallStatus::Installed { .. }
    ) {
        return current;
    }
    updater
        .queue()
        .pending()
        .map_or(current, |queued| queued_status(&queued))
}

fn queued_status(queued: &super::install::QueuedInstall) -> UpdateInstallStatus {
    UpdateInstallStatus::Queued {
        version: queued.version.clone(),
        reason: queued.reason,
        requested_at: format_queue_time(queued.requested_at_unix_ms),
    }
}

/// Starts the blocking install on the async runtime and returns immediately.
fn start_install(app_handle: &tauri::AppHandle, version: &str) {
    let updater = app_handle.state::<UpdaterState>();
    updater.set_status(UpdateInstallStatus::Installing {
        version: version.to_owned(),
    });
    let handle = app_handle.clone();
    let version = version.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let updater = handle.state::<UpdaterState>();
        let app = handle.state::<AppState>();
        let status = install_blocking(&updater, &app, &version);
        updater.set_status(status.clone());
        if let Err(error) = handle.emit(EVENT_UPDATE_INSTALL_FINISHED, &status) {
            tracing::warn!(%error, "could not emit the update install result");
        }
        if matches!(status, UpdateInstallStatus::Installed { .. }) {
            // The new copy is already running as the primary instance; this process only has to
            // get out of its way.
            handle.exit(0);
        }
    });
}

fn install_blocking(updater: &UpdaterState, app: &AppState, version: &str) -> UpdateInstallStatus {
    match run_install(updater, app, version) {
        Ok(outcome) => {
            record_operation(
                app,
                OperationStatus::Succeeded,
                &format!("Updated AgenTuna to {}", outcome.version),
            );
            UpdateInstallStatus::Installed {
                version: outcome.version,
            }
        }
        Err(error) => {
            record_operation(app, OperationStatus::Failed, "Update install failed");
            status_for_error(version, error)
        }
    }
}

fn run_install(
    updater: &UpdaterState,
    app: &AppState,
    version: &str,
) -> Result<InstallOutcome, InstallError> {
    let archive = updater.artifact_path(version)?;
    let running = running_app(updater)?;
    let request = InstallRequest {
        version: version.to_owned(),
        archive,
        staging_root: std::env::temp_dir(),
        rollback_root: updater.rollback_root(),
        journal: updater.journal_path(),
    };
    let outcome = updater.installer().prepare_and_swap(&request, &running)?;
    // Release the single-instance record so the relaunched copy becomes primary instead of
    // focusing this process while it exits.
    if let Some(instance) = app.instance() {
        instance.shutdown();
    }
    updater.installer().relaunch(&outcome)?;
    Ok(outcome)
}

fn running_app(updater: &UpdaterState) -> Result<RunningApp, InstallError> {
    let executable = std::env::current_exe().map_err(InstallError::Io)?;
    let bundle = RunningApp::bundle_for_executable(&executable).ok_or(InstallError::NotBundled)?;
    let team_id = match updater.verifier().team_id(&bundle) {
        Ok(team_id) => team_id,
        Err(SignatureError::NotSigned { .. }) => None,
        Err(error) => return Err(InstallError::Signature(error)),
    };
    Ok(RunningApp {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        bundle,
        team_id,
    })
}

fn status_for_error(version: &str, error: InstallError) -> UpdateInstallStatus {
    let message = error.to_string();
    match error {
        InstallError::Relaunch { rolled_back, .. } if rolled_back => UpdateInstallStatus::RolledBack {
            version: version.to_owned(),
            reason: message,
            remedy: "The previous version was restored. Quit and reopen AgenTuna, then retry the update.".to_owned(),
        },
        other => UpdateInstallStatus::Failed {
            version: Some(version.to_owned()),
            message: other.to_string(),
            remedy: remedy_for(&other),
            rolled_back: false,
        },
    }
}

fn remedy_for(error: &InstallError) -> String {
    match error {
        InstallError::Signature(_)
        | InstallError::TeamIdMismatch { .. }
        | InstallError::StagedUnsigned
        | InstallError::RunningUnverified
        | InstallError::NotBundled => {
            "This build cannot be updated automatically. Download the release from the project page and install it manually.".to_owned()
        }
        InstallError::Archive(_)
        | InstallError::ArchiveMissing(_)
        | InstallError::NoBundle
        | InstallError::UnsafeEntry { .. }
        | InstallError::ArchiveTooLarge => {
            "Delete the downloaded update and run Check for Updates again.".to_owned()
        }
        InstallError::Swap(error) if error.previous_bundle_intact() => {
            "The current version is untouched. Retry later; if it keeps failing, install the release manually.".to_owned()
        }
        _ => "Nothing was installed. Check the logs and retry; the previous bundle is kept in the update cache.".to_owned(),
    }
}

fn record_operation(app: &AppState, status: OperationStatus, summary: &str) {
    let Ok(database) = app.database_handle() else {
        return;
    };
    let Ok(database) = database.lock() else {
        return;
    };
    let new =
        NewOperation::new(OperationKind::Update, OperationActor::Updater).with_summary(summary);
    let error_kind = (status == OperationStatus::Failed).then_some("update_install_failed");
    if let Err(error) = at_db::ops::record(database.connection(), &new, &[], status, error_kind) {
        tracing::warn!(%error, "could not record the update operation");
    }
}

/// The first activity that must finish before an install may start.
fn activity(app: &AppState, editor_dirty: bool, modal_open: bool) -> Option<InstallDeferReason> {
    if app
        .bridge()
        .jobs()
        .iter()
        .any(|job| job.state == JobState::Running)
    {
        return Some(InstallDeferReason::JobRunning);
    }
    // A command that is mid-write holds the shared database lock; that is this process's
    // observable sign of a deploy or migration operation in flight.
    if app
        .database_handle()
        .ok()
        .is_some_and(|database| database.try_lock().is_err())
    {
        return Some(InstallDeferReason::OperationRunning);
    }
    if modal_open {
        return Some(InstallDeferReason::ModalOpen);
    }
    if editor_dirty {
        return Some(InstallDeferReason::EditorDirty);
    }
    None
}

fn map_update_error(error: UpdateError) -> CommandError {
    match error {
        UpdateError::Network(_) => {
            CommandError::unavailable("the network policy refused the update request")
        }
        UpdateError::Manifest(UpdateManifestError::Downgrade) => CommandError::new(
            CommandErrorCode::Conflict,
            "the offered version is older than the running version",
        ),
        UpdateError::Manifest(UpdateManifestError::SignatureInvalid) => CommandError::new(
            CommandErrorCode::InvalidArgument,
            "the update manifest signature is invalid; nothing was downloaded",
        ),
        other => CommandError::internal(other.to_string()),
    }
}

fn map_install_error(error: InstallError) -> CommandError {
    match error {
        InstallError::InvalidVersion { .. } => {
            CommandError::invalid_argument("version", "expected a release version")
        }
        InstallError::ArchiveMissing(_) => CommandError::new(
            CommandErrorCode::NotFound,
            "no staged archive exists for that version",
        ),
        other => CommandError::internal(other.to_string()),
    }
}
