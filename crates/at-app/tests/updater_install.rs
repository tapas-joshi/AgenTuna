//! TF-032 install path: Team ID policy, bundle swap, rollback, relaunch and the deferred queue.
//!
//! Everything here runs offline in temporary directories. The signature verifier and the
//! relauncher are fakes, and the swap failpoint simulates a rename that fails halfway.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use at_app::app::updater::install::{
    BundleInstaller, BundleSwapReconcile, Clock, DeferredInstall, IdleSignal, InstallDeferReason,
    InstallError, InstallOutcome, InstallRequest, RelaunchError, Relauncher, RunningApp,
    SignatureError, SignatureVerifier, UpdateInstallStatus, check_team_id, install_gate,
    parse_team_identifier, reconcile_bundle_swap,
};
use at_fs::bundle::{BundleSwapPhase, read_bundle_swap_journal};
use at_fs::failpoint::{Fault, FaultPoint, TestInjector, with_injector};

/// Verifier that answers with a fixed Team ID and records what it was asked.
struct FakeVerifier {
    team_id: Option<String>,
    seen: Mutex<Vec<PathBuf>>,
}

impl FakeVerifier {
    fn new(team_id: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            team_id: team_id.map(str::to_owned),
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl SignatureVerifier for FakeVerifier {
    fn team_id(&self, bundle: &Path) -> Result<Option<String>, SignatureError> {
        self.seen
            .lock()
            .expect("verifier log")
            .push(bundle.to_path_buf());
        Ok(self.team_id.clone())
    }
}

/// Relauncher that records launches and can be told to fail.
struct FakeRelauncher {
    launched: Mutex<Vec<PathBuf>>,
    fail: AtomicBool,
}

impl FakeRelauncher {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            launched: Mutex::new(Vec::new()),
            fail: AtomicBool::new(fail),
        })
    }

    fn launched(&self) -> Vec<PathBuf> {
        self.launched.lock().expect("launch log").clone()
    }
}

impl Relauncher for FakeRelauncher {
    fn relaunch(&self, bundle: &Path) -> Result<(), RelaunchError> {
        if self.fail.load(Ordering::Acquire) {
            return Err(RelaunchError::Failed {
                status: "exit status: 1".to_owned(),
            });
        }
        self.launched
            .lock()
            .expect("launch log")
            .push(bundle.to_path_buf());
        Ok(())
    }
}

struct FakeClock(i64);

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> i64 {
        self.0
    }
}

struct FakeIdle(Option<InstallDeferReason>);

impl IdleSignal for FakeIdle {
    fn defer_reason(&self) -> Option<InstallDeferReason> {
        self.0
    }
}

struct Fixture {
    root: tempfile::TempDir,
    running: RunningApp,
    verifier: Arc<FakeVerifier>,
    relauncher: Arc<FakeRelauncher>,
    installer: BundleInstaller,
    request: InstallRequest,
}

fn archive_with_app(path: &Path, marker: &str) {
    let file = fs::File::create(path).expect("archive file");
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    writer
        .add_directory("AgenTuna.app/", options)
        .expect("dir entry");
    writer
        .start_file("AgenTuna.app/Contents/marker", options)
        .expect("marker entry");
    writer.write_all(marker.as_bytes()).expect("marker bytes");
    let executable = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    writer
        .start_file("AgenTuna.app/Contents/MacOS/agentuna-app", executable)
        .expect("executable entry");
    writer.write_all(b"binary").expect("binary bytes");
    writer.finish().expect("finish archive");
}

