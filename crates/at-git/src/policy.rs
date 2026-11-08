//! The only place in `at-git` that may reach the network.
//!
//! Git is invoked here and nowhere else: [`run_network_git`] takes the [`NetworkAction`] the call
//! implements, asks [`NetworkPolicy`] first, logs the destination host and only then hands an
//! [`AuthorizedGitCall`] to a [`GitRunner`]. The token has no public constructor, so a runner
//! cannot be called — and a process cannot be spawned — without a permitted action. A refusal
//! returns [`GitError::Denied`] and the runner is never touched: no process, no socket.
//!
//! Two more rules keep the process boundary safe:
//!
//! - The host is extracted from the remote before anything runs. A remote without a host (a local
//!   path, a `file://` URL) is refused: local git work must not go through this gate.
//! - The process runner disables terminal prompts, bounds stdout and stderr and kills the child on
//!   timeout, so a GUI process with no terminal cannot hang on a credential prompt.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use at_model::network::{
    NetworkAction, NetworkDenied, NetworkEvent, NetworkEventLog, NetworkHost, NetworkPolicy,
    NetworkTrigger,
};
use time::OffsetDateTime;

/// How long one network git command may run before it is killed.
pub const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

/// How much stdout or stderr one invocation may keep, in bytes.
pub const MAX_GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// How often the runner checks whether the child exited.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Environment variable that stops git from asking for a password on a terminal it does not have.
const GIT_TERMINAL_PROMPT_ENV: &str = "GIT_TERMINAL_PROMPT";

/// One git command that will use the network.
#[derive(Debug, Clone)]
pub struct NetworkGitCall<'a> {
    /// What the command does on the network.
    pub action: NetworkAction,
    /// Who started it. Every git egress today is user-initiated.
    pub trigger: NetworkTrigger,
    /// Remote URL or scp-like remote the command talks to. Never logged as given.
    pub remote: &'a str,
    /// Arguments passed to git, already split; no shell interpolation happens.
    pub args: &'a [&'a str],
    /// When the call starts; supplied by the caller so tests are deterministic.
    pub at: OffsetDateTime,
}

/// Proof that one git command passed the network policy.
///
/// The fields are private and there is no public constructor, so only [`run_network_git`] can
/// mint one. [`GitRunner`] takes `&AuthorizedGitCall`, which makes "spawn git without an action"
/// impossible to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGitCall {
    action: NetworkAction,
    host: NetworkHost,
    args: Vec<String>,
    _seal: (),
}

impl AuthorizedGitCall {
    /// The permitted action.
    #[must_use]
    pub const fn action(&self) -> NetworkAction {
        self.action
    }

    /// The normalized destination host.
    #[must_use]
    pub const fn host(&self) -> &NetworkHost {
        &self.host
    }

    /// Arguments after the program name.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// A token for the runner's own unit tests; not part of the public contract.
    #[cfg(test)]
    pub(crate) fn for_test(args: Vec<String>) -> Self {
        Self {
            action: NetworkAction::GitFetch,
            host: NetworkHost::parse("example.test").expect("a valid test host"),
            args,
            _seal: (),
        }
    }

    /// A token minted by [`crate::remote::run_remote_git`] after the policy allowed the call;
    /// carries the already-authorized action and host plus the argv.
    pub(crate) fn for_remote(action: NetworkAction, host: NetworkHost, args: &[&str]) -> Self {
        Self {
            action,
            host,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            _seal: (),
        }
    }
}

/// Result of one git invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Whether git exited with status 0.
    pub success: bool,
    /// Exit code when the process exited normally.
    pub code: Option<i32>,
    /// Captured standard output, truncated at the runner's limit.
    pub stdout: Vec<u8>,
    /// Captured standard error, truncated at the runner's limit.
    pub stderr: Vec<u8>,
}

/// A permitted git call: the normalized host plus git's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitReport {
    /// Normalized destination host that the policy allowed.
    pub host: NetworkHost,
    /// What git printed.
    pub output: GitOutput,
}

/// Runs one authorized git invocation. The real implementation spawns git; tests record instead.
pub trait GitRunner: Send + Sync {
    /// Runs the invocation the token describes.
    ///
    /// # Errors
    ///
    /// [`GitError`] when the process cannot start, its output cannot be read or it times out.
    fn run(&self, authorized: &AuthorizedGitCall) -> Result<GitOutput, GitError>;
}

