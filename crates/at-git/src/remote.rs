//! Remote-backup git: clone, commit, push and fetch against a user-configured remote.
//!
//! This module is the only part of `at-git` that runs git against a remote for the backup
//! feature, and it reuses [`crate::policy::run_network_git`] so every network call passes the
//! same [`at_model::network::NetworkPolicy`] gate as every other egress site. Nothing here can
//! run without an [`AuthorizedGitCall`], which only the policy mints.
//!
//! ## Credential handling
//!
//! The GitHub token never appears in argv, in a URL, in `.git/config`, in a log line or in an
//! error. [`GitAuth::token`] builds a temporary askpass script that echoes the token from an
//! environment variable; git is invoked with `GIT_ASKPASS` pointing at that script,
//! `GIT_TERMINAL_PROMPT=0` so a missing credential fails instead of prompting, and an empty
//! `credential.helper` so no stored helper can supply or leak a different credential. The
//! script lives in a private temporary directory that is removed when the [`GitAuth`] drops.
//!
//! The remote URL stored in git config is the token-free `https://github.com/<owner>/<repo>`
//! form; git asks the askpass for the password at push/fetch time. For a `file://` or local-path
//! remote used in tests, no credential is needed and [`GitAuth::none`] is used.

use at_model::network::{NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

use crate::policy::{
    AuthorizedGitCall, GitError, GitOutput, MAX_GIT_OUTPUT_BYTES, POLL_INTERVAL, read_bounded,
};

/// How long one remote git command may run before it is killed.
pub const REMOTE_GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// How many times one clone is attempted when git reports a dropped connection.
const CLONE_ATTEMPTS: u32 = 2;

/// Backoff before retrying a clone after a dropped connection.
const CLONE_RETRY_BACKOFF: Duration = Duration::from_millis(250);

/// Longest diagnostic kept from git's stderr, in characters.
pub const MAX_REMOTE_DIAGNOSTIC_CHARS: usize = 2048;

/// Environment variable the askpass script reads the token from.
const TOKEN_ENV: &str = "AGENTUNA_REMOTE_BACKUP_TOKEN";

/// Askpass script body. It answers git's two prompts: username, then password (the token).
/// The token is read from the environment at exec time, never embedded in the script.
const ASKPASS_SCRIPT: &str = "#!/bin/sh\ncase \"$1\" in\n  *sername*|*Username*) printf '%s' \"$AGENTUNA_REMOTE_BACKUP_USERNAME\" ;;\n  *) printf '%s' \"$AGENTUNA_REMOTE_BACKUP_TOKEN\" ;;\nesac\n";

/// Username git sends for an HTTPS token push; GitHub accepts any non-empty value.
const TOKEN_USERNAME: &str = "x-access-token";

/// One remote git invocation as the backup code builds it.
#[derive(Debug, Clone)]
pub struct RemoteGitCall<'a> {
    /// What the command does on the network. Every variant maps to a [`NetworkAction`].
    pub action: NetworkAction,
    /// Who started it; remote backup is always user-initiated.
    pub trigger: NetworkTrigger,
    /// Remote URL the command talks to. Never logged as given; the policy extracts the host.
    pub remote: &'a str,
    /// Arguments after the program name.
    pub args: &'a [&'a str],
    /// Directory git runs in.
    pub cwd: &'a Path,
    /// Standard input, for commands that read it.
    pub stdin: Option<&'a [u8]>,
    /// Extra environment for this one call (for example `GIT_ASKPASS`).
    pub env: &'a [(OsString, OsString)],
    /// When the call starts; supplied by the caller so tests are deterministic.
    pub at: OffsetDateTime,
}