fn fixture(staged_team: Option<&str>, running_team: Option<&str>, relaunch_fails: bool) -> Fixture {
    let root = tempfile::tempdir().expect("temporary root");
    let bundle = root.path().join("Applications/AgenTuna.app");
    fs::create_dir_all(bundle.join("Contents")).expect("installed bundle");
    fs::write(bundle.join("Contents/marker"), b"previous").expect("old marker");
    let updates = root.path().join("cache/updates");
    fs::create_dir_all(&updates).expect("update cache");
    let archive = updates.join("0.2.0.zip");
    archive_with_app(&archive, "new");
    let verifier = FakeVerifier::new(staged_team);
    let relauncher = FakeRelauncher::new(relaunch_fails);
    let installer = BundleInstaller::new(
        verifier.clone(),
        relauncher.clone(),
        Arc::new(FakeClock(1_700_000_000_000)),
    );
    Fixture {
        request: InstallRequest {
            version: "0.2.0".to_owned(),
            archive,
            staging_root: root.path().join("staging"),
            rollback_root: updates.join("rollback"),
            journal: updates.join("bundle-swap.json"),
        },
        running: RunningApp {
            version: "0.1.0".to_owned(),
            bundle,
            team_id: running_team.map(str::to_owned),
        },
        verifier,
        relauncher,
        installer,
        root,
    }
}

fn marker(bundle: &Path) -> String {
    fs::read_to_string(bundle.join("Contents/marker")).expect("bundle marker")
}

#[test]
fn a_matching_team_id_installs_and_keeps_the_previous_bundle() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let outcome = fixture
        .installer
        .install(&fixture.request, &fixture.running)
        .expect("install succeeds");

    assert_eq!(marker(&fixture.running.bundle), "new");
    assert_eq!(marker(&outcome.rollback), "previous");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fixture.running.bundle.join("Contents/MacOS/agentuna-app"))
            .expect("installed executable")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit survives extraction");
    }
    assert_eq!(
        fixture.relauncher.launched(),
        vec![fixture.running.bundle.clone()]
    );
    let journal = read_bundle_swap_journal(&fixture.request.journal)
        .expect("journal readable")
        .expect("journal present");
    assert_eq!(journal.phase, BundleSwapPhase::Installed);
    assert_eq!(journal.installed_version.as_deref(), Some("0.2.0"));
    assert_eq!(journal.previous_version.as_deref(), Some("0.1.0"));
    // The verifier looked at the extracted bundle, not the archive.
    assert_eq!(fixture.verifier.seen.lock().expect("verifier log").len(), 1);
}

#[test]
fn a_different_team_id_is_refused_before_anything_moves() {
    let fixture = fixture(Some("OTHER"), Some("TEAM123"), false);
    let result = fixture
        .installer
        .install(&fixture.request, &fixture.running);
    assert!(matches!(result, Err(InstallError::TeamIdMismatch { .. })));
    assert_eq!(marker(&fixture.running.bundle), "previous");
    assert!(fixture.relauncher.launched().is_empty());
    assert!(
        read_bundle_swap_journal(&fixture.request.journal)
            .expect("journal readable")
            .is_none()
    );
}

#[test]
fn an_ad_hoc_running_app_refuses_every_install() {
    let ad_hoc = fixture(Some("TEAM123"), None, false);
    let result = ad_hoc.installer.install(&ad_hoc.request, &ad_hoc.running);
    assert!(matches!(result, Err(InstallError::RunningUnverified)));
    assert_eq!(marker(&ad_hoc.running.bundle), "previous");

    let unsigned = fixture(None, Some("TEAM123"), false);
    let result = unsigned
        .installer
        .install(&unsigned.request, &unsigned.running);
    assert!(matches!(result, Err(InstallError::StagedUnsigned)));
}

#[test]
fn the_team_id_policy_is_same_identity_only() {
    assert!(check_team_id(Some("A"), Some("A")).is_ok());
    assert!(matches!(
        check_team_id(Some("A"), Some("B")),
        Err(InstallError::TeamIdMismatch { .. })
    ));
    assert!(matches!(
        check_team_id(Some("A"), None),
        Err(InstallError::StagedUnsigned)
    ));
    assert!(matches!(
        check_team_id(None, Some("A")),
        Err(InstallError::RunningUnverified)
    ));
    assert!(matches!(
        check_team_id(None, None),
        Err(InstallError::RunningUnverified)
    ));
}