/// Failure of a gated git call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// The network policy refused the call; git was not run.
    #[error(transparent)]
    Denied(NetworkDenied),
    /// The action is not a git action, so this gate refuses to run it.
    #[error("`{0}` is not a git network action")]
    WrongAction(NetworkAction),
    /// The process could not be started.
    #[error("could not start git: {0}")]
    Spawn(#[source] io::Error),
    /// The child could not be waited for.
    #[error("could not wait for git: {0}")]
    Wait(#[source] io::Error),
    /// A pipe could not be read.
    #[error("could not read git output: {0}")]
    Output(#[source] io::Error),
    /// The command outlived its deadline and was killed.
    #[error("git did not finish within {0:?}")]
    TimedOut(Duration),
    /// The command was cancelled by the caller.
    #[error("git command was cancelled")]
    Cancelled,
}

impl GitError {
    /// The policy refusal, when this failure is one.
    #[must_use]
    pub fn denied(&self) -> Option<&NetworkDenied> {
        match self {
            Self::Denied(denied) => Some(denied),
            _ => None,
        }
    }
}

impl From<NetworkDenied> for GitError {
    fn from(denied: NetworkDenied) -> Self {
        Self::Denied(denied)
    }
}

/// Runs `call` only if the network policy permits it, after logging the host.
///
/// The policy is consulted before the runner, so a refusal creates no process and no socket. The
/// permitted call is recorded in `log` with the bytes git printed, which is the only network
/// detail AgenTuna keeps; a call that fails after the policy allowed it is recorded with zero
/// bytes so the user can still see that AgenTuna tried.
///
/// # Errors
///
/// [`GitError::Denied`] when the policy refuses or the remote has no host,
/// [`GitError::WrongAction`] when `call.action` is not a git action, and the runner's own failures.
pub fn run_network_git(
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
    call: &NetworkGitCall<'_>,
    runner: &dyn GitRunner,
) -> Result<GitReport, GitError> {
    if !is_git_action(call.action) {
        return Err(GitError::WrongAction(call.action));
    }
    let Some(remote_host) = NetworkHost::from_url(call.remote) else {
        tracing::warn!(
            network = true,
            action = %call.action.wire_name(),
            reason = "invalid_host",
            "blocked git network call"
        );
        return Err(GitError::Denied(NetworkDenied::invalid_host(call.action)));
    };
    let host = match policy.authorize(call.action, remote_host.as_str(), call.trigger) {
        Ok(host) => host,
        Err(denied) => {
            tracing::warn!(
                network = true,
                action = %call.action.wire_name(),
                host = %remote_host,
                reason = %denied.reason.id(),
                "blocked git network call"
            );
            return Err(GitError::Denied(denied));
        }
    };
    tracing::info!(
        network = true,
        action = %call.action.wire_name(),
        host = %host,
        "git network call"
    );
    let authorized = AuthorizedGitCall {
        action: call.action,
        host: host.clone(),
        args: call.args.iter().map(|arg| (*arg).to_owned()).collect(),
        _seal: (),
    };
    let result = runner.run(&authorized);
    let bytes = match &result {
        Ok(output) => output.stdout.len().saturating_add(output.stderr.len()) as u64,
        Err(_) => 0,
    };
    log.record(NetworkEvent::new(
        call.at,
        call.action,
        Some(host.clone()),
        bytes,
    ));
    Ok(GitReport {
        host,
        output: result?,
    })
}

/// Runs git from `PATH` with prompts disabled, bounded output and a deadline.
///
/// `program` and the timeout are injectable so the timeout and output-cap behaviour can be
/// exercised without a network. The runner still needs an [`AuthorizedGitCall`], which only the
/// policy can mint.
#[derive(Debug, Clone)]
pub struct ProcessGitRunner {
    program: PathBuf,
    timeout: Duration,
    output_limit: usize,
}

impl Default for ProcessGitRunner {
    fn default() -> Self {
        Self::git()
    }
}

impl ProcessGitRunner {
    /// The system git with the default deadline and output limit.
    #[must_use]
    pub fn git() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: GIT_NETWORK_TIMEOUT,
            output_limit: MAX_GIT_OUTPUT_BYTES,
        }
    }

    /// Runs `program` instead of git, with an explicit deadline.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
            output_limit: MAX_GIT_OUTPUT_BYTES,
        }
    }

    /// Caps how many bytes of stdout and stderr are kept.
    #[must_use]
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// The program this runner executes.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