/// Runs one remote git invocation with full environment control.
///
/// The real implementation spawns git; tests record calls or answer from a fixture. Unlike
/// [`crate::policy::GitRunner`], this trait sees the working directory, stdin and environment
/// because the backup flow needs all three (askpass env, temp clone cwd, `--stdin` config).
pub trait RemoteGitRunner: Send + Sync {
    /// Runs the invocation.
    ///
    /// # Errors
    /// [`GitError`] when the process cannot start, its output cannot be read or it times out.
    fn run(&self, call: &RemoteGitCall<'_>) -> Result<GitOutput, GitError>;
}

/// Spawns git with prompts disabled, bounded output, a deadline and caller-supplied env.
pub struct ProcessRemoteGitRunner {
    program: PathBuf,
    timeout: Duration,
    output_limit: usize,
    env: Vec<(OsString, OsString)>,
    cancel: Option<Arc<AtomicBool>>,
}

impl Default for ProcessRemoteGitRunner {
    fn default() -> Self {
        Self::git()
    }
}

impl ProcessRemoteGitRunner {
    /// The system git with the default deadline and output limit.
    #[must_use]
    pub fn git() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: REMOTE_GIT_TIMEOUT,
            output_limit: MAX_GIT_OUTPUT_BYTES,
            env: Vec::new(),
            cancel: None,
        }
    }

    /// Runs `program` instead of git, with an explicit deadline.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
            output_limit: MAX_GIT_OUTPUT_BYTES,
            env: Vec::new(),
            cancel: None,
        }
    }

    /// Caps how many bytes of stdout and stderr are kept.
    #[must_use]
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// Passes an extra environment variable to git.
    ///
    /// Tests use this to sandbox `HOME` and neutral configuration.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Sets an optional cancellation flag. When set during execution, the child process is killed
    /// and reaped immediately.
    #[must_use]
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// The program this runner executes.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

impl RemoteGitRunner for ProcessRemoteGitRunner {
    fn run(&self, call: &RemoteGitCall<'_>) -> Result<GitOutput, GitError> {
        let stdin = call.stdin.map(<[u8]>::to_vec);
        let mut command = Command::new(&self.program);
        let mut child = command
            .current_dir(call.cwd)
            .args(call.args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .envs(call.env.iter().map(|(key, value)| (key, value)))
            .spawn()
            .map_err(GitError::Spawn)?;

        let writer = stdin.and_then(|bytes| {
            child.stdin.take().map(|mut pipe| {
                thread::spawn(move || {
                    use std::io::Write;
                    let _ = pipe.write_all(&bytes);
                })
            })
        });

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
            if let Some(cancel) = &self.cancel
                && cancel.load(Ordering::Relaxed)
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_reader(stdout);
                let _ = join_reader(stderr);
                join_writer(writer);
                return Err(GitError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = join_reader(stdout);
                        let _ = join_reader(stderr);
                        join_writer(writer);
                        return Err(GitError::TimedOut(self.timeout));
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = join_reader(stdout);
                    let _ = join_reader(stderr);
                    join_writer(writer);
                    return Err(GitError::Wait(error));
                }
            }
        };

