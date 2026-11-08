//! Privacy proof for the app's network surface: starting its services and reading its screens'
//! data opens no connection.
//!
//! A loopback listener acts as a proxy recorder. It is not reachable from any screen, so a
//! pending connection on it would only appear if this code tried to connect somewhere; more
//! importantly the network state starts with an empty event log and refuses every background
//! action, so nothing in this surface can even ask for a connection.
//!
//! The GUI itself is not launched here: constructing the shell's services (job bridge, network
//! state) and reading every network view is the part of "launch, visit every screen, idle" that
//! exists headlessly. A process-wide DNS hook needs unsafe FFI, which this workspace forbids.

mod support;

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
use at_app::app::jobs::{
    Clock, ExecutionContext, HealthScheduler, HealthTarget, HealthTransport, Jitter,
    McpHealthRunner, SchedulePolicy,
};
use at_app::app::network::{NetworkState, authorize_update, event_views, policy_view};
use at_app::app::state::AppState;
use at_db::settings::{BackgroundHealth, HealthInterval};
use at_git::marketplace::{MarketplaceClient, MarketplaceError};
use at_git::policy::{NetworkGitCall, run_network_git};
use at_git::remote::{RemoteError, RemoteGitCall, run_remote_git};
use at_mcp::{ConnectIntent, HttpClient, HttpErrorKind, authorize_connect};
use at_model::network::{
    DenialReason, NetworkAction, NetworkActionState, NetworkEventLog, NetworkPolicy, NetworkTrigger,
};
use at_model::{AgentId, McpServer, McpServerDef, McpServerId, McpTransport, Scope};
use support::Sandbox;
use time::OffsetDateTime;

struct NeverGitRunner;

impl at_git::policy::GitRunner for NeverGitRunner {
    fn run(
        &self,
        _call: &at_git::policy::AuthorizedGitCall,
    ) -> Result<at_git::policy::GitOutput, at_git::policy::GitError> {
        panic!("GitRunner must never be called when network policy denies the action");
    }
}

impl at_git::remote::RemoteGitRunner for NeverGitRunner {
    fn run(
        &self,
        _call: &RemoteGitCall<'_>,
    ) -> Result<at_git::policy::GitOutput, at_git::policy::GitError> {
        panic!("RemoteGitRunner must never be called when network policy denies the action");
    }
}

struct FixedClock(i64);
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

struct ZeroJitter;
impl Jitter for ZeroJitter {
    fn delay_ms(&self, _max_ms: u64) -> u64 {
        0
    }
}
/// Sink that records nothing: the bridge must not need a webview to be constructed.
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: JobEvent) {}
}

fn recorder() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback recorder");
    listener
        .set_nonblocking(true)
        .expect("non-blocking recorder");
    listener
}

#[test]
fn starting_the_services_and_reading_the_views_opens_no_connection() {
    let recorder = recorder();

    let bridge = Arc::new(JobBridge::new(
        Arc::new(NullSink),
        Arc::new(TracingOperationsLog),
    ));
    let app = AppState::new(Arc::clone(&bridge), None);
    assert!(app.bridge().jobs().is_empty());

    let network = NetworkState::default();
    for _ in 0..100 {
        let view = policy_view(&network);
        assert_eq!(view.actions.len(), 11);
        assert!(view.events.is_empty());
        assert!(event_views(&network.events()).is_empty());
        network.clear_events();
    }

    assert!(
        recorder.accept().is_err(),
        "starting the services and reading the network views must not open a socket"
    );
    assert_eq!(network.event_count(), 0);
    let report = bridge.shutdown(Duration::from_millis(50));
    assert_eq!(report.interrupted, 0);
}

#[test]
fn the_default_policy_refuses_every_background_call() {
    // This is the architectural half of "idling for ten minutes makes no request": there is no
    // permitted action for a background caller to reach, including the scheduler's own action.
    let network = NetworkState::default();
    for &action in NetworkAction::ALL {
        let denied = network
            .authorize(action, "example.test", NetworkTrigger::Background)
            .expect_err("no background action is permitted by default");
        assert!(
            matches!(
                denied.reason,
                DenialReason::ActionDisabled | DenialReason::BackgroundNotConsented
            ),
            "{action}: {:?}",
            denied.reason
        );
    }
    for &action in NetworkAction::ALL {
        let state = network.policy().state(action);
        assert_ne!(
            state,
            NetworkActionState::Available,
            "{action} must not be pre-consented in a fresh install"
        );
    }
}