impl GitRunner for ProcessGitRunner {
    fn run(&self, authorized: &AuthorizedGitCall) -> Result<GitOutput, GitError> {
        let mut command = Command::new(&self.program);
        command
            .args(authorized.args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(GIT_TERMINAL_PROMPT_ENV, "0");
        let mut child = at_mcp::spawn::spawn_retrying(&mut command).map_err(GitError::Spawn)?;

        let stdout = child.stdout.take().map(|pipe| {
            let limit = self.output_limit;
            thread::spawn(move || read_bounded(pipe, limit))
        });
        let stderr = child.stderr.take().map(|pipe| {
            let limit = self.output_limit;
            thread::spawn(move || read_bounded(pipe, limit))
        });

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = join_reader(stdout);
                        let _ = join_reader(stderr);
                        return Err(GitError::TimedOut(self.timeout));
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = join_reader(stdout);
                    let _ = join_reader(stderr);
                    return Err(GitError::Wait(error));
                }
            }
        };

        Ok(GitOutput {
            success: status.success(),
            code: status.code(),
            stdout: join_reader(stdout)?,
            stderr: join_reader(stderr)?,
        })
    }
}

/// Drains a pipe completely, keeping at most `limit` bytes so a chatty child cannot fill memory.
pub(crate) fn read_bounded(mut pipe: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(kept);
        }
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
}

pub(crate) fn join_reader(
    handle: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, GitError> {
    let Some(handle) = handle else {
        return Ok(Vec::new());
    };
    match handle.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(GitError::Output(error)),
        Err(_) => Err(GitError::Output(io::Error::other(
            "git output reader panicked",
        ))),
    }
}