        let stdout = join_reader(stdout)?;
        let stderr = join_reader(stderr)?;
        join_writer(writer);
        Ok(GitOutput {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

fn join_writer(handle: Option<thread::JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

fn join_reader(
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

/// Credential material for one remote call, delivered without argv or URL.
///
/// Two shapes: [`GitAuth::token`] writes a private askpass script that echoes the token from
/// the process environment; [`GitAuth::none`] disables helpers and prompts for a remote that
/// needs no credential (a local path or `file://` URL in tests). The askpass directory is
/// removed on drop, so the script never outlives the calls it served.
pub struct GitAuth {
    env: Vec<(OsString, OsString)>,
    dir: Option<PathBuf>,
}

impl GitAuth {
    /// No credential: helpers and prompts are disabled for the call.
    #[must_use]
    pub fn none() -> Self {
        Self {
            env: vec![
                (OsString::from("GIT_ASKPASS"), OsString::from("")),
                (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
                (OsString::from("GCM_INTERACTIVE"), OsString::from("never")),
                (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
                (
                    OsString::from("GIT_CONFIG_KEY_0"),
                    OsString::from("credential.helper"),
                ),
                (OsString::from("GIT_CONFIG_VALUE_0"), OsString::from("")),
            ],
            dir: None,
        }
    }

    /// Token auth via a temporary askpass script.
    ///
    /// The script and its directory are created with owner-only permissions and deleted when
    /// this value drops. The token travels in `AGENTUNA_REMOTE_BACKUP_TOKEN`, which git never
    /// sees in argv; the script reads it from its own environment at exec time.
    ///
    /// # Errors
    /// [`GitError::Spawn`] is not used; this returns [`RemoteError::AuthSetup`] when the temp
    /// directory or script cannot be written.
    pub fn token(token: &str) -> Result<Self, RemoteError> {
        let dir = std::env::temp_dir().join(format!(
            "agentuna-askpass-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&dir).map_err(|source| RemoteError::AuthSetup {
            path: dir.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        let script = dir.join("askpass.sh");
        fs::write(&script, ASKPASS_SCRIPT).map_err(|source| RemoteError::AuthSetup {
            path: script.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o700));
        }
        Ok(Self {
            env: vec![
                (OsString::from("GIT_ASKPASS"), script.into_os_string()),
                (OsString::from(TOKEN_ENV), OsString::from(token)),
                (
                    OsString::from("AGENTUNA_REMOTE_BACKUP_USERNAME"),
                    OsString::from(TOKEN_USERNAME),
                ),
                (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
                (OsString::from("GCM_INTERACTIVE"), OsString::from("never")),
                (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
                (
                    OsString::from("GIT_CONFIG_KEY_0"),
                    OsString::from("credential.helper"),
                ),
                (OsString::from("GIT_CONFIG_VALUE_0"), OsString::from("")),
            ],
            dir: Some(dir),
        })
    }

    /// Environment pairs for one git invocation.
    #[must_use]
    pub fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }
}

impl Drop for GitAuth {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// Failure of a remote-backup git operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteError {
    /// The network policy refused the call; git was not run.
    #[error(transparent)]
    Denied(#[from] at_model::network::NetworkDenied),
    /// The action is not a git network action.
    #[error("`{0}` is not a git network action")]
    WrongAction(NetworkAction),
    /// The remote URL has no host (a local path is not a network remote).
    #[error("the remote backup URL has no host; remote backup needs an https or ssh URL")]
    NoRemoteHost,
    /// The remote URL uses an insecure transport (such as unencrypted http://).
    #[error(
        "remote backup requires a secure transport (https:// or ssh://); insecure http:// is refused"
    )]
    InsecureTransport,
    /// The process boundary failed.
    #[error(transparent)]
    Process(#[from] GitError),
    /// Git ran and failed.
    #[error("git {args} failed with {code}: {stderr}")]
    CommandFailed {
        /// Arguments after the program name, joined for display.
        args: String,
        /// Exit code, when the process exited normally.
        code: String,
        /// Bounded, redacted standard error.
        stderr: String,
    },
    /// Git printed something this module cannot parse.
    #[error("could not parse git {args} output: {reason}")]
    InvalidOutput {
        /// Arguments after the program name, joined for display.
        args: String,
        /// What did not parse.
        reason: String,
    },
    /// The askpass script or its directory could not be written.
    #[error("could not set up credential helper at {path}: {source}")]
    AuthSetup {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The push was rejected as non-fast-forward; the caller fetches and merges (TC-018).
    #[error("push rejected: remote has commits the local clone does not")]
    NonFastForward,
    /// The remote rejected the credential.
    #[error("the remote rejected the stored credential; re-enter the token in Settings")]
    AuthRejected,
    /// The remote could not be reached.
    #[error("the remote could not be reached; check the connection and try again")]
    Offline,
}

impl RemoteError {
    /// Whether this failure means "the network or the remote is unreachable".
    #[must_use]
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Offline)
    }

    /// Whether this failure means "the credential was rejected".
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::AuthRejected)
    }
}

/// Runs `call` through the network policy, then through `runner`.
///
/// This is the only entry point: it extracts the host, asks [`NetworkPolicy`], logs the event
/// and only then invokes the runner. A refusal returns [`RemoteError::Denied`] and the runner
/// is never touched.
pub fn run_remote_git(
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
    call: &RemoteGitCall<'_>,
    runner: &dyn RemoteGitRunner,
) -> Result<GitOutput, RemoteError> {
    if !is_remote_action(call.action) {
        return Err(RemoteError::WrongAction(call.action));
    }
    let host = if let Some(host) = at_model::network::NetworkHost::from_url(call.remote) {
        host
    } else if call.remote.starts_with("file://") || call.remote.starts_with('/') {
        at_model::network::NetworkHost::parse("localhost").map_err(|_| {
            RemoteError::Denied(at_model::network::NetworkDenied::invalid_host(call.action))
        })?
    } else {
        tracing::warn!(
            network = true,
            action = %call.action.wire_name(),
            reason = "invalid_host",
            "blocked remote-backup git call"
        );
        return Err(RemoteError::Denied(
            at_model::network::NetworkDenied::invalid_host(call.action),
        ));
    };
    let host = policy
        .authorize(call.action, host.as_str(), call.trigger)
        .map_err(RemoteError::Denied)?;
    tracing::info!(
        network = true,
        action = %call.action.wire_name(),
        host = %host,
        "remote-backup git call"
    );
    let _authorized = AuthorizedGitCall::for_remote(call.action, host.clone(), call.args);
    let output = runner.run(&RemoteGitCall {
        action: call.action,
        trigger: call.trigger,
        remote: call.remote,
        args: call.args,
        cwd: call.cwd,
        stdin: call.stdin,
        env: call.env,
        at: call.at,
    })?;
    let bytes = (output.stdout.len() + output.stderr.len()) as u64;
    log.record(at_model::network::NetworkEvent::new(
        call.at,
        call.action,
        Some(host),
        bytes,
    ));
    Ok(output)
}

fn is_remote_action(action: NetworkAction) -> bool {
    matches!(
        action,
        NetworkAction::GitFetch
            | NetworkAction::GitClone
            | NetworkAction::RemoteBackupPush
            | NetworkAction::RemoteBackupPull
    )
}

/// Decodes git's standard error for a message, bounded and stripped of any token the askpass
/// env might have leaked into a diagnostic.
pub fn remote_diagnostic(bytes: &[u8], redactor: &dyn Fn(&str) -> String) -> String {
    let text = String::from_utf8_lossy(bytes);
    let cleaned = redactor(&text);
    cleaned.chars().take(MAX_REMOTE_DIAGNOSTIC_CHARS).collect()
}

/// True when git's stderr reports an authentication failure.
pub fn looks_like_auth_failure(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    text.contains("authentication failed")
        || text.contains("could not read username")
        || text.contains("could not read password")
        || text.contains("invalid username or token")
        || text.contains("password authentication is not supported")
        || (text.contains("403") && text.contains("forbidden"))
        || text.contains("terminal prompts disabled")
}

/// True when git's stderr reports the remote is unreachable or the transfer dropped.
pub fn looks_like_offline(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    text.contains("could not resolve host")
        || text.contains("failed to connect")
        || text.contains("connection refused")
        || text.contains("connection timed out")
        || text.contains("network is unreachable")
        || text.contains("no route to host")
        || text.contains("temporary failure in name resolution")
        || text.contains("operation timed out")
        || text.contains("remote end hung up")
        || text.contains("unexpected disconnect")
        || text.contains("connection reset")
        || text.contains("early eof")
        || text.contains("transfer closed")
        || text.contains("rpc failed")
}

/// True when a push was rejected because the remote moved ahead.
pub fn looks_like_non_fast_forward(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    text.contains("non-fast-forward")
        || text.contains("fetch first")
        || text.contains("failed to push some refs")
            && (text.contains("rejected") || text.contains("stale info"))
}

/// Runs one remote git command and maps a non-zero exit into [`RemoteError`].
///
/// `redact` masks the token in diagnostics; callers pass a closure backed by
/// [`at_model::redact::Redactor`] or a one-value masker so the token can never appear in an error.
pub fn remote_git(
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
    runner: &dyn RemoteGitRunner,
    call: &RemoteGitCall<'_>,
    redact: &dyn Fn(&str) -> String,
) -> Result<GitOutput, RemoteError> {
    let output = run_remote_git(policy, log, call, runner)?;
    if output.success {
        return Ok(output);
    }
    let stderr = remote_diagnostic(&output.stderr, redact);
    if looks_like_auth_failure(&output.stderr) {
        return Err(RemoteError::AuthRejected);
    }
    if looks_like_offline(&output.stderr) {
        return Err(RemoteError::Offline);
    }
    if call.args.first() == Some(&"push") && looks_like_non_fast_forward(&output.stderr) {
        return Err(RemoteError::NonFastForward);
    }
    Err(RemoteError::CommandFailed {
        args: call.args.join(" "),
        code: output
            .code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        stderr,
    })
}

// ---------------------------------------------------------------------------
// Repository helpers built on top of remote_git
// ---------------------------------------------------------------------------

/// Result of `git ls-remote --symref <remote> HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHead {
    /// Default branch name when the remote reports one (for example `main`).
    pub branch: Option<String>,
    /// Commit the remote's HEAD points at.
    pub commit: String,
}

impl RemoteContext<'_> {
    /// Reads the remote's default branch and HEAD commit without cloning.
    ///
    /// # Errors
    /// [`RemoteError`] when the policy refuses, the remote is unreachable or the output is not
    /// the expected `ref:`/`HEAD` pair.
    pub fn remote_head(&mut self, remote: &str, auth: &GitAuth) -> Result<RemoteHead, RemoteError> {
        let args = ["ls-remote", "--symref", remote, "HEAD"];
        let cwd = std::env::temp_dir();
        let call = RemoteGitCall {
            action: NetworkAction::GitFetch,
            trigger: NetworkTrigger::User,
            remote,
            args: &args,
            cwd: &cwd,
            stdin: None,
            env: auth.env(),
            at: self.now,
        };
        let output = self.git(&call)?;
        parse_ls_remote_head(&output.stdout)
    }
    /// Runs one remote git command through the policy gate.
    ///
    /// # Errors
    /// [`RemoteError`] on refusal, process failure or a classified git error.
    pub fn git(&mut self, call: &RemoteGitCall<'_>) -> Result<GitOutput, RemoteError> {
        remote_git(self.policy, self.log, self.runner, call, self.redact)
    }
}

/// Validates that `url` is a supported remote backup URL.
///
/// Refuses unencrypted `http://` URLs (backups must travel over an encrypted connection),
/// URLs without a host, and malformed URLs.
///
/// # Errors
/// [`RemoteError::InsecureTransport`] or [`RemoteError::NoRemoteHost`].
pub fn validate_repo_url(url: &str) -> Result<(), RemoteError> {
    let raw = url.trim();
    if raw.is_empty() {
        return Err(RemoteError::NoRemoteHost);
    }
    if raw.starts_with("http://") {
        return Err(RemoteError::InsecureTransport);
    }
    if raw.starts_with("file://") || raw.starts_with('/') {
        return Ok(());
    }
    let Some(_) = at_model::network::NetworkHost::from_url(raw) else {
        return Err(RemoteError::NoRemoteHost);
    };
    Ok(())
}

fn parse_ls_remote_head(bytes: &[u8]) -> Result<RemoteHead, RemoteError> {
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Ok(RemoteHead {
            branch: None,
            commit: String::new(),
        });
    }
    let mut branch = None;
    let mut commit = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("ref:") {
            // "ref: refs/heads/main\tHEAD"
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if let Some(reference) = parts.first()
                && let Some(name) = reference.strip_prefix("refs/heads/")
            {
                branch = Some(name.to_owned());
            }
        } else {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 && parts[1] == "HEAD" {
                commit = Some(parts[0].to_owned());
            }
        }
    }
    let commit = commit.ok_or_else(|| RemoteError::InvalidOutput {
        args: "ls-remote --symref".to_owned(),
        reason: "no HEAD line".to_owned(),
    })?;
    Ok(RemoteHead { branch, commit })
}
///
/// Bundled so the helpers below stay readable; a caller builds one per user action and reuses
/// it for every git call that action makes.
pub struct RemoteContext<'a> {
    /// Network policy every call must pass.
    pub policy: &'a NetworkPolicy,
    /// In-memory event log the policy appends permitted calls to.
    pub log: &'a mut NetworkEventLog,
    /// Runner that spawns git.
    pub runner: &'a dyn RemoteGitRunner,
    /// Local runner for clone-local commands (init, commit, status, add).
    pub local: &'a dyn crate::local::LocalGitRunner,
    /// Masks the token in diagnostics before they reach an error or a log.
    pub redact: &'a dyn Fn(&str) -> String,
    /// Clock for event timestamps; injectable so tests are deterministic.
    pub now: OffsetDateTime,
}

/// Commits staged changes in `repo` with `message` and returns the new commit id.
///
/// Uses a local-only git invocation; the caller stages first. The author/committer come from
/// the repo's git config or the environment, never from argv secrets.
///
/// # Errors
/// [`RemoteError::CommandFailed`] or [`RemoteError::InvalidOutput`].
pub fn commit_local(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
    message: &str,
) -> Result<String, RemoteError> {
    let args = [
        "-c",
        "user.name=AgenTuna",
        "-c",
        "user.email=agentuna@localhost",
        "commit",
        "-m",
        message,
    ];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    rev_parse_head(runner, repo)
}

/// Reads `HEAD`'s commit id in `repo`.
///
/// # Errors
/// [`RemoteError`] on git failure or unparseable output.
pub fn rev_parse_head(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
) -> Result<String, RemoteError> {
    let args = ["rev-parse", "HEAD"];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let commit = text.trim();
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RemoteError::InvalidOutput {
            args: args.join(" "),
            reason: format!("unexpected rev-parse output: {commit:?}"),
        });
    }
    Ok(commit.to_owned())
}

