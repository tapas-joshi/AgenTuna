//! TM-015 acceptance tests: spawn, handshake, tool listing, stderr capture and the
//! shutdown escalation ladder.
//!
//! Every test runs the `at-mcp-fake-server` fixture in a temporary directory against an
//! in-memory credential store. Nothing reads or writes the developer's HOME, Keychain or
//! agent folders. Run with `cargo test -p at-mcp --features testkit`.

#![cfg(unix)]
#![cfg(feature = "testkit")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use at_mcp::path::{ManagerRoots, PathResolver};
use at_mcp::session::{
    DEFAULT_STARTUP, LaunchConsent, PACKAGE_RUNNER_STARTUP, SessionErrorKind, ShutdownStep,
    StdioClient, StdioLaunch, StdioSession, TimeoutPolicy,
};
use at_mcp::{HttpClient, McpErrorKind, test_connection};
use at_model::{EnvValue, McpServer, McpServerId, McpTransport, NetworkPolicy, Scope, SecretRef};
use at_secrets::{KeychainId, MemoryStore, SecretStore, SecretString};
use rustix::process::{Pid, test_kill_process, test_kill_process_group};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const SECRET: &str = "sk-live-tm015-sentinel-0123456789";
const ACCOUNT: &str = "tm015-account";

fn fake_server() -> PathBuf {
    option_env!("CARGO_BIN_EXE_at-mcp-fake-server")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("run with --features testkit so Cargo builds at-mcp-fake-server"))
}

fn fast_policy() -> TimeoutPolicy {
    TimeoutPolicy::default()
        .with_budgets(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .with_graces(Duration::from_millis(400), Duration::from_millis(400))
}

fn resolver() -> Arc<PathResolver> {
    Arc::new(
        PathResolver::builder()
            .shell(PathBuf::from("/bin/sh"))
            .default_dirs(Vec::new())
            .manager_roots(ManagerRoots::default())
            .build(),
    )
}

fn client_with(store: Arc<MemoryStore>, policy: Option<TimeoutPolicy>) -> StdioClient {
    let client = StdioClient::new(resolver(), store);
    match policy {
        Some(policy) => client.with_timeouts(policy),
        None => client,
    }
}

fn store_with_secret() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::new());
    let id = KeychainId::new(ACCOUNT).expect("keychain id");
    store
        .create(&id, SecretString::new(SECRET))
        .expect("store secret");
    store
}

fn secret_env() -> BTreeMap<String, EnvValue> {
    let mut env = BTreeMap::new();
    env.insert(
        "GITHUB_TOKEN".to_owned(),
        EnvValue::Secret(SecretRef {
            env_name: "GITHUB_TOKEN".to_owned(),
            keychain_account: ACCOUNT.to_owned(),
        }),
    );
    env
}