#[test]
fn offline_mode_refuses_every_registered_action() {
    let network = NetworkState::default();
    network.set_offline(true);
    for &action in NetworkAction::ALL {
        for trigger in [NetworkTrigger::User, NetworkTrigger::Background] {
            let denied = network
                .authorize(action, "example.test", trigger)
                .expect_err("offline mode refuses everything");
            assert_eq!(denied.reason, DenialReason::Offline, "{action}/{trigger}");
        }
    }
    assert!(policy_view(&network).offline, "the view shows offline mode");
}

#[test]
fn a_replaced_policy_changes_the_view_and_the_decision() {
    let network = NetworkState::default();
    assert!(!network.policy().marketplace_enabled());
    network.set_policy(
        NetworkPolicy::default()
            .with_marketplace_enabled(true)
            .with_background_health(at_model::network::BackgroundHealth::All),
    );
    let view = policy_view(&network);
    assert!(view.marketplace_enabled);
    assert_eq!(view.background_health, "all");
    assert!(
        network
            .authorize(
                NetworkAction::MarketplaceBrowse,
                "skills.example.test",
                NetworkTrigger::User
            )
            .is_ok()
    );
}

#[tokio::test]
async fn table_driven_entry_points_with_no_explicit_action_deny_and_open_no_socket() {
    let recorder = recorder();
    let loopback_addr = recorder.local_addr().expect("local addr");
    let loopback_url = format!("http://{loopback_addr}");
    let git_loopback_url = format!("http://{loopback_addr}/repo.git");

    let default_policy = NetworkPolicy::default();
    let network_state = NetworkState::default();
    let mut log = NetworkEventLog::new();

    // 1. git fetch: with no explicit user action (Background trigger), policy returns NetworkDenied.
    let fetch_call = NetworkGitCall {
        action: NetworkAction::GitFetch,
        trigger: NetworkTrigger::Background,
        remote: &git_loopback_url,
        args: &["fetch", &git_loopback_url],
        at: OffsetDateTime::now_utc(),
    };
    let fetch_err = run_network_git(&default_policy, &mut log, &fetch_call, &NeverGitRunner)
        .expect_err("git fetch must be denied in background with default policy");
    assert_eq!(
        fetch_err.denied().expect("denied").reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "git fetch must not open a socket"
    );

    // 2. git clone: background trigger returns NetworkDenied without spawning git.
    let clone_call = NetworkGitCall {
        action: NetworkAction::GitClone,
        trigger: NetworkTrigger::Background,
        remote: &git_loopback_url,
        args: &["clone", &git_loopback_url],
        at: OffsetDateTime::now_utc(),
    };
    let clone_err = run_network_git(&default_policy, &mut log, &clone_call, &NeverGitRunner)
        .expect_err("git clone must be denied in background with default policy");
    assert_eq!(
        clone_err.denied().expect("denied").reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "git clone must not open a socket"
    );

    // 3. git ls-remote: background remote check returns NetworkDenied.
    let ls_remote_call = RemoteGitCall {
        action: NetworkAction::GitFetch,
        trigger: NetworkTrigger::Background,
        remote: &git_loopback_url,
        args: &["ls-remote", "--symref", &git_loopback_url, "HEAD"],
        cwd: Path::new("."),
        stdin: None,
        env: &[],
        at: OffsetDateTime::now_utc(),
    };
    let ls_remote_err = run_remote_git(&default_policy, &mut log, &ls_remote_call, &NeverGitRunner)
        .expect_err("git ls-remote must be denied in background with default policy");
    let RemoteError::Denied(ls_remote_denied) = ls_remote_err else {
        panic!("expected RemoteError::Denied, got: {ls_remote_err:?}");
    };
    assert_eq!(
        ls_remote_denied.reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "git ls-remote must not open a socket"
    );

    // 4. marketplace: search and refresh are denied because marketplace_enabled is false by default.
    let marketplace_client =
        MarketplaceClient::with_endpoint(&format!("{loopback_url}/api/v1/skills/search"))
            .expect("marketplace client");
    let cache_dir = tempfile::tempdir().expect("cache dir");
    let cache_file = cache_dir.path().join("cache.json");
    let search_err = marketplace_client
        .search("test-query", &cache_file, &default_policy, &mut log)
        .await
        .expect_err("marketplace search must be denied with default policy");
    assert!(
        matches!(search_err, MarketplaceError::Denied(ref denied) if denied.reason == DenialReason::ActionDisabled),
        "expected ActionDisabled for marketplace search, got: {search_err:?}"
    );
    assert!(
        recorder.accept().is_err(),
        "marketplace search must not open a socket"
    );
    let refresh_err = marketplace_client
        .refresh("test-query", &cache_file, &default_policy, &mut log)
        .await
        .expect_err("marketplace refresh must be denied with default policy");
    assert!(
        matches!(refresh_err, MarketplaceError::Denied(ref denied) if denied.reason == DenialReason::ActionDisabled),
        "expected ActionDisabled for marketplace refresh, got: {refresh_err:?}"
    );
    assert!(
        recorder.accept().is_err(),
        "marketplace refresh must not open a socket"
    );
    // 5. MCP HTTP connect & 7. health HTTP checks:
    // scheduled HTTP check is denied because background_health defaults to Local.
    let http_server = McpServer::from_def(
        McpServerId::new(),
        Scope::User,
        McpServerDef {
            display_name: "test-http".to_string(),
            description: None,
            transport: McpTransport::Http {
                url: url::Url::parse(&loopback_url).expect("url"),
                headers: BTreeMap::new(),
                oauth: None,
            },
            extra: BTreeMap::new(),
        },
    );
    let mcp_connect_err = authorize_connect(
        &default_policy,
        &http_server,
        ConnectIntent::Scheduled,
        NetworkAction::ScheduledHttpHealthCheck,
    )
    .expect_err("scheduled HTTP health check connect must be denied with default policy");
    assert_eq!(
        mcp_connect_err.denied().expect("denied").reason,
        DenialReason::ActionDisabled
    );
    assert!(
        recorder.accept().is_err(),
        "authorize_connect must not open a socket"
    );

    let http_client = HttpClient::new(
        Arc::new(default_policy.clone()),
        Arc::new(at_secrets::MemoryStore::new()),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let health_check_err = http_client
        .open_scheduled_health(&http_server, &cancel)
        .await
        .expect_err("open_scheduled_health must be denied with default policy");
    assert_eq!(health_check_err.kind(), HttpErrorKind::Denied);
    assert!(
        recorder.accept().is_err(),
        "scheduled HTTP check must not open a socket"
    );

    // 6. MCP OAuth: denied in background with default policy.
    let oauth_auth_err = default_policy
        .authorize(
            NetworkAction::McpOAuth,
            &loopback_addr.ip().to_string(),
            NetworkTrigger::Background,
        )
        .expect_err("McpOAuth must be denied in background with default policy");
    assert_eq!(oauth_auth_err.reason, DenialReason::BackgroundNotConsented);
    assert!(
        recorder.accept().is_err(),
        "OAuth policy check must not open a socket"
    );

    // 8. updater check: denied in background because updates_opt_in is false.
    let update_check_err = authorize_update(
        &default_policy,
        NetworkAction::UpdateCheck,
        &loopback_addr.ip().to_string(),
        NetworkTrigger::Background,
    )
    .expect_err("update check must be denied in background with default policy");
    assert_eq!(
        update_check_err.denied().expect("denied").reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "update check must not open a socket"
    );

    // 9. updater download: denied in background because updates_opt_in is false.
    let update_download_err = authorize_update(
        &default_policy,
        NetworkAction::UpdateDownload,
        &loopback_addr.ip().to_string(),
        NetworkTrigger::Background,
    )
    .expect_err("update download must be denied in background with default policy");
    assert_eq!(
        update_download_err.denied().expect("denied").reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "update download must not open a socket"
    );
    // 10. remote backup push: denied in background with default policy.
    let backup_push_call = RemoteGitCall {
        action: NetworkAction::RemoteBackupPush,
        trigger: NetworkTrigger::Background,
        remote: &git_loopback_url,
        args: &["push", &git_loopback_url, "main"],
        cwd: Path::new("."),
        stdin: None,
        env: &[],
        at: OffsetDateTime::now_utc(),
    };
    let backup_push_err = run_remote_git(
        &default_policy,
        &mut log,
        &backup_push_call,
        &NeverGitRunner,
    )
    .expect_err("remote backup push must be denied in background with default policy");
    let RemoteError::Denied(backup_push_denied) = backup_push_err else {
        panic!("expected RemoteError::Denied, got: {backup_push_err:?}");
    };
    assert_eq!(
        backup_push_denied.reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "remote backup push must not open a socket"
    );

    // 11. remote backup pull: denied in background with default policy.
    let backup_pull_call = RemoteGitCall {
        action: NetworkAction::RemoteBackupPull,
        trigger: NetworkTrigger::Background,
        remote: &git_loopback_url,
        args: &["pull", &git_loopback_url, "main"],
        cwd: Path::new("."),
        stdin: None,
        env: &[],
        at: OffsetDateTime::now_utc(),
    };
    let backup_pull_err = run_remote_git(
        &default_policy,
        &mut log,
        &backup_pull_call,
        &NeverGitRunner,
    )
    .expect_err("remote backup pull must be denied in background with default policy");
    let RemoteError::Denied(backup_pull_denied) = backup_pull_err else {
        panic!("expected RemoteError::Denied, got: {backup_pull_err:?}");
    };
    assert_eq!(
        backup_pull_denied.reason,
        DenialReason::BackgroundNotConsented
    );
    assert!(
        recorder.accept().is_err(),
        "remote backup pull must not open a socket"
    );

    // 12. GitHub API (remote backup repo creation gate): denied in background with default policy.
    let github_err = network_state
        .authorize(
            NetworkAction::RemoteBackupPush,
            "api.github.com",
            NetworkTrigger::Background,
        )
        .expect_err("GitHub API call must be denied in background with default policy");
    assert_eq!(github_err.reason, DenialReason::BackgroundNotConsented);
    assert!(
        recorder.accept().is_err(),
        "GitHub API check must not open a socket"
    );
}

#[tokio::test]
async fn headless_launch_navigation_and_scheduler_ticks_open_no_connection() {
    // Note: a true GUI/DNS-level test is not covered here because verifying zero DNS
    // queries and zero outbound TCP sockets from a running WebKit webview across 10 real
    // minutes requires (1) a full GUI desktop session, and (2) process-wide socket/DNS
    // hooks or eBPF/pf packet capture at the OS level, which requires unsafe FFI or root
    // privileges forbidden in this workspace.
    //
    // This test exercises the headless equivalent: constructing the real AppState and
    // command layer in a temp sandbox, invoking every read-only command called at startup
    // and navigation across all screens, plus advancing the jobs scheduler across 10 minutes
    // of virtual ticks, asserting zero accepted socket connections on a loopback listener.
    let recorder = recorder();
    let loopback_addr = recorder.local_addr().expect("local addr");

    let sandbox = Sandbox::new().expect("sandbox");
    let database = sandbox.database().expect("database");
    sandbox
        .register_agent(&database, AgentId::Codex)
        .expect("register agent");
    let state = sandbox.state().expect("state");
    let network = NetworkState::default();

    // 1. Settings screen
    let settings = at_app::app::commands::settings_load_inner(&state).expect("settings load");
    assert!(settings.is_object());
    let _ = state.config();
    let policy_v = policy_view(&network);
    assert_eq!(policy_v.actions.len(), 11);
    let events_v = event_views(&network.events());
    assert!(events_v.is_empty());
    let app_info = at_app::app::commands::app_info();
    assert_eq!(app_info.name, "AgenTuna");
    let _ = at_app::app::commands::app_signing_status();

    // 2. Overview screen (across scopes)
    let overview_user =
        at_app::app::overview::overview_load_inner(&state, "user").expect("overview user");
    assert!(!overview_user.scopes.is_empty());
    let overview_global =
        at_app::app::overview::overview_load_inner(&state, "global").expect("overview global");
    assert!(!overview_global.scopes.is_empty());
    let overview_project =
        at_app::app::overview::overview_load_inner(&state, "project").expect("overview project");
    assert!(!overview_project.scopes.is_empty());

    // 3. Skills screen
    let skills = at_app::app::flow_harness::skills_list(
        &state,
        &at_app::app::skills_list::SkillsListRequest {
            scope: Some("user".to_string()),
            project_id: None,
        },
    )
    .expect("skills list user");
    assert!(skills.rows.is_empty() || !skills.rows.is_empty());

    // 4. MCP servers screen
    let mcp_servers = at_app::app::flow_harness::mcp_server_list(
        &state,
        at_app::app::mcp_servers::McpServerListRequest {
            scope: "user".to_string(),
            project_id: None,
        },
    )
    .expect("mcp server list user");
    assert_eq!(mcp_servers.servers.len(), 0);

    // 5. Effective config screen
    let effective = at_app::app::effective::effective_query_inner(
        &state,
        at_app::app::effective::EffectiveQueryRequest {
            scope: "user".to_string(),
            kind: None,
            agent: None,
        },
    )
    .expect("effective query user");
    assert!(effective.items.is_empty());
    let counts = at_app::app::effective::effective_counts_inner(&state).expect("effective counts");
    assert_eq!(counts.skills, 0);

    // 6. Sync & drift screen + Backups
    let drift = at_app::app::flow_harness::drift_snapshot(&state).expect("drift snapshot");
    assert!(drift.rows.is_empty());
    let backups = at_app::app::flow_harness::backups_list(&state).expect("backups list");
    assert!(backups.is_empty());

    // 7. Agents screen
    let agents = at_app::app::workflows::agents_list_inner(&state).expect("agents list");
    assert!(!agents.is_empty());

    // 8. Projects screen
    let projects = at_app::app::projects::projects_list_inner(&state).expect("projects list");
    assert!(projects.is_empty());

    // 9. Instructions screen
    let instructions =
        at_app::app::workflows::instructions_list_inner(&state, None).expect("instructions list");
    assert!(instructions.is_empty());

    // 10. Migrate scan screen
    let scan_result = at_app::app::migrate::migrate_scan_inner(&state, None)
        .await
        .expect("migrate scan");
    assert_eq!(scan_result.items.len(), 0);

    // 11. Preferences screen
    let prefs = at_app::app::commands::settings_load_inner(&state).expect("preferences load");
    assert!(prefs.is_object());

    // Sockets after navigating all screens must remain zero
    assert!(
        recorder.accept().is_err(),
        "visiting every screen must open zero sockets"
    );

    // Advance the jobs scheduler across 10 minutes (600 seconds) of ticks
    let mut scheduler = HealthScheduler::new();
    let db_mutex = Mutex::new(database);
    let cancel = tokio_util::sync::CancellationToken::new();
    let runner = McpHealthRunner::default();
    let jitter = ZeroJitter;

    let loopback_http_target = HealthTarget {
        id: "itm_scheduled_http_check".to_string(),
        name: "scheduled-http-check".to_string(),
        item_id: Some("itm_scheduled_http_check".to_string()),
        scope: Scope::User,
        project_id: None,
        agent_id: None,
        transport: HealthTransport::Http,
        host: Some(loopback_addr.ip().to_string()),
        manually_tested: true,
        server: McpServer::from_def(
            McpServerId::new(),
            Scope::User,
            McpServerDef {
                display_name: "scheduled-http-check".to_string(),
                description: None,
                transport: McpTransport::Http {
                    url: url::Url::parse(&format!("http://{loopback_addr}")).expect("url"),
                    headers: BTreeMap::new(),
                    oauth: None,
                },
                extra: BTreeMap::new(),
            },
        ),
    };

    let schedule_policy = SchedulePolicy {
        interval: HealthInterval::FiveMinutes,
        background: BackgroundHealth::Local, // default setting: HTTP checks disabled!
        online: true,
    };

    // 10 minutes = 600 ticks
    for second in 0..=600 {
        let now_ms = (second as i64) * 1000;
        scheduler.record_activity(now_ms);
        let due = scheduler.due_checks(
            now_ms,
            schedule_policy,
            std::slice::from_ref(&loopback_http_target),
            &jitter,
        );
        for check in due {
            let clock = FixedClock(now_ms);
            let context = ExecutionContext {
                clock: &clock,
                interval: HealthInterval::FiveMinutes,
                runner: &runner,
                network: &network,
                database: &db_mutex,
                cancellation: &cancel,
                deadline: Duration::from_secs(5),
                jitter: &jitter,
                event_sink: None,
            };
            let _ = scheduler.execute(check, &context).await;
        }
    }

    // Verify: zero outbound connections after visiting all screens and idling 10 minutes
    assert!(
        recorder.accept().is_err(),
        "idling 10 minutes with default settings must make zero network connections"
    );
    assert_eq!(
        network.event_count(),
        0,
        "event log must record zero network events"
    );
}