fn is_git_action(action: NetworkAction) -> bool {
    matches!(
        action,
        NetworkAction::GitFetch
            | NetworkAction::GitClone
            | NetworkAction::RemoteBackupPush
            | NetworkAction::RemoteBackupPull
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    #[derive(Default)]
    struct RecordingRunner {
        calls: Mutex<Vec<Vec<String>>>,
        output: Option<GitOutput>,
    }

    impl RecordingRunner {
        fn returning(stdout: &[u8], stderr: &[u8]) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                output: Some(GitOutput {
                    success: true,
                    code: Some(0),
                    stdout: stdout.to_vec(),
                    stderr: stderr.to_vec(),
                }),
            }
        }

        fn calls(&self) -> usize {
            self.calls
                .lock()
                .map(|calls| calls.len())
                .unwrap_or_default()
        }

        fn first_call(&self) -> Option<Vec<String>> {
            self.calls
                .lock()
                .ok()
                .and_then(|calls| calls.first().cloned())
        }
    }

    impl GitRunner for RecordingRunner {
        fn run(&self, authorized: &AuthorizedGitCall) -> Result<GitOutput, GitError> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(authorized.args().to_vec());
            }
            self.output.clone().map_or_else(
                || {
                    Err(GitError::Output(io::Error::other(
                        "recording runner has no output",
                    )))
                },
                Ok,
            )
        }
    }

    fn fetch_call<'a>(args: &'a [&'a str]) -> NetworkGitCall<'a> {
        NetworkGitCall {
            action: NetworkAction::GitFetch,
            trigger: NetworkTrigger::User,
            remote: "https://user:token@github.com/acme/repo.git",
            args,
            at: NOW,
        }
    }

    #[test]
    fn a_denied_call_never_reaches_git_and_records_nothing() {
        let policy = NetworkPolicy::default().with_offline(true);
        let mut log = NetworkEventLog::new();
        let runner = RecordingRunner::default();
        let error = run_network_git(&policy, &mut log, &fetch_call(&["fetch"]), &runner)
            .expect_err("offline policy must refuse");
        let denied = error.denied().expect("a policy refusal");
        assert_eq!(denied.reason, at_model::network::DenialReason::Offline);
        assert_eq!(runner.calls(), 0, "git must not run");
        assert!(log.is_empty(), "a refused call is not a network event");
    }

    #[test]
    fn a_remote_without_a_host_is_refused_before_git_runs() {
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::new();
        let runner = RecordingRunner::default();
        for remote in ["/tmp/repo", "file:///tmp/repo", ""] {
            let call = NetworkGitCall {
                remote,
                ..fetch_call(&["fetch"])
            };
            let error = run_network_git(&policy, &mut log, &call, &runner).expect_err("no host");
            assert_eq!(
                error.denied().expect("a policy refusal").reason,
                at_model::network::DenialReason::InvalidHost,
                "{remote:?}"
            );
        }
        assert_eq!(runner.calls(), 0);
        assert!(log.is_empty());
    }

    #[test]
    fn a_permitted_call_runs_once_and_records_host_and_bytes() {
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::new();
        let runner = RecordingRunner::returning(b"abcd", b"ef");
        let args = ["fetch", "--prune", "origin"];
        let report =
            run_network_git(&policy, &mut log, &fetch_call(&args), &runner).expect("permitted");

        assert_eq!(report.host.as_str(), "github.com");
        assert_eq!(report.host.to_string(), "github.com");
        assert_eq!(runner.calls(), 1);
        assert_eq!(
            runner.first_call().expect("one call"),
            args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()
        );

        let events = log.to_vec();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, NetworkAction::GitFetch);
        assert_eq!(
            events[0].host.as_ref().map(NetworkHost::as_str),
            Some("github.com"),
            "user info from the remote never reaches the event"
        );
        assert_eq!(events[0].bytes, 6);
        assert_eq!(events[0].at, NOW);
    }

    #[test]
    fn a_failed_run_is_still_recorded_without_bytes() {
        struct FailingRunner;
        impl GitRunner for FailingRunner {
            fn run(&self, _authorized: &AuthorizedGitCall) -> Result<GitOutput, GitError> {
                Err(GitError::TimedOut(Duration::from_secs(1)))
            }
        }
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::new();
        let error = run_network_git(&policy, &mut log, &fetch_call(&["fetch"]), &FailingRunner)
            .expect_err("runner failed");
        assert!(matches!(error, GitError::TimedOut(_)));
        let events = log.to_vec();
        assert_eq!(events.len(), 1, "the attempt is visible in the ring buffer");
        assert_eq!(events[0].bytes, 0);
    }

    #[test]
    fn background_git_is_refused_by_default() {
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::new();
        let runner = RecordingRunner::default();
        let call = NetworkGitCall {
            trigger: NetworkTrigger::Background,
            ..fetch_call(&["fetch"])
        };
        let error = run_network_git(&policy, &mut log, &call, &runner).expect_err("background");
        assert_eq!(
            error.denied().expect("a policy refusal").reason,
            at_model::network::DenialReason::BackgroundNotConsented
        );
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn an_action_that_is_not_git_is_refused() {
        let policy = NetworkPolicy::default().with_marketplace_enabled(true);
        let mut log = NetworkEventLog::new();
        let runner = RecordingRunner::default();
        let call = NetworkGitCall {
            action: NetworkAction::MarketplaceBrowse,
            ..fetch_call(&["fetch"])
        };
        let error = run_network_git(&policy, &mut log, &call, &runner).expect_err("wrong action");
        assert!(matches!(
            error,
            GitError::WrongAction(NetworkAction::MarketplaceBrowse)
        ));
        assert_eq!(runner.calls(), 0);
    }

    #[test]
    fn the_token_exposes_only_the_permitted_call() {
        let authorized = AuthorizedGitCall::for_test(vec!["fetch".to_owned()]);
        assert_eq!(authorized.action(), NetworkAction::GitFetch);
        assert_eq!(authorized.host().as_str(), "example.test");
        assert_eq!(authorized.args(), ["fetch"]);
    }

    #[test]
    fn the_process_runner_captures_and_caps_output() {
        let runner =
            ProcessGitRunner::new("/bin/sh", Duration::from_secs(10)).with_output_limit(1024);
        let authorized =
            AuthorizedGitCall::for_test(vec!["-c".to_owned(), "head -c 4096 /dev/zero".to_owned()]);
        let output = runner.run(&authorized).expect("sh runs");
        assert!(output.success);
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout.len(), 1024, "output is capped");
    }

    #[test]
    fn the_process_runner_kills_a_command_that_overruns_its_deadline() {
        let runner = ProcessGitRunner::new("/bin/sleep", Duration::from_millis(150));
        let authorized = AuthorizedGitCall::for_test(vec!["10".to_owned()]);
        let started = Instant::now();
        let error = runner.run(&authorized).expect_err("sleep 10 must time out");
        assert!(matches!(error, GitError::TimedOut(_)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the child is killed, not waited for"
        );
    }

    #[test]
    fn the_git_runner_uses_the_system_git_and_a_non_prompting_environment() {
        let runner = ProcessGitRunner::git();
        assert_eq!(runner.program(), Path::new("git"));
        assert_eq!(runner.timeout, GIT_NETWORK_TIMEOUT);
        assert_eq!(runner.output_limit, MAX_GIT_OUTPUT_BYTES);
    }
}