fn launch(args: &[&str]) -> StdioLaunch {
    StdioLaunch::new(fake_server().display().to_string()).with_args(args.iter().copied())
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

fn read_report(path: &Path) -> BTreeMap<String, String> {
    let text = fs::read_to_string(path).expect("fake server wrote its report");
    text.lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn process_exists(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| test_kill_process(pid).is_ok())
}

fn group_exists(pgid: u32) -> bool {
    i32::try_from(pgid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pgid| test_kill_process_group(pgid).is_ok())
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write fixture");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

async fn open(
    launch: &StdioLaunch,
    store: Arc<MemoryStore>,
    policy: TimeoutPolicy,
) -> Result<StdioSession, at_mcp::session::SessionError> {
    client_with(store, Some(policy))
        .open(
            launch,
            &LaunchConsent::user_approved(launch),
            &CancellationToken::new(),
        )
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_completes_and_tools_paginate() {
    let launch = launch(&["--tools", "5", "--page-size", "2"]);
    let mut session = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect("handshake");

    let identity = session.server_identity().expect("server identity");
    assert_eq!(identity.name.as_deref(), Some("at-mcp-fake-server"));
    assert_eq!(identity.version.as_deref(), Some("0.1.0"));
    assert!(!identity.protocol_version.is_empty());

    let cancel = CancellationToken::new();
    let listing = session.list_tools(&cancel).await.expect("tools");
    let names: Vec<&str> = listing
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["tool-000", "tool-001", "tool-002", "tool-003", "tool-004"]
    );
    assert!(!listing.truncated);

    let report = session.shutdown(&CancellationToken::new()).await;
    assert_eq!(report.steps.first(), Some(&ShutdownStep::CloseTransport));
    assert!(session.list_tools(&cancel).await.is_err(), "closed session");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_megabyte_of_stderr_does_not_block_the_handshake() {
    let launch = launch(&["--stderr-bytes", "1048576", "--tools", "2"]);
    let session = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect("a chatty server still initializes");

    let snapshot = session.stderr_snapshot();
    assert!(snapshot.total_bytes >= 1024 * 1024, "{snapshot:?}");
    assert_eq!(snapshot.kept_bytes, at_mcp::session::STDERR_RING_BYTES);
    assert!(
        snapshot.text.ends_with("stderr-flood-complete\n"),
        "{snapshot:?}"
    );

    let listing = session
        .list_tools(&CancellationToken::new())
        .await
        .expect("tools");
    assert_eq!(listing.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_server_fails_with_startup_timeout_and_is_reaped() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let launch = launch(&[
        "--init-delay-ms",
        "30000",
        "--report",
        &path_arg(&report_path),
    ]);
    let policy = TimeoutPolicy::default()
        .with_budgets(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .with_graces(Duration::from_millis(300), Duration::from_millis(300));

    let started = Instant::now();
    let error = open(&launch, Arc::new(MemoryStore::new()), policy)
        .await
        .expect_err("the server never answers initialize");
    assert_eq!(error.kind(), SessionErrorKind::StartupTimeout);
    assert!(
        error.ui_message().contains("did not finish starting"),
        "{}",
        error.ui_message()
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    let report = read_report(&report_path);
    let pid: u32 = report["pid"].parse().expect("pid");
    assert!(
        !process_exists(pid),
        "the timed-out child was killed and reaped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ignores_sigint_and_stops_on_sigterm() {
    let dir = TempDir::new().expect("tempdir");
    let signal_log = dir.path().join("signals.log");
    let launch = launch(&[
        "--signals",
        "ignore-int",
        "--ignore-eof",
        "--signal-log",
        &path_arg(&signal_log),
    ]);
    let mut session = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect("handshake");

    let report = session.shutdown(&CancellationToken::new()).await;

    assert!(report.ran(ShutdownStep::Interrupt));
    assert!(report.ran(ShutdownStep::Terminate));
    assert!(
        !report.ran(ShutdownStep::Kill),
        "SIGTERM was enough: {report:?}"
    );
    assert_eq!(read_lines(&signal_log), ["SIGINT", "SIGTERM"]);
    assert_eq!(report.exit.and_then(|exit| exit.code), Some(0));
    assert!(report.group_cleared);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deaf_server_is_killed_and_its_process_group_is_cleared() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let signal_log = dir.path().join("signals.log");
    let launch = launch(&[
        "--signals",
        "ignore-all",
        "--ignore-eof",
        "--grandchild",
        "--report",
        &path_arg(&report_path),
        "--signal-log",
        &path_arg(&signal_log),
    ]);
    let mut session = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect("handshake");

    let pid = session.process_id();
    let report = read_report(&report_path);
    let pgid: u32 = report["pgid"].parse().expect("pgid");
    let grandchild: u32 = report["grandchild"].parse().expect("grandchild pid");
    assert_eq!(pgid, pid, "the child leads its own process group");
    assert!(process_exists(grandchild), "the grandchild starts alive");

    let shutdown = session.shutdown(&CancellationToken::new()).await;

    assert!(shutdown.ran(ShutdownStep::Interrupt));
    assert!(shutdown.ran(ShutdownStep::Terminate));
    assert!(shutdown.ran(ShutdownStep::Kill), "{shutdown:?}");
    assert!(shutdown.group_cleared, "no surviving member: {shutdown:?}");
    assert!(!group_exists(pgid));
    assert!(
        !process_exists(grandchild),
        "the grandchild died with the group"
    );
    assert_eq!(read_lines(&signal_log), ["SIGINT", "SIGTERM"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_child_gets_its_own_group_cwd_and_resolved_environment() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let work = dir.path().join("work");
    fs::create_dir(&work).expect("work dir");

    let mut env = secret_env();
    env.insert(
        "AT_MCP_MARKER".to_owned(),
        EnvValue::Literal("present".to_owned()),
    );
    let launch = launch(&["--report", &path_arg(&report_path)])
        .with_cwd(&work)
        .with_env(env);

    let session = open(&launch, store_with_secret(), fast_policy())
        .await
        .expect("handshake");
    let report = read_report(&report_path);

    assert_eq!(
        Path::new(&report["cwd"])
            .canonicalize()
            .expect("reported cwd"),
        work.canonicalize().expect("fixture cwd")
    );
    assert_eq!(report["env.AT_MCP_MARKER"], "present");
    assert_eq!(report["env.GITHUB_TOKEN"], SECRET);
    assert!(
        report["env.PATH"].contains('/'),
        "the parent environment is inherited"
    );
    assert_eq!(
        report["pgid"].parse::<u32>().expect("pgid"),
        session.process_id()
    );
    assert_eq!(
        report["pid"].parse::<u32>().expect("pid"),
        session.process_id()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resolved_secret_in_argv_blocks_the_launch() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let launch =
        launch(&["--report", &path_arg(&report_path), "--token", SECRET]).with_env(secret_env());

    let error = open(&launch, store_with_secret(), fast_policy())
        .await
        .expect_err("secret in argv");
    assert_eq!(error.kind(), SessionErrorKind::SecretInArguments);
    assert!(!error.ui_message().contains(SECRET));
    assert!(!report_path.exists(), "the server never started");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_keychain_entry_blocks_the_launch() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let launch = launch(&["--report", &path_arg(&report_path)]).with_env(secret_env());

    let error = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect_err("missing secret");
    assert_eq!(error.kind(), SessionErrorKind::MissingSecret);
    assert!(error.ui_message().contains("GITHUB_TOKEN"));
    assert!(!report_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn consent_for_another_launch_is_refused() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let approved = launch(&["--tools", "1"]);
    let consent = LaunchConsent::user_approved(&approved);
    let requested = launch(&["--report", &path_arg(&report_path)]);

    let error = client_with(Arc::new(MemoryStore::new()), Some(fast_policy()))
        .open(&requested, &consent, &CancellationToken::new())
        .await
        .expect_err("consent does not cover this launch");
    assert_eq!(error.kind(), SessionErrorKind::ConsentMismatch);
    assert!(!report_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_exits_before_initialize_reports_exit_with_its_stderr() {
    let launch = launch(&["--exit-before-init", "7", "--stderr-bytes", "64"]);
    let error = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect_err("the server exits");
    assert_eq!(error.kind(), SessionErrorKind::Exited);
    assert!(error.to_string().contains("exit code 7"), "{error}");
    let stderr = error.stderr_excerpt().expect("stderr excerpt");
    assert!(stderr.contains("stderr-flood-complete"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unresolved_command_names_the_missing_binary() {
    let dir = TempDir::new().expect("tempdir");
    let empty_bin = dir.path().join("bin");
    fs::create_dir(&empty_bin).expect("bin");
    let shell = dir.path().join("shell");
    write_executable(
        &shell,
        &format!("#!/bin/sh\nprintf '%s' '{}'\n", empty_bin.display()),
    );
    let paths = Arc::new(
        PathResolver::builder()
            .shell(&shell)
            .default_dirs(Vec::new())
            .manager_roots(ManagerRoots::default())
            .build(),
    );
    let launch = StdioLaunch::new("definitely-not-a-real-binary");
    let client = StdioClient::new(paths, Arc::new(MemoryStore::new()));

    let error = client
        .open(
            &launch,
            &LaunchConsent::user_approved(&launch),
            &CancellationToken::new(),
        )
        .await
        .expect_err("no such command");
    assert_eq!(error.kind(), SessionErrorKind::UnresolvedCommand);
    assert!(error.ui_message().contains("definitely-not-a-real-binary"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_executable_file_is_refused_before_spawning() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("not-executable");
    fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write");
    let launch = StdioLaunch::new(path.display().to_string());
    let error = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect_err("not executable");
    assert_eq!(error.kind(), SessionErrorKind::NotExecutable);
}

#[test]
fn the_shipped_timeout_table_uses_basenames() {
    assert_eq!(
        TimeoutPolicy::for_command("npx").startup,
        PACKAGE_RUNNER_STARTUP
    );
    assert_eq!(
        TimeoutPolicy::for_command("/opt/homebrew/bin/bunx").startup,
        PACKAGE_RUNNER_STARTUP
    );
    assert_eq!(
        TimeoutPolicy::for_command("uvx").startup,
        PACKAGE_RUNNER_STARTUP
    );
    assert_eq!(
        TimeoutPolicy::for_command("pipx").startup,
        PACKAGE_RUNNER_STARTUP
    );
    assert_eq!(
        TimeoutPolicy::for_command("pnpm").startup,
        PACKAGE_RUNNER_STARTUP
    );
    assert_eq!(TimeoutPolicy::for_command("node").startup, DEFAULT_STARTUP);
    assert_eq!(
        TimeoutPolicy::for_command(&fake_server().display().to_string()).startup,
        DEFAULT_STARTUP
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_npx_named_server_is_started_with_the_package_runner_budget() {
    let dir = TempDir::new().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).expect("bin");
    let npx = bin.join("npx");
    std::os::unix::fs::symlink(fake_server(), &npx).expect("symlink npx");
    let shell = dir.path().join("shell");
    write_executable(
        &shell,
        &format!("#!/bin/sh\nprintf '%s' '{}'\n", bin.display()),
    );
    let paths = Arc::new(
        PathResolver::builder()
            .shell(&shell)
            .default_dirs(Vec::new())
            .manager_roots(ManagerRoots::default())
            .build(),
    );
    let launch = StdioLaunch::new("npx").with_args(["--tools", "1"]);
    assert_eq!(
        launch.timeout_policy().startup,
        PACKAGE_RUNNER_STARTUP,
        "the launcher itself selects the 180 s budget"
    );

    // No override: the client uses the per-command table.
    let client = StdioClient::new(paths, Arc::new(MemoryStore::new()));
    let session = client
        .open(
            &launch,
            &LaunchConsent::user_approved(&launch),
            &CancellationToken::new(),
        )
        .await
        .expect("open through the resolved PATH");
    assert_eq!(
        session
            .list_tools(&CancellationToken::new())
            .await
            .expect("tools")
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_token_stops_before_the_spawn() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let launch = launch(&["--report", &path_arg(&report_path)]);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = client_with(Arc::new(MemoryStore::new()), Some(fast_policy()))
        .open(&launch, &LaunchConsent::user_approved(&launch), &cancel)
        .await
        .expect_err("cancelled");
    assert_eq!(error.kind(), SessionErrorKind::Cancelled);
    assert!(!report_path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_during_the_handshake_kills_and_reaps_the_child() {
    let dir = TempDir::new().expect("tempdir");
    let report_path = dir.path().join("report.txt");
    let launch = launch(&[
        "--init-delay-ms",
        "30000",
        "--report",
        &path_arg(&report_path),
    ]);
    let consent = LaunchConsent::user_approved(&launch);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let opening = tokio::spawn(async move {
        client_with(Arc::new(MemoryStore::new()), Some(fast_policy()))
            .open(&launch, &consent, &token)
            .await
    });

    // Deterministic rendezvous: the fixture writes its pid before reading a request.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !report_path.exists() {
        assert!(Instant::now() < deadline, "the fake server never reported");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();

    let error = opening.await.expect("join").expect_err("cancelled");
    assert_eq!(error.kind(), SessionErrorKind::Cancelled);
    let report = read_report(&report_path);
    let pid: u32 = report["pid"].parse().expect("pid");
    assert!(
        !process_exists(pid),
        "the cancelled child was killed and reaped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_shutdown_skips_the_grace_periods() {
    let launch = launch(&["--signals", "ignore-all", "--ignore-eof"]);
    let policy = TimeoutPolicy::default()
        .with_budgets(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .with_graces(Duration::from_secs(30), Duration::from_secs(30));
    let mut session = open(&launch, Arc::new(MemoryStore::new()), policy)
        .await
        .expect("handshake");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let started = Instant::now();
    let report = session.shutdown(&cancel).await;
    let elapsed = started.elapsed();

    assert!(report.ran(ShutdownStep::Kill), "{report:?}");
    assert!(report.group_cleared);
    assert!(
        elapsed < Duration::from_secs(10),
        "shutdown ignored the 30 s graces, took {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_are_capped_at_five_hundred() {
    let dir = TempDir::new().expect("tempdir");
    let request_log = dir.path().join("requests.log");
    let launch = launch(&[
        "--tools",
        "600",
        "--page-size",
        "100",
        "--request-log",
        &path_arg(&request_log),
    ]);
    let session = open(&launch, Arc::new(MemoryStore::new()), fast_policy())
        .await
        .expect("handshake");

    let listing = session
        .list_tools(&CancellationToken::new())
        .await
        .expect("tools");
    assert_eq!(listing.len(), 500);
    assert!(listing.truncated);
    assert_eq!(
        listing.tools.first().map(|tool| tool.name.as_str()),
        Some("tool-000")
    );
    assert_eq!(
        listing.tools.last().map(|tool| tool.name.as_str()),
        Some("tool-499")
    );

    let pages = read_lines(&request_log)
        .into_iter()
        .filter(|method| method == "tools/list")
        .count();
    assert_eq!(pages, 5, "the harness never asks for a sixth page");
}

/// The literal acceptance clock: a non-package-runner command gets 20 s and no more.
/// Ignored by default because it would add 20 s to every test run; the fast-policy test
/// above exercises the same code path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "waits the shipped 20 s startup budget; run with --ignored"]
async fn the_shipped_budget_fails_a_slow_server_at_twenty_seconds() {
    let launch = launch(&["--init-delay-ms", "30000"]);
    let client = StdioClient::new(resolver(), Arc::new(MemoryStore::new()));
    let started = Instant::now();
    let error = client
        .open(
            &launch,
            &LaunchConsent::user_approved(&launch),
            &CancellationToken::new(),
        )
        .await
        .expect_err("timeout");
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), SessionErrorKind::StartupTimeout);
    assert!(elapsed >= Duration::from_secs(19), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

/// TM-017: the public test-connection API lists and sorts the fixture's tools.
#[tokio::test]
async fn test_connection_api_requires_consent_and_lists_all_tools() {
    use std::hash::{Hash, Hasher};

    let sandbox = TempDir::new().expect("sandbox");
    let agent_config = sandbox.path().join("agent-config.json");
    fs::write(&agent_config, br#"{"servers":{"keep":true}}"#).expect("fixture config");
    let config_hash = |path: &Path| {
        let bytes = fs::read(path).expect("read config fixture");
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    };
    let hash_before = config_hash(&agent_config);
    let server = McpServer {
        id: McpServerId::new(),
        display_name: "fixture".to_owned(),
        description: None,
        transport: McpTransport::Stdio {
            command: fake_server().display().to_string(),
            args: vec![
                "--tools".to_owned(),
                "26".to_owned(),
                "--page-size".to_owned(),
                "5".to_owned(),
            ],
            cwd: Some(sandbox.path().to_path_buf()),
            env: BTreeMap::new(),
        },
        scope: Scope::User,
        extra: BTreeMap::new(),
    };
    let stdio = client_with(Arc::new(MemoryStore::new()), Some(fast_policy()));
    let http = HttpClient::new(
        Arc::new(NetworkPolicy::default()),
        Arc::new(MemoryStore::new()),
    );
    let cancel = CancellationToken::new();

    let without_consent = test_connection(&server, &stdio, &http, None, &cancel).await;
    assert!(!without_consent.ok);
    assert_eq!(
        without_consent.error.as_ref().map(|error| error.kind()),
        Some(&McpErrorKind::ConsentRequired)
    );

    let launch = StdioLaunch::from_server(&server).expect("stdio launch");
    let result = test_connection(
        &server,
        &stdio,
        &http,
        Some(&LaunchConsent::user_approved(&launch)),
        &cancel,
    )
    .await;
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.server_name, "at-mcp-fake-server");
    assert_eq!(result.server_version.as_deref(), Some("0.1.0"));
    assert_eq!(result.tools_count, 26);
    assert!(result.latency_ms > 0);
    assert_eq!(
        result.tools.first().map(|tool| tool.name.as_str()),
        Some("tool-000")
    );
    assert_eq!(
        result.tools.last().map(|tool| tool.name.as_str()),
        Some("tool-025")
    );
    assert!(result.error.is_none());
    assert_eq!(config_hash(&agent_config), hash_before);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancelled_at = Instant::now();
    let cancelled_result = test_connection(
        &server,
        &stdio,
        &http,
        Some(&LaunchConsent::user_approved(&launch)),
        &cancelled,
    )
    .await;
    assert_eq!(
        cancelled_result.error.as_ref().map(|error| error.kind()),
        Some(&McpErrorKind::Cancelled)
    );
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert_eq!(config_hash(&agent_config), hash_before);
}

/// Cancelling the public test API during initialize reaps its child before returning.
#[tokio::test]
async fn test_connection_api_cancellation_reaps_in_flight_child() {
    let temp = TempDir::new().expect("temp dir");
    let report_path = temp.path().join("server-report.txt");
    let server = McpServer {
        id: McpServerId::new(),
        display_name: "slow fixture".to_owned(),
        description: None,
        transport: McpTransport::Stdio {
            command: fake_server().display().to_string(),
            args: vec![
                "--report".to_owned(),
                path_arg(&report_path),
                "--init-delay-ms".to_owned(),
                "5000".to_owned(),
            ],
            cwd: None,
            env: BTreeMap::new(),
        },
        scope: Scope::User,
        extra: BTreeMap::new(),
    };
    let stdio = client_with(Arc::new(MemoryStore::new()), Some(fast_policy()));
    let http = HttpClient::new(
        Arc::new(NetworkPolicy::default()),
        Arc::new(MemoryStore::new()),
    );
    let launch = StdioLaunch::from_server(&server).expect("stdio launch");
    let consent = LaunchConsent::user_approved(&launch);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        test_connection(&server, &stdio, &http, Some(&consent), &task_cancel).await
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !report_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture started");
    let report = read_report(&report_path);
    let pid = report["pid"].parse::<u32>().expect("fixture PID");
    let cancelled_at = Instant::now();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("test_connection returned within one second")
        .expect("task joined");

    assert_eq!(
        result.error.as_ref().map(|error| error.kind()),
        Some(&McpErrorKind::Cancelled)
    );
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert!(!process_exists(pid), "cancelled test left a child process");
}