#[test]
fn codesign_output_maps_to_a_team_id_or_none() {
    assert_eq!(
        parse_team_identifier("Executable=x\nTeamIdentifier=TEAM123\n"),
        Some("TEAM123".to_owned())
    );
    assert_eq!(parse_team_identifier("TeamIdentifier=not set"), None);
    assert_eq!(
        parse_team_identifier("Signature=adhoc\nTeamIdentifier=TEAM123"),
        None
    );
}

#[test]
fn a_failed_swap_rename_leaves_the_running_bundle_intact() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let injector = Arc::new(TestInjector::new());
    injector.arm(
        FaultPoint::BundleSwapInstall,
        Fault::Fail(rustix::io::Errno::ACCESS.raw_os_error()),
    );

    let result = with_injector(injector, || {
        fixture
            .installer
            .install(&fixture.request, &fixture.running)
    });

    assert!(matches!(result, Err(InstallError::Swap(_))));
    assert_eq!(marker(&fixture.running.bundle), "previous");
    assert!(fixture.relauncher.launched().is_empty());
    assert!(
        read_bundle_swap_journal(&fixture.request.journal)
            .expect("journal readable")
            .is_none()
    );
}

#[test]
fn a_failed_relaunch_restores_the_previous_bundle() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), true);
    let result = fixture
        .installer
        .install(&fixture.request, &fixture.running);
    assert!(matches!(
        result,
        Err(InstallError::Relaunch {
            rolled_back: true,
            ..
        })
    ));
    assert_eq!(marker(&fixture.running.bundle), "previous");
    assert!(
        read_bundle_swap_journal(&fixture.request.journal)
            .expect("journal readable")
            .is_none()
    );
}

#[test]
fn a_traversal_entry_is_refused_before_extraction_escapes() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let file = fs::File::create(&fixture.request.archive).expect("archive file");
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    writer
        .start_file("../escape.txt", options)
        .expect("traversal entry");
    writer.write_all(b"x").expect("payload");
    writer.finish().expect("finish archive");

    let result = fixture
        .installer
        .install(&fixture.request, &fixture.running);
    assert!(matches!(result, Err(InstallError::UnsafeEntry { .. })));
    assert!(!fixture.root.path().join("staging/escape.txt").exists());
    assert!(!fixture.root.path().join("escape.txt").exists());
}

#[test]
fn a_healthy_launch_commits_and_a_failed_one_rolls_back() {
    let committed = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let outcome = committed
        .installer
        .install(&committed.request, &committed.running)
        .expect("install succeeds");
    let reconcile = reconcile_bundle_swap(&committed.request.journal, "0.2.0").expect("reconcile");
    assert_eq!(
        reconcile,
        BundleSwapReconcile::Committed {
            version: "0.2.0".to_owned()
        }
    );
    assert!(!outcome.rollback.exists());
    assert!(
        read_bundle_swap_journal(&committed.request.journal)
            .expect("journal readable")
            .is_none()
    );

    let rolled = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let _outcome = rolled
        .installer
        .install(&rolled.request, &rolled.running)
        .expect("install succeeds");
    let reconcile = reconcile_bundle_swap(&rolled.request.journal, "0.1.0").expect("reconcile");
    assert_eq!(
        reconcile,
        BundleSwapReconcile::RolledBack {
            version: Some("0.2.0".to_owned())
        }
    );
    assert_eq!(marker(&rolled.running.bundle), "previous");
}

#[test]
fn a_run_during_an_unrelated_version_is_left_for_manual_review() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let outcome = fixture
        .installer
        .install(&fixture.request, &fixture.running)
        .expect("install succeeds");
    let reconcile = reconcile_bundle_swap(&fixture.request.journal, "9.9.9").expect("reconcile");
    assert!(matches!(
        reconcile,
        BundleSwapReconcile::ManualReview { .. }
    ));
    assert!(outcome.rollback.exists());
}