/// Whether `repo` has staged or unstaged changes against `HEAD`.
///
/// # Errors
/// [`RemoteError`] on git failure.
pub fn has_changes(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
) -> Result<bool, RemoteError> {
    let args = ["status", "--porcelain=v1", "-z", "--untracked-files=all"];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: true,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    Ok(!output.stdout.is_empty())
}

/// Stages every path under `repo` with `git add -A`.
///
/// Used only on the temp clone the backup code owns; the user's real store is never staged
/// here. `-A` is safe because the clone contains only the snapshot files the backup wrote.
///
/// # Errors
/// [`RemoteError`] on git failure.
pub fn stage_all(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
) -> Result<(), RemoteError> {
    let args = ["add", "-A"];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: true,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    Ok(())
}

impl RemoteContext<'_> {
    /// Clones `remote` into `dest` (which must not exist) on the remote's default branch.
    ///
    /// A dropped connection is retried once after a short fixed backoff; a partial clone from
    /// the first attempt is removed first. Two failed attempts return the typed offline error so
    /// the caller can offer the explicit retry action.
    ///
    /// # Errors
    /// [`RemoteError`] on policy refusal, git failure or auth/offline classification.
    pub fn clone_remote(
        &mut self,
        remote: &str,
        dest: &Path,
        auth: &GitAuth,
    ) -> Result<(), RemoteError> {
        let mut attempt = 1;
        loop {
            match self.clone_remote_once(remote, dest, auth) {
                Ok(()) => return Ok(()),
                Err(error) if attempt < CLONE_ATTEMPTS && matches!(error, RemoteError::Offline) => {
                    if fs::symlink_metadata(dest).is_ok() {
                        let _ = fs::remove_dir_all(dest);
                    }
                    thread::sleep(CLONE_RETRY_BACKOFF);
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// One clone attempt; [`Self::clone_remote`] owns the bounded retry.
    fn clone_remote_once(
        &mut self,
        remote: &str,
        dest: &Path,
        auth: &GitAuth,
    ) -> Result<(), RemoteError> {
        let remote_text = remote.to_owned();
        let dest_text = dest.to_string_lossy().into_owned();
        let args = ["clone", "--quiet", remote, dest_text.as_str()];
        let call = RemoteGitCall {
            action: NetworkAction::GitClone,
            trigger: NetworkTrigger::User,
            remote: &remote_text,
            args: &args,
            cwd: dest.parent().unwrap_or(Path::new("/")),
            stdin: None,
            env: auth.env(),
            at: self.now,
        };
        self.git(&call)?;
        Ok(())
    }

    /// Fetches `remote` in `repo`, updating remote-tracking refs.
    ///
    /// # Errors
    /// [`RemoteError`] on policy refusal, git failure or auth/offline classification.
    pub fn fetch_remote(
        &mut self,
        repo: &Path,
        remote: &str,
        auth: &GitAuth,
    ) -> Result<(), RemoteError> {
        let args = ["fetch", "--quiet", "origin"];
        let call = RemoteGitCall {
            action: NetworkAction::GitFetch,
            trigger: NetworkTrigger::User,
            remote,
            args: &args,
            cwd: repo,
            stdin: None,
            env: auth.env(),
            at: self.now,
        };
        self.git(&call)?;
        Ok(())
    }

    /// Pushes `repo`'s current branch to `remote`'s matching branch.
    ///
    /// `force` is never set: a non-fast-forward push returns [`RemoteError::NonFastForward`] for
    /// the caller to hand to the merge flow (TC-018).
    ///
    /// # Errors
    /// [`RemoteError`] on policy refusal, git failure, auth/offline or non-fast-forward.
    pub fn push_remote(
        &mut self,
        repo: &Path,
        remote: &str,
        branch: &str,
        auth: &GitAuth,
    ) -> Result<(), RemoteError> {
        let refspec = format!("HEAD:refs/heads/{branch}");
        let args = ["push", "--quiet", "origin", refspec.as_str()];
        let call = RemoteGitCall {
            action: NetworkAction::RemoteBackupPush,
            trigger: NetworkTrigger::User,
            remote,
            args: &args,
            cwd: repo,
            stdin: None,
            env: auth.env(),
            at: self.now,
        };
        self.git(&call)?;
        Ok(())
    }
}

/// Initializes a new repository at `dir` for the first backup push.
///
/// Local-only: no network. The caller then commits the snapshot and pushes.
///
/// # Errors
/// [`RemoteError::CommandFailed`] on git failure.
pub fn init_repo(runner: &dyn crate::local::LocalGitRunner, dir: &Path) -> Result<(), RemoteError> {
    let args = ["init", "-q", "-b", "main"];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo: dir,
            args: &args,
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    Ok(())
}

/// Sets `remote.origin.url` in `repo` without embedding credentials.
///
/// The URL stored is the token-free form; the askpass supplies the password at push time.
///
/// # Errors
/// [`RemoteError::CommandFailed`] on git failure.
pub fn set_remote_url(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
    url: &str,
) -> Result<(), RemoteError> {
    let args = ["remote", "add", "origin", url];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        // `remote add` fails when origin already exists; try `set-url` instead.
        let args = ["remote", "set-url", "origin", url];
        let output = runner
            .run(&crate::local::LocalGitCall {
                repo,
                args: &args,
                stdin: None,
                literal_pathspecs: false,
            })
            .map_err(RemoteError::Process)?;
        if !output.success {
            return Err(RemoteError::CommandFailed {
                args: args.join(" "),
                code: output
                    .code
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                    .collect(),
            });
        }
    }
    Ok(())
}

/// Reads the current branch name of `repo` (`git symbolic-ref --short HEAD`).
///
/// # Errors
/// [`RemoteError::CommandFailed`] or [`RemoteError::InvalidOutput`].
pub fn current_branch(
    runner: &dyn crate::local::LocalGitRunner,
    repo: &Path,
) -> Result<String, RemoteError> {
    let args = ["symbolic-ref", "--short", "HEAD"];
    let output = runner
        .run(&crate::local::LocalGitCall {
            repo,
            args: &args,
            stdin: None,
            literal_pathspecs: false,
        })
        .map_err(RemoteError::Process)?;
    if !output.success {
        return Err(RemoteError::CommandFailed {
            args: args.join(" "),
            code: output
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(MAX_REMOTE_DIAGNOSTIC_CHARS)
                .collect(),
        });
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if branch.is_empty() {
        return Err(RemoteError::InvalidOutput {
            args: args.join(" "),
            reason: "empty branch name".to_owned(),
        });
    }
    Ok(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_is_classified() {
        assert!(looks_like_auth_failure(
            b"fatal: Authentication failed for 'https://github.com/x/y.git/'"
        ));
        assert!(looks_like_auth_failure(b"fatal: could not read Password"));
        assert!(!looks_like_auth_failure(b"fatal: not a git repository"));
    }

    #[test]
    fn offline_is_classified() {
        assert!(looks_like_offline(
            b"fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host"
        ));
        assert!(looks_like_offline(
            b"fatal: unable to connect to github.com: Connection refused"
        ));
        assert!(!looks_like_offline(b"fatal: Authentication failed"));
    }

    #[test]
    fn non_fast_forward_is_classified() {
        assert!(looks_like_non_fast_forward(
            b"! [rejected] main -> main (non-fast-forward)\nfatal: failed to push some refs"
        ));
        assert!(!looks_like_non_fast_forward(
            b"fatal: Authentication failed"
        ));
    }

    #[test]
    fn ls_remote_head_parses() {
        let bytes = b"ref: refs/heads/main\tHEAD\nabc123def456\tHEAD\n";
        let head = parse_ls_remote_head(bytes).unwrap();
        assert_eq!(head.branch.as_deref(), Some("main"));
        assert_eq!(head.commit, "abc123def456");
    }

    #[test]
    fn askpass_script_never_contains_the_token() {
        let test_token = format!("{}{}", "ghp_", "secretvalue1234567890");
        let auth = GitAuth::token(&test_token).unwrap();
        let env_text = format!("{:?}", auth.env());
        // The token IS in env (that's the delivery), but the script file must not contain it.
        let dir = auth.dir.as_ref().unwrap().clone();
        let script = fs::read_to_string(dir.join("askpass.sh")).unwrap();
        assert!(!script.contains(&test_token));
        assert!(script.contains("AGENTUNA_REMOTE_BACKUP_TOKEN"));
        // env carries it under the documented variable name
        assert!(env_text.contains(TOKEN_ENV));
        drop(auth);
        assert!(!dir.exists(), "askpass dir removed on drop");
    }
}