#[test]
fn a_queued_install_survives_a_restart_and_waits_for_idle() {
    let root = tempfile::tempdir().expect("temporary root");
    let path = root.path().join("updates/install-queue.json");
    let clock = Arc::new(FakeClock(1_700_000_000_000));
    let queue = DeferredInstall::load(path.clone(), clock.clone());

    let queued = queue
        .enqueue("0.2.0", InstallDeferReason::JobRunning)
        .expect("enqueue persists");
    assert_eq!(queued.version, "0.2.0");
    assert_eq!(queued.requested_at_unix_ms, 1_700_000_000_000);

    let restarted = DeferredInstall::load(path.clone(), clock);
    assert_eq!(
        restarted
            .pending()
            .expect("record survives a restart")
            .version,
        "0.2.0"
    );
    assert!(
        restarted
            .take_if_idle(&FakeIdle(Some(InstallDeferReason::JobRunning)))
            .expect("busy check")
            .is_none()
    );
    assert!(restarted.pending().is_some());
    let taken = restarted
        .take_if_idle(&FakeIdle(None))
        .expect("idle check")
        .expect("queued record");
    assert_eq!(taken.version, "0.2.0");
    assert!(restarted.pending().is_none());
    assert!(!path.exists());
}

#[test]
fn cancel_clears_the_queue_and_a_corrupt_record_is_ignored() {
    let root = tempfile::tempdir().expect("temporary root");
    let path = root.path().join("install-queue.json");
    let queue = DeferredInstall::load(path.clone(), Arc::new(FakeClock(1)));
    queue
        .enqueue("0.3.0", InstallDeferReason::EditorDirty)
        .expect("enqueue");
    assert!(queue.cancel().expect("cancel"));
    assert!(!queue.cancel().expect("cancel is idempotent"));
    assert!(!path.exists());

    fs::write(&path, b"not json").expect("corrupt record");
    let reloaded = DeferredInstall::load(path, Arc::new(FakeClock(1)));
    assert!(reloaded.pending().is_none());
}

#[test]
fn the_gate_offers_install_only_for_a_staged_update() {
    let ready = install_gate(
        UpdateInstallStatus::Ready {
            version: "0.2.0".to_owned(),
        },
        Some(InstallDeferReason::ModalOpen),
    );
    assert!(ready.can_install);
    assert!(!ready.can_cancel);
    assert_eq!(ready.defer_reason, Some(InstallDeferReason::ModalOpen));

    let installing = install_gate(
        UpdateInstallStatus::Installing {
            version: "0.2.0".to_owned(),
        },
        None,
    );
    assert!(!installing.can_install);
    assert!(!installing.can_cancel);

    let queued = install_gate(
        UpdateInstallStatus::Queued {
            version: "0.2.0".to_owned(),
            reason: InstallDeferReason::EditorDirty,
            requested_at: "2026-11-07T00:00:00Z".to_owned(),
        },
        None,
    );
    assert!(!queued.can_install);
    assert!(queued.can_cancel);

    assert!(!install_gate(UpdateInstallStatus::Idle, None).can_install);
}

#[test]
fn a_successful_install_outcome_names_the_rollback_copy() {
    let fixture = fixture(Some("TEAM123"), Some("TEAM123"), false);
    let InstallOutcome {
        version,
        bundle,
        rollback,
        journal,
    } = fixture
        .installer
        .install(&fixture.request, &fixture.running)
        .expect("install succeeds");
    assert_eq!(version, "0.2.0");
    assert_eq!(bundle, fixture.running.bundle);
    assert!(rollback.starts_with(fixture.request.rollback_root.clone()));
    assert_eq!(journal, fixture.request.journal);
}
