//! stdio MCP client sessions: spawn, handshake, tool listing and bounded shutdown.
//!
//! # Consent is a type, not a convention
//!
//! A definition found on disk is data; running it executes code on this machine. The two
//! are kept apart by [`LaunchConsent`]: it has no constructor that accepts a bare
//! [`McpServer`] or a path, only [`LaunchConsent::user_approved`], which is meant to be
//! called from the user gesture that asked to run or test a server. [`StdioClient::open`]
//! refuses to spawn without one and re-checks that the approved launch still equals the
//! launch it received, so a token minted for one command cannot run another.
//!
//! # What the harness owns
//!
//! rmcp supplies the JSON-RPC client and the line-delimited stdio framing. This module
//! owns everything the spec leaves to the application:
//!
//! * the child runs in its own process group (`setpgid`) so `npx`'s `node` grandchild is
//!   signalled too, and never through a shell;
//! * the environment is the inherited parent environment plus the server's resolved
//!   entries — secret values go to the environment only, and a resolved secret found in
//!   argv aborts the launch before the process exists;
//! * stderr is drained continuously into an 8 KiB ring buffer, so a chatty server cannot
//!   block on a full pipe and the buffer cannot grow without bound;
//! * the handshake, each `tools/list` page and shutdown are bounded, and a
//!   [`CancellationToken`] is honoured at every await;
//! * shutdown escalates `SIGINT` → 2 s → `SIGTERM` → 3 s → `SIGKILL`, always signalling
//!   the whole process group, then reaps the child and clears stragglers.
//!
//! # Timeout policy
//!
//! [`TimeoutPolicy::for_command`] derives the startup budget from the executable name:
//! package runners (`npx`, `pnpm`, `bunx`, `uvx`, `pipx`) may spend 180 s on a first-run
//! download, everything else gets 20 s. The handshake is additionally allowed
//! [`TimeoutPolicy::initialize`] (20 s), and because a first-run download happens *inside*
//! that window, the wait for the `initialize` response is
//! `max(startup, initialize)` — see [`TimeoutPolicy::handshake_budget`].

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use at_model::{EnvValue, McpServer, McpTransport};
use at_secrets::{KeychainId, Redactor, SecretStore, SecretString};
use rmcp::ServiceError;
use rmcp::model::{
    ClientCapabilities, ClientConfig, Implementation, PaginatedRequestParams, ProtocolVersion,
    ServerPeerInfo, Tool,
};
use rmcp::service::{RoleClient, RunningService, serve_client_with_ct};
use rmcp::transport::async_rw::AsyncRwTransport;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::path::{PathResolver, PathSource, ResolveError};

/// Startup budget for a package runner, whose first run may download the server.
pub const PACKAGE_RUNNER_STARTUP: Duration = Duration::from_secs(180);

/// Startup budget for every other executable.
pub const DEFAULT_STARTUP: Duration = Duration::from_secs(20);

/// Budget for the `initialize` request itself.
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for one `tools/list` page.
pub const TOOL_PAGE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long `SIGINT` gets before the harness escalates to `SIGTERM`.
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(2);

/// How long `SIGTERM` gets before the harness escalates to `SIGKILL`.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// Case: a definition ties the ceiling to the tools the UI can show.
pub const TOOL_LIMIT: usize = 500;

/// Capacity of the stderr ring buffer.
pub const STDERR_RING_BYTES: usize = 8 * 1024;

/// Executable basenames whose first run may download a package.
pub const PACKAGE_RUNNER_COMMANDS: [&str; 5] = ["npx", "pnpm", "bunx", "uvx", "pipx"];

/// Budget for the process group to disappear after a kill.
const GROUP_SETTLE: Duration = Duration::from_secs(2);

/// How much of the captured stderr a user-facing error carries.
const STDERR_EXCERPT_CHARS: usize = 600;

// ---------------------------------------------------------------------------
// Timeout policy
// ---------------------------------------------------------------------------

/// Every deadline one session uses.
///
/// The defaults are the shipped policy; tests replace them with millisecond budgets so
/// the suite does not sleep through the real 20 s clock. [`StdioClient::with_timeouts`]
/// is the seam for that, and for a caller that needs a different trade-off.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimeoutPolicy {
    /// Process start to the `initialize` response.
    pub startup: Duration,
    /// The `initialize` exchange on its own.
    pub initialize: Duration,
    /// One `tools/list` page.
    pub tool_page: Duration,
    /// Grace after `SIGINT`.
    pub interrupt_grace: Duration,
    /// Grace after `SIGTERM`.
    pub terminate_grace: Duration,
    /// Maximum number of tools collected across pages.
    pub tool_limit: usize,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            startup: DEFAULT_STARTUP,
            initialize: INITIALIZE_TIMEOUT,
            tool_page: TOOL_PAGE_TIMEOUT,
            interrupt_grace: INTERRUPT_GRACE,
            terminate_grace: TERMINATE_GRACE,
            tool_limit: TOOL_LIMIT,
        }
    }
}

impl TimeoutPolicy {
    /// The shipped policy for an executable, by basename.
    ///
    /// `npx`, `pnpm`, `bunx`, `uvx` and `pipx` get [`PACKAGE_RUNNER_STARTUP`]; every
    /// other name keeps [`DEFAULT_STARTUP`]. A directory component and a Windows
    /// `.exe`/`.cmd`/`.bat` suffix are ignored.
    #[must_use]
    pub fn for_command(command: &str) -> Self {
        let startup = if is_package_runner(command) {
            PACKAGE_RUNNER_STARTUP
        } else {
            DEFAULT_STARTUP
        };
        Self {
            startup,
            ..Self::default()
        }
    }

    /// Replaces the three phase budgets, keeping the shutdown graces and tool ceiling.
    #[must_use]
    pub fn with_budgets(
        mut self,
        startup: Duration,
        initialize: Duration,
        tool_page: Duration,
    ) -> Self {
        self.startup = startup;
        self.initialize = initialize;
        self.tool_page = tool_page;
        self
    }

    /// Replaces the shutdown graces.
    #[must_use]
    pub fn with_graces(mut self, interrupt_grace: Duration, terminate_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.terminate_grace = terminate_grace;
        self
    }

    /// The budget for spawn + `initialize`.
    ///
    /// This is the longer of [`Self::startup`] and [`Self::initialize`]: a package
    /// runner's first-run download happens before the server can answer `initialize`,
    /// so the startup budget applies to the response wait instead of being swallowed by
    /// a shorter per-request timeout.
    #[must_use]
    pub fn handshake_budget(&self) -> Duration {
        self.startup.max(self.initialize)
    }
}

/// The file name of `command`, with a Windows executable suffix removed.
fn command_basename(command: &str) -> &str {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    for suffix in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

/// Whether `command` is a package runner that may download on first use.
#[must_use]
pub fn is_package_runner(command: &str) -> bool {
    let basename = command_basename(command);
    PACKAGE_RUNNER_COMMANDS
        .iter()
        .any(|runner| runner.eq_ignore_ascii_case(basename))
}

// ---------------------------------------------------------------------------
// Launch description and consent
// ---------------------------------------------------------------------------

/// One stdio server exactly as it would be started.
///
/// This is a plain description: holding one runs nothing. Read it out of a stored
/// [`McpServer`] with [`StdioLaunch::from_server`] and pass it to
/// [`StdioClient::open`] together with a [`LaunchConsent`].
#[derive(Clone, PartialEq, Eq)]
pub struct StdioLaunch {
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, EnvValue>,
}

impl StdioLaunch {
    /// A launch with no arguments, working directory or environment entries.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    /// Sets the argument vector, passed verbatim and never through a shell.
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the child's working directory.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets the server's environment entries.
    #[must_use]
    pub fn with_env(mut self, env: BTreeMap<String, EnvValue>) -> Self {
        self.env = env;
        self
    }

    /// Reads the stdio half of a stored server.
    ///
    /// # Errors
    /// [`SessionErrorKind::NotStdio`] when the server is an HTTP endpoint; there is no
    /// child process to start.
    pub fn from_server(server: &McpServer) -> Result<Self, SessionError> {
        Self::from_transport(&server.transport)
    }

    /// Reads the stdio half of a transport.
    ///
    /// # Errors
    /// [`SessionErrorKind::NotStdio`] for [`McpTransport::Http`].
    pub fn from_transport(transport: &McpTransport) -> Result<Self, SessionError> {
        match transport {
            McpTransport::Stdio {
                command,
                args,
                cwd,
                env,
            } => Ok(Self {
                command: command.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                env: env.clone(),
            }),
            McpTransport::Http { .. } => Err(SessionError::not_stdio()),
        }
    }

    /// The configured command, as written in the definition.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Arguments passed to the executable.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Working directory, when the definition pins one.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Environment entries, literal or secret references.
    #[must_use]
    pub fn env(&self) -> &BTreeMap<String, EnvValue> {
        &self.env
    }

    /// The executable's basename, used for the timeout policy.
    #[must_use]
    pub fn command_basename(&self) -> &str {
        command_basename(&self.command)
    }

    /// The shipped timeout policy for this launch's command.
    #[must_use]
    pub fn timeout_policy(&self) -> TimeoutPolicy {
        TimeoutPolicy::for_command(&self.command)
    }
}

/// Arguments and environment values are not printed.
impl fmt::Debug for StdioLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioLaunch")
            .field("command", &self.command)
            .field("arg_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Proof that the user approved starting exactly this stdio server.
///
/// The token has no `From<McpServer>` and no public field: the only way to obtain one is
/// [`LaunchConsent::user_approved`], whose call sites are the approval gestures (Test
/// connection, Enable, Run). Discovery, import previews and background scans never call
/// it, so a definition that was merely found on disk cannot reach
/// [`StdioClient::open`].
///
/// Approval is bound to the launch: changing the command, an argument, the working
/// directory or an environment entry invalidates the token and `open` refuses with
/// [`SessionErrorKind::ConsentMismatch`].
#[derive(Clone)]
pub struct LaunchConsent {
    approved: StdioLaunch,
}

impl LaunchConsent {
    /// Records the user's approval of `launch`.
    ///
    /// Call this only from the gesture that asked to run or test the server, or from the
    /// code that applies a user's explicit "run without asking again" preference.
    #[must_use]
    pub fn user_approved(launch: &StdioLaunch) -> Self {
        Self {
            approved: launch.clone(),
        }
    }

    /// Whether this token approves `launch`.
    #[must_use]
    pub fn matches(&self, launch: &StdioLaunch) -> bool {
        self.approved == *launch
    }

    /// The launch this token approves.
    #[must_use]
    pub fn approved_launch(&self) -> &StdioLaunch {
        &self.approved
    }
}

impl fmt::Debug for LaunchConsent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchConsent")
            .field("approved_for", &self.approved)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Stable classification of a session failure.
///
/// The kind is for code and telemetry; [`SessionError::ui_message`] is what a screen may
/// render verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionErrorKind {
    /// No [`LaunchConsent`], or one minted for a different launch.
    ConsentMismatch,
    /// The server is not stdio.
    NotStdio,
    /// The command is not on PATH and not an absolute path.
    UnresolvedCommand,
    /// A file with the command's name exists but cannot be executed.
    NotExecutable,
    /// The operating system refused to execute the file.
    PermissionDenied,
    /// A referenced Keychain entry is missing.
    MissingSecret,
    /// The configured working directory does not exist.
    InvalidWorkingDirectory,
    /// A resolved secret value appears in the argument vector.
    SecretInArguments,
    /// The process could not be spawned for another reason.
    SpawnFailed,
    /// No `initialize` response inside the startup budget.
    StartupTimeout,
    /// No `tools/list` page inside the per-page budget.
    ToolListTimeout,
    /// The client and server share no protocol version.
    ProtocolVersionMismatch,
    /// The peer answered with something that is not a valid MCP message.
    ProtocolError,
    /// The process exited before the session was ready.
    Exited,
    /// The caller's cancellation token fired.
    Cancelled,
    /// The session was already shut down.
    Closed,
    /// The process or its group could not be torn down.
    ShutdownFailed,
    /// Path resolution is not implemented on this platform.
    PlatformUnsupported,
}

impl SessionErrorKind {
    /// Stable tag for logs and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConsentMismatch => "consent_mismatch",
            Self::NotStdio => "not_stdio",
            Self::UnresolvedCommand => "unresolved_command",
            Self::NotExecutable => "not_executable",
            Self::PermissionDenied => "permission_denied",
            Self::MissingSecret => "missing_secret",
            Self::InvalidWorkingDirectory => "invalid_working_directory",
            Self::SecretInArguments => "secret_in_arguments",
            Self::SpawnFailed => "spawn_failed",
            Self::StartupTimeout => "startup_timeout",
            Self::ToolListTimeout => "tool_list_timeout",
            Self::ProtocolVersionMismatch => "protocol_version_mismatch",
            Self::ProtocolError => "protocol_error",
            Self::Exited => "exited",
            Self::Cancelled => "cancelled",
            Self::Closed => "closed",
            Self::ShutdownFailed => "shutdown_failed",
            Self::PlatformUnsupported => "platform_unsupported",
        }
    }
}

impl fmt::Display for SessionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed stdio session with a UI-ready message.
///
/// Messages never carry a resolved secret: stderr excerpts go through the session's
/// [`Redactor`] and arguments are described, not printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    kind: SessionErrorKind,
    message: String,
    stderr_excerpt: Option<String>,
    hint: Option<String>,
}

impl SessionError {
    /// Builds an error from a kind and a message.
    ///
    /// Prefer the specific helpers inside this crate; this constructor exists for
    /// callers that need to re-wrap a failure while keeping the taxonomy.
    #[must_use]
    pub fn new(kind: SessionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            stderr_excerpt: None,
            hint: None,
        }
    }

    /// The classification.
    #[must_use]
    pub fn kind(&self) -> SessionErrorKind {
        self.kind
    }

    /// The message a screen may render.
    #[must_use]
    pub fn ui_message(&self) -> &str {
        &self.message
    }

    /// A redacted excerpt of the server's stderr, when it wrote anything.
    #[must_use]
    pub fn stderr_excerpt(&self) -> Option<&str> {
        self.stderr_excerpt.as_deref()
    }

    /// A concrete next step, when the failure has one (for example a version-manager
    /// hint).
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    fn with_stderr(mut self, excerpt: Option<String>) -> Self {
        self.stderr_excerpt = excerpt;
        self
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn consent_mismatch() -> Self {
        Self::new(
            SessionErrorKind::ConsentMismatch,
            "AgenTuna will not start this server because the approval does not match its \
             current definition. Approve the server again to continue.",
        )
    }

    fn not_stdio() -> Self {
        Self::new(
            SessionErrorKind::NotStdio,
            "This server uses HTTP, so there is no local process to start.",
        )
    }

    fn unresolved(command: &str, hint: Option<String>) -> Self {
        let error = Self::new(
            SessionErrorKind::UnresolvedCommand,
            format!(
                "Could not find `{command}`. It may be installed in a shell AgenTuna cannot \
                 see from the app."
            ),
        );
        match hint {
            Some(hint) => error.with_hint(hint),
            None => error,
        }
    }

    fn not_executable(path: &Path) -> Self {
        Self::new(
            SessionErrorKind::NotExecutable,
            format!(
                "{} exists but is not executable. Check its permissions.",
                path.display()
            ),
        )
    }

    fn permission_denied(program: &Path) -> Self {
        Self::new(
            SessionErrorKind::PermissionDenied,
            format!(
                "macOS refused to run {}. Allow it in System Settings > Privacy & Security, \
                 then try again.",
                program.display()
            ),
        )
    }

    fn missing_secret(name: &str) -> Self {
        Self::new(
            SessionErrorKind::MissingSecret,
            format!(
                "The secret for `{name}` is not in the Keychain. Add it under Secrets, then \
                 try again."
            ),
        )
    }

    fn invalid_working_directory(path: &Path) -> Self {
        Self::new(
            SessionErrorKind::InvalidWorkingDirectory,
            format!(
                "The working directory {} does not exist. Fix the path in the server \
                 definition.",
                path.display()
            ),
        )
    }

    fn secret_in_arguments() -> Self {
        Self::new(
            SessionErrorKind::SecretInArguments,
            "A resolved secret appears in this server's arguments, where other users and \
             process listings can read it. Move the value to an environment entry and try \
             again.",
        )
    }

    fn spawn_failed(program: &Path, reason: &str) -> Self {
        Self::new(
            SessionErrorKind::SpawnFailed,
            format!("Could not start {}: {reason}", program.display()),
        )
    }

    fn startup_timeout(budget: Duration) -> Self {
        Self::new(
            SessionErrorKind::StartupTimeout,
            format!(
                "The server did not finish starting within {} s. It may be waiting for a \
                 first-run download or a network call.",
                budget.as_secs_f64()
            ),
        )
    }

    fn tool_list_timeout(budget: Duration) -> Self {
        Self::new(
            SessionErrorKind::ToolListTimeout,
            format!(
                "The server started but did not list its tools within {} s.",
                budget.as_secs_f64()
            ),
        )
    }

    fn protocol_version_mismatch(detail: &str) -> Self {
        Self::new(
            SessionErrorKind::ProtocolVersionMismatch,
            format!("The server does not support a protocol version AgenTuna speaks. {detail}"),
        )
    }

    fn protocol_error(detail: &str) -> Self {
        Self::new(
            SessionErrorKind::ProtocolError,
            format!("The server replied with something that is not a valid MCP message. {detail}"),
        )
    }

    fn exited(exit: Option<ExitSummary>) -> Self {
        let detail = exit.map_or_else(
            || "without an exit status".to_owned(),
            |exit| exit.to_string(),
        );
        Self::new(
            SessionErrorKind::Exited,
            format!("The server exited before it was ready ({detail})."),
        )
    }

    fn cancelled() -> Self {
        Self::new(
            SessionErrorKind::Cancelled,
            "The connection check was cancelled.",
        )
    }

    fn closed() -> Self {
        Self::new(
            SessionErrorKind::Closed,
            "The session is closed. Open the server again to retry.",
        )
    }

    fn internal(detail: &str) -> Self {
        Self::new(SessionErrorKind::SpawnFailed, detail.to_owned())
    }

    fn from_resolve(error: ResolveError) -> Self {
        match error {
            ResolveError::InvalidCommand { command, reason } => Self::new(
                SessionErrorKind::UnresolvedCommand,
                format!("`{command}` is not a valid command name: {reason}."),
            ),
            ResolveError::NotFound { command, hint, .. } => {
                let hint = hint.map(|hint| hint.message);
                Self::unresolved(&command, hint)
            }
            ResolveError::NotExecutable { path, .. } => Self::not_executable(&path),
            ResolveError::UnsupportedPlatform => Self::new(
                SessionErrorKind::PlatformUnsupported,
                "Starting MCP servers is not supported on this platform.",
            ),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

// ---------------------------------------------------------------------------
// Stderr ring buffer
// ---------------------------------------------------------------------------

/// The last [`STDERR_RING_BYTES`] bytes a server wrote to stderr.
///
/// A chatty server is common (npm logs progress, Node prints warnings), so the reader
/// task must never stop draining, and the memory must not grow with the output. The
/// buffer keeps the newest bytes, which is what explains a failure.
#[derive(Debug)]
pub struct StderrBuffer {
    inner: Mutex<StderrState>,
}

#[derive(Debug)]
struct StderrState {
    bytes: VecDeque<u8>,
    capacity: usize,
    total: u64,
}

impl StderrBuffer {
    /// A buffer that keeps the last `capacity` bytes.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(StderrState {
                bytes: VecDeque::with_capacity(capacity),
                capacity,
                total: 0,
            }),
        }
    }

    /// Appends `chunk`, dropping the oldest bytes once the capacity is reached.
    pub fn push(&self, chunk: &[u8]) {
        let mut state = self.lock();
        state.total = state.total.saturating_add(chunk.len() as u64);
        if chunk.len() >= state.capacity {
            state.bytes.clear();
            let start = chunk.len() - state.capacity;
            state.bytes.extend(&chunk[start..]);
            return;
        }
        let overflow = state
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(state.capacity);
        if overflow > 0 {
            state.bytes.drain(..overflow);
        }
        state.bytes.extend(chunk);
    }

    /// How many bytes are buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().bytes.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().bytes.is_empty()
    }

    /// Total bytes seen, including those already dropped.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.lock().total
    }

    /// The buffered bytes as text, lossy for invalid UTF-8.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// A point-in-time view for UI and diagnostics.
    #[must_use]
    pub fn snapshot(&self) -> StderrSnapshot {
        let bytes = self.bytes();
        let total = self.total_bytes();
        StderrSnapshot {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            kept_bytes: bytes.len(),
            total_bytes: total,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let state = self.lock();
        let (first, second) = state.bytes.as_slices();
        let mut bytes = Vec::with_capacity(state.bytes.len());
        bytes.extend_from_slice(first);
        bytes.extend_from_slice(second);
        bytes
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StderrState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for StderrBuffer {
    fn default() -> Self {
        Self::new(STDERR_RING_BYTES)
    }
}

/// A consistent view of the stderr ring at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StderrSnapshot {
    /// Buffered text, lossy for invalid UTF-8.
    pub text: String,
    /// Bytes currently buffered.
    pub kept_bytes: usize,
    /// Bytes ever received.
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// Launch preparation
// ---------------------------------------------------------------------------

/// A launch with the executable resolved, secrets read and argv checked.
struct PreparedLaunch {
    program: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    /// Environment entries; values wrap [`SecretString`] so a resolved secret zeroes on
    /// drop even when the definition called it a literal.
    env: Vec<(String, SecretString)>,
    redactor: Redactor,
    source: PathSource,
}

impl fmt::Debug for PreparedLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedLaunch")
            .field("program", &self.program)
            .field("arg_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field(
                "env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Resolves the executable, resolves secrets and refuses a secret in argv.
///
/// Runs on a blocking thread: path discovery may start a login shell and the Keychain is
/// synchronous.
fn prepare(
    launch: &StdioLaunch,
    paths: &PathResolver,
    secrets: &dyn SecretStore,
) -> Result<PreparedLaunch, SessionError> {
    let location = paths
        .which(launch.command())
        .map_err(SessionError::from_resolve)?;

    let cwd = match launch.cwd() {
        Some(cwd) => {
            let cwd = absolute(cwd);
            if !cwd.is_dir() {
                return Err(SessionError::invalid_working_directory(&cwd));
            }
            Some(cwd)
        }
        None => None,
    };

    let mut redactor = Redactor::new();
    let mut env: Vec<(String, SecretString)> = Vec::with_capacity(launch.env().len());
    let mut secret_indices: Vec<usize> = Vec::new();
    for (name, value) in launch.env() {
        match value {
            EnvValue::Literal(text) => {
                env.push((name.clone(), SecretString::new(text.clone())));
            }
            EnvValue::Secret(secret) => {
                let id = KeychainId::new(secret.keychain_account.clone())
                    .map_err(|_| SessionError::missing_secret(name))?;
                let value = secrets
                    .read(&id)
                    .map_err(|_| SessionError::missing_secret(name))?;
                redactor.insert(SecretString::new(value.expose_secret()));
                secret_indices.push(env.len());
                env.push((name.clone(), value));
            }
        }
    }

    // A secret in argv is readable by every process on the machine (`ps`), so refuse
    // before the process exists. Only resolved secrets are checked: an ordinary literal
    // such as `info` is allowed to appear in `--log=info`. An empty secret is skipped,
    // because it would match every argument.
    let leaked = std::iter::once(launch.command())
        .chain(launch.args().iter().map(String::as_str))
        .any(|argument| {
            secret_indices.iter().any(|index| {
                let value = env[*index].1.expose_secret();
                !value.is_empty() && argument.contains(value)
            })
        });
    if leaked {
        return Err(SessionError::secret_in_arguments());
    }

    Ok(PreparedLaunch {
        program: location.path,
        args: launch.args().to_vec(),
        cwd,
        env,
        redactor,
        source: location.source,
    })
}

/// Makes `path` absolute without touching the file system.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// The long-lived half of the stdio client.
///
/// It holds the resolved path policy and the credential store; one instance serves every
/// server check. Timeouts default to [`TimeoutPolicy::for_command`]; tests override them
/// with [`Self::with_timeouts`].
#[derive(Clone)]
pub struct StdioClient {
    paths: Arc<PathResolver>,
    secrets: Arc<dyn SecretStore>,
    client: Implementation,
    timeouts: Option<TimeoutPolicy>,
}

impl StdioClient {
    /// A client that reports itself as `AgenTuna/<crate version>`.
    #[must_use]
    pub fn new(paths: Arc<PathResolver>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            paths,
            secrets,
            client: Implementation::new("AgenTuna", env!("CARGO_PKG_VERSION")),
            timeouts: None,
        }
    }

    /// Replaces the per-command timeout policy.
    ///
    /// Production callers leave this unset; tests use millisecond budgets. The override
    /// applies to every phase, including shutdown graces.
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: TimeoutPolicy) -> Self {
        self.timeouts = Some(timeouts);
        self
    }

    /// Replaces the reported client identity (`clientInfo`).
    #[must_use]
    pub fn with_client_identity(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        self.client = Implementation::new(name.into(), version.into());
        self
    }

    /// Spawns the server, completes `initialize` and returns a ready session.
    ///
    /// `consent` must have been minted for exactly `launch`; otherwise the call fails
    /// with [`SessionErrorKind::ConsentMismatch`] and no process is created. `cancel` is
    /// observed before the spawn, while secrets are being read, during the handshake and
    /// at every later await; on cancellation the child is killed and reaped before the
    /// error is returned.
    ///
    /// # Errors
    /// Every [`SessionErrorKind`] except [`SessionErrorKind::ToolListTimeout`] and
    /// [`SessionErrorKind::Closed`], which belong to later calls.
    pub async fn open(
        &self,
        launch: &StdioLaunch,
        consent: &LaunchConsent,
        cancel: &CancellationToken,
    ) -> Result<StdioSession, SessionError> {
        if !consent.matches(launch) {
            return Err(SessionError::consent_mismatch());
        }
        if cancel.is_cancelled() {
            return Err(SessionError::cancelled());
        }

        let policy = self
            .timeouts
            .clone()
            .unwrap_or_else(|| launch.timeout_policy());

        let prepared = {
            let paths = self.paths.clone();
            let secrets = self.secrets.clone();
            let launch = launch.clone();
            let task =
                tokio::task::spawn_blocking(move || prepare(&launch, &paths, secrets.as_ref()));
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(SessionError::cancelled()),
                joined = task => joined
                    .map_err(|_| SessionError::internal("Preparing the server launch was interrupted."))??,
            }
        };

        debug!(
            program = %prepared.program.display(),
            source = %prepared.source,
            cwd = ?prepared.cwd,
            "starting stdio MCP server"
        );

        let mut command = Command::new(&prepared.program);
        command
            .args(&prepared.args)
            .envs(
                prepared
                    .env
                    .iter()
                    .map(|(key, value)| (key, value.expose_secret())),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        if let Some(cwd) = &prepared.cwd {
            command.current_dir(cwd);
        }
        // Its own group, so `npx`'s node child receives the same signals. Never a shell:
        // the executable and arguments are separate argv entries.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err(match error.kind() {
                    io::ErrorKind::PermissionDenied => {
                        SessionError::permission_denied(&prepared.program)
                    }
                    io::ErrorKind::NotFound => {
                        SessionError::unresolved(&prepared.program.display().to_string(), None)
                    }
                    _ => SessionError::spawn_failed(&prepared.program, &error.to_string()),
                });
            }
        };
        let pid = child.id().unwrap_or_default();
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            abort_child(&mut child).await;
            return Err(SessionError::internal(
                "The spawned server did not expose stdin, stdout and stderr.",
            ));
        };

        debug!(pid, "stdio MCP server started");

        // `open` is cancel-safe: a caller may drop this future at any await below (the
        // scheduler's per-check deadline, app quit). Dropping `tokio::process::Child` alone
        // leaves the server running (`kill_on_drop(false)`), so an armed guard kills the
        // whole process group unless the session takes ownership of it.
        let mut spawn_guard = SpawnGuard::new(pid);

        let buffer = Arc::new(StderrBuffer::default());
        let drain = spawn_stderr_drain(stderr, buffer.clone());

        let transport = AsyncRwTransport::<RoleClient, _, _>::new_client(stdout, stdin);
        let config = ClientConfig::new(ClientCapabilities::default(), self.client.clone());
        let budget = policy.handshake_budget();
        // rmcp's `RunningService` holds a `DropGuard` on this token and cancels it when the
        // service ends. Hand it a child: cancellation still flows parent -> child, but a
        // finished check must never cancel the caller's app-lifetime token.
        let handshake = serve_client_with_ct(config, transport, cancel.child_token());

        enum Outcome {
            Ready(RunningService<RoleClient, ClientConfig>),
            Failed(SessionError),
            Exited(Option<ExitSummary>),
        }

        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => Outcome::Failed(SessionError::cancelled()),
            result = tokio::time::timeout(budget, handshake) => match result {
                Ok(Ok(running)) => {
                    let version = running
                        .peer_info()
                        .map(|info| info.protocol_version.clone());
                    match version {
                        Some(version) if ProtocolVersion::KNOWN_VERSIONS.contains(&version) => {
                            Outcome::Ready(running)
                        }
                        Some(version) => Outcome::Failed(SessionError::protocol_version_mismatch(
                            &format!("The server selected unsupported version `{version}`."),
                        )),
                        None => Outcome::Failed(SessionError::protocol_version_mismatch(
                            "The server did not report a protocol version.",
                        )),
                    }
                }
                Ok(Err(error)) => Outcome::Failed(map_initialize_error(&error)),
                Err(_elapsed) => Outcome::Failed(SessionError::startup_timeout(budget)),
            },
            status = child.wait() => Outcome::Exited(status.ok().map(ExitSummary::from_status)),
        };

        match outcome {
            Outcome::Ready(running) => {
                spawn_guard.disarm();
                Ok(StdioSession {
                    running: Some(running),
                    child: Some(child),
                    pid,
                    stderr: buffer,
                    redactor: prepared.redactor,
                    policy,
                    drain: Some(drain),
                })
            }
            Outcome::Exited(exit) => {
                spawn_guard.disarm();
                abort_child(&mut child).await;
                await_drain(&mut Some(drain), policy.interrupt_grace).await;
                let error = SessionError::exited(exit);
                Err(error.with_stderr(excerpt(buffer.snapshot().text.as_str(), &prepared.redactor)))
            }
            Outcome::Failed(error) => {
                spawn_guard.disarm();
                // A protocol error that raced the process exit is really "it died":
                // the exit detail and its stderr are more useful than the transport
                // complaint.
                let early_exit = child.try_wait().ok().flatten();
                abort_child(&mut child).await;
                await_drain(&mut Some(drain), policy.interrupt_grace).await;
                let error = match early_exit {
                    Some(status) if error.kind() == SessionErrorKind::ProtocolError => {
                        SessionError::exited(Some(ExitSummary::from_status(status)))
                    }
                    _ => error,
                };
                Err(error.with_stderr(excerpt(buffer.snapshot().text.as_str(), &prepared.redactor)))
            }
        }
    }
}

impl fmt::Debug for StdioClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioClient")
            .field("client", &self.client.name)
            .field("version", &self.client.version)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

/// Kills a spawned server's process group if [`StdioClient::open`] is dropped before the
/// session owns the child.
struct SpawnGuard {
    pgid: Option<u32>,
}

impl SpawnGuard {
    fn new(pgid: u32) -> Self {
        Self { pgid: Some(pgid) }
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            let _ = signal_group(pgid, GroupSignal::Kill);
        }
    }
}

/// Maps rmcp's handshake failure onto the session taxonomy.
fn map_initialize_error(error: &rmcp::service::ClientInitializeError) -> SessionError {
    use rmcp::service::ClientInitializeError;
    match error {
        ClientInitializeError::Cancelled => SessionError::cancelled(),
        ClientInitializeError::NoCompatibleProtocolVersion { .. } => {
            SessionError::protocol_version_mismatch(&error.to_string())
        }
        ClientInitializeError::ConnectionClosed(_) => {
            SessionError::protocol_error("The server closed the connection during initialize.")
        }
        other => SessionError::protocol_error(&other.to_string()),
    }
}

/// A ready stdio session.
pub struct StdioSession {
    running: Option<RunningService<RoleClient, ClientConfig>>,
    child: Option<Child>,
    pid: u32,
    stderr: Arc<StderrBuffer>,
    redactor: Redactor,
    policy: TimeoutPolicy,
    drain: Option<JoinHandle<()>>,
}

impl StdioSession {
    /// The child's process id; also its process group id on Unix.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.pid
    }

    /// The process group the harness signals.
    #[must_use]
    pub fn process_group_id(&self) -> u32 {
        self.pid
    }

    /// The server's identity and negotiated protocol version.
    #[must_use]
    pub fn server_identity(&self) -> Option<ServerIdentity> {
        self.running
            .as_ref()
            .and_then(|running| running.peer_info())
            .map(|info| ServerIdentity::from_peer(&info))
    }

    /// The stderr ring as it stands now.
    #[must_use]
    pub fn stderr_snapshot(&self) -> StderrSnapshot {
        self.stderr.snapshot()
    }

    /// One redacted excerpt of the buffered stderr.
    #[must_use]
    pub fn stderr_excerpt(&self) -> Option<String> {
        excerpt(self.stderr.snapshot().text.as_str(), &self.redactor)
    }

    /// Lists every tool, following cursors, with one deadline per page.
    ///
    /// Stops after [`TimeoutPolicy::tool_limit`] tools and reports `truncated`. `cancel`
    /// is observed before and during each page.
    ///
    /// # Errors
    /// [`SessionErrorKind::Cancelled`], [`SessionErrorKind::ToolListTimeout`],
    /// [`SessionErrorKind::ProtocolError`] or [`SessionErrorKind::Closed`].
    pub async fn list_tools(
        &self,
        cancel: &CancellationToken,
    ) -> Result<ToolListing, SessionError> {
        let peer = match &self.running {
            Some(running) => running.peer().clone(),
            None => return Err(SessionError::closed()),
        };
        let mut tools: Vec<ToolSummary> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut truncated = false;

        loop {
            if cancel.is_cancelled() {
                return Err(SessionError::cancelled());
            }
            let params = Some(PaginatedRequestParams::default().with_cursor(cursor.clone()));
            let page = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(SessionError::cancelled()),
                result = tokio::time::timeout(self.policy.tool_page, peer.list_tools(params)) => result,
            };
            let page = match page {
                Ok(Ok(page)) => page,
                Ok(Err(error)) => return Err(self.map_service_error(error)),
                Err(_elapsed) => {
                    return Err(SessionError::tool_list_timeout(self.policy.tool_page));
                }
            };

            tools.extend(page.tools.iter().map(ToolSummary::from_tool));
            let limit = self.policy.tool_limit;
            if tools.len() >= limit {
                if tools.len() > limit || page.next_cursor.is_some() {
                    truncated = true;
                }
                tools.truncate(limit);
                break;
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        Ok(ToolListing { tools, truncated })
    }

    /// Stops the server and reaps it.
    ///
    /// Closes the transport, then escalates `SIGINT` → [`TimeoutPolicy::interrupt_grace`]
    /// → `SIGTERM` → [`TimeoutPolicy::terminate_grace`] → `SIGKILL`, always against the
    /// whole process group, and finally clears processes the leader left behind. A
    /// cancelled `cancel` skips the remaining grace periods and goes straight to the next
    /// signal: shutdown always finishes, because abandoning it would leak the child.
    pub async fn shutdown(&mut self, cancel: &CancellationToken) -> ShutdownReport {
        let mut report = ShutdownReport::default();

        if let Some(running) = self.running.take() {
            report.steps.push(ShutdownStep::CloseTransport);
            // The MCP spec asks the server to exit when stdin closes. Do it in the
            // background: a server that ignores the close must not delay the signals,
            // and dropping the service still closes the pipes.
            tokio::spawn(async move {
                let _ = running.cancel().await;
            });
        }

        let Some(mut child) = self.child.take() else {
            return report;
        };

        match child.try_wait() {
            Ok(Some(status)) => {
                report.exit = Some(ExitSummary::from_status(status));
                self.clear_group(&mut report).await;
                return report;
            }
            Ok(None) => {}
            Err(error) => {
                warn!(error = %error, "could not inspect the server process");
            }
        }

        for (signal, step, grace) in [
            (
                GroupSignal::Interrupt,
                ShutdownStep::Interrupt,
                self.policy.interrupt_grace,
            ),
            (
                GroupSignal::Terminate,
                ShutdownStep::Terminate,
                self.policy.terminate_grace,
            ),
        ] {
            if signal_group(self.process_group_id(), signal).is_ok() {
                report.steps.push(step);
            }
            if let Some(status) = wait_for_exit(&mut child, grace, cancel).await {
                report.exit = Some(ExitSummary::from_status(status));
                break;
            }
        }

        if report.exit.is_none() {
            if signal_group(self.process_group_id(), GroupSignal::Kill).is_ok() {
                report.steps.push(ShutdownStep::Kill);
            }
            report.exit =
                match tokio::time::timeout(self.policy.terminate_grace, child.wait()).await {
                    Ok(Ok(status)) => Some(ExitSummary::from_status(status)),
                    _ => None,
                };
        } else {
            // The leader is reaped; make sure it did not leave the node child behind.
            let _ = child.wait().await;
        }

        self.clear_group(&mut report).await;
        await_drain(&mut self.drain, self.policy.terminate_grace).await;
        report
    }

    /// Kills anything left in the process group and records the outcome.
    async fn clear_group(&self, report: &mut ShutdownReport) {
        let pgid = self.process_group_id();
        if !group_alive(pgid) {
            report.group_cleared = true;
            return;
        }
        if signal_group(pgid, GroupSignal::Kill).is_ok() {
            report.steps.push(ShutdownStep::GroupCleanup);
        }
        report.group_cleared = wait_for_group_exit(pgid, GROUP_SETTLE).await;
        if !report.group_cleared {
            warn!(pgid, "process group still has members after SIGKILL");
        }
    }

    fn map_service_error(&self, error: ServiceError) -> SessionError {
        match error {
            ServiceError::Cancelled { .. } => SessionError::cancelled(),
            ServiceError::Timeout { timeout } => SessionError::tool_list_timeout(timeout),
            other => {
                SessionError::protocol_error(&other.to_string()).with_stderr(self.stderr_excerpt())
            }
        }
    }
}

impl fmt::Debug for StdioSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioSession")
            .field("pid", &self.pid)
            .field("open", &self.child.is_some())
            .field("stderr", &self.stderr_snapshot())
            .finish_non_exhaustive()
    }
}

/// Best effort: a dropped session kills its group so a forgotten handle cannot leave a
/// server running. A panic in its own task must not escape here.
impl Drop for StdioSession {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pgid = self.process_group_id();
        if signal_group(pgid, GroupSignal::Kill).is_err() {
            warn!(pgid, "could not signal the server process group on drop");
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
            Err(_) => {
                // No runtime: the group is dead, the OS reaps the rest.
                let _ = child.start_kill();
            }
        }
        if let Some(drain) = self.drain.take() {
            drain.abort();
        }
    }
}

/// What the server reported during `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    /// `serverInfo.name`, when the server sent one.
    pub name: Option<String>,
    /// `serverInfo.version`, when the server sent one.
    pub version: Option<String>,
    /// The negotiated MCP protocol version.
    pub protocol_version: String,
    /// Optional usage instructions.
    pub instructions: Option<String>,
}
impl ServerIdentity {
    pub(crate) fn from_peer(info: &ServerPeerInfo) -> Self {
        let server = info.server_info.as_ref();
        Self {
            name: server.map(|server| server.name.clone()),
            version: server.map(|server| server.version.clone()),
            protocol_version: info.protocol_version.to_string(),
            instructions: info.instructions.clone(),
        }
    }
}

/// One tool, reduced to what the UI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSummary {
    /// Tool name.
    pub name: String,
    /// Human-readable title, when the server provides one.
    pub title: Option<String>,
    /// Description, when the server provides one.
    pub description: Option<String>,
}

impl ToolSummary {
    fn from_tool(tool: &Tool) -> Self {
        Self {
            name: tool.name.to_string(),
            title: tool.title.clone(),
            description: tool.description.as_ref().map(ToString::to_string),
        }
    }
}

/// The tools one session advertised.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolListing {
    /// Tools in server order.
    pub tools: Vec<ToolSummary>,
    /// True when the page ceiling hid tools that exist.
    pub truncated: bool,
}

impl ToolListing {
    /// How many tools were collected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the server advertised none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// One step the shutdown sequence took, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShutdownStep {
    /// The transport was closed so the server could exit on EOF.
    CloseTransport,
    /// `SIGINT` was sent to the process group.
    Interrupt,
    /// `SIGTERM` was sent to the process group.
    Terminate,
    /// `SIGKILL` was sent to the process group.
    Kill,
    /// The leader had already exited; leftover group members were killed.
    GroupCleanup,
}

/// How a process exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitSummary {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Terminating signal, on Unix.
    pub signal: Option<i32>,
}

impl ExitSummary {
    fn from_status(status: ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            Self {
                code: status.code(),
                signal: status.signal(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                code: status.code(),
                signal: None,
            }
        }
    }
}

impl fmt::Display for ExitSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(code), _) => write!(f, "exit code {code}"),
            (None, Some(signal)) => write!(f, "signal {signal}"),
            (None, None) => f.write_str("unknown exit status"),
        }
    }
}

/// The result of [`StdioSession::shutdown`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Steps in the order they ran.
    pub steps: Vec<ShutdownStep>,
    /// How the leader exited, when it was observed.
    pub exit: Option<ExitSummary>,
    /// True when no process in the group survives.
    pub group_cleared: bool,
}

impl ShutdownReport {
    /// Whether `step` ran.
    #[must_use]
    pub fn ran(&self, step: ShutdownStep) -> bool {
        self.steps.contains(&step)
    }
}

// ---------------------------------------------------------------------------
// Process supervision
// ---------------------------------------------------------------------------

/// The escalation ladder's signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupSignal {
    Interrupt,
    Terminate,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: GroupSignal) -> io::Result<()> {
    use rustix::process::{Pid, Signal, kill_process_group};
    let raw = i32::try_from(pgid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let signal = match signal {
        GroupSignal::Interrupt => Signal::INT,
        GroupSignal::Terminate => Signal::TERM,
        GroupSignal::Kill => Signal::KILL,
    };
    Ok(kill_process_group(raw, signal)?)
}

#[cfg(unix)]
fn group_alive(pgid: u32) -> bool {
    use rustix::process::{Pid, test_kill_process_group};
    let Some(raw) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match test_kill_process_group(raw) {
        Ok(()) => true,
        Err(error) => error != rustix::io::Errno::SRCH,
    }
}

/// Windows has no POSIX signals and no process groups in the POSIX sense: the only
/// portable way to stop a tree is `taskkill /T /F`, which is a forced kill. The
/// escalation ladder therefore collapses to one step there.
#[cfg(windows)]
fn signal_group(pgid: u32, _signal: GroupSignal) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pgid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("taskkill reported failure"))
    }
}

#[cfg(windows)]
fn group_alive(pgid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pgid}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pgid.to_string()),
        Err(_) => false,
    }
}

/// Waits up to `grace` for the child to exit; cancellation skips the wait.
///
/// Cancel-safe: the status is returned only when the child really exited, so a `None`
/// leaves the child usable for the next escalation step.
async fn wait_for_exit(
    child: &mut Child,
    grace: Duration,
    cancel: &CancellationToken,
) -> Option<ExitStatus> {
    if grace.is_zero() {
        return child.try_wait().ok().flatten();
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        result = tokio::time::timeout(grace, child.wait()) => match result {
            Ok(Ok(status)) => Some(status),
            _ => None,
        },
    }
}

/// Waits until no process in `pgid` survives, bounded.
async fn wait_for_group_exit(pgid: u32, budget: Duration) -> bool {
    let task = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now().checked_add(budget);
        loop {
            if !group_alive(pgid) {
                return true;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    task.await.unwrap_or(false)
}

/// Kills and reaps a child whose session never became usable.
async fn abort_child(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = signal_group(pid, GroupSignal::Kill);
    }
    let _ = tokio::time::timeout(GROUP_SETTLE, child.wait()).await;
}

/// Starts the stderr reader; it stops at EOF, which the kill guarantees.
fn spawn_stderr_drain(
    mut stderr: tokio::process::ChildStderr,
    buffer: Arc<StderrBuffer>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = vec![0_u8; 4096];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => buffer.push(&chunk[..read]),
            }
        }
    })
}

/// Waits for the drain task, bounded, so the last bytes are in the buffer.
///
/// The task ends at EOF on stderr; a grandchild that inherited the pipe can keep it open,
/// so this never waits longer than `budget`.
async fn await_drain(drain: &mut Option<JoinHandle<()>>, budget: Duration) {
    if let Some(task) = drain.take() {
        let _ = tokio::time::timeout(budget, task).await;
    }
}

/// The last `limit` characters, redacted.
fn excerpt(text: &str, redactor: &Redactor) -> Option<String> {
    let trimmed = text.trim_end();
    if trimmed.trim().is_empty() {
        return None;
    }
    Some(redactor.redact(tail_chars(trimmed, STDERR_EXCERPT_CHARS)))
}

/// The final `max_chars` characters of `text`.
fn tail_chars(text: &str, max_chars: usize) -> &str {
    let mut seen = 0;
    for (index, _) in text.char_indices().rev() {
        seen += 1;
        if seen == max_chars {
            return &text[index..];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_secrets::MemoryStore;

    fn launch_with_secret() -> (StdioLaunch, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::new());
        let id = KeychainId::new("test-account").expect("keychain id");
        store
            .create(&id, SecretString::new("sk-live-sentinel-0123456789"))
            .expect("store secret");
        let mut env = BTreeMap::new();
        env.insert(
            "GITHUB_TOKEN".to_owned(),
            EnvValue::Secret(at_model::SecretRef {
                env_name: "GITHUB_TOKEN".to_owned(),
                keychain_account: "test-account".to_owned(),
            }),
        );
        env.insert(
            "LOG_LEVEL".to_owned(),
            EnvValue::Literal("debug".to_owned()),
        );
        let launch = StdioLaunch::new("/bin/sh").with_env(env);
        (launch, store)
    }

    #[test]
    fn package_runners_get_the_download_budget() {
        for runner in PACKAGE_RUNNER_COMMANDS {
            let policy = TimeoutPolicy::for_command(runner);
            assert_eq!(policy.startup, PACKAGE_RUNNER_STARTUP, "{runner}");
            assert_eq!(
                policy.handshake_budget(),
                PACKAGE_RUNNER_STARTUP,
                "{runner}"
            );
        }
        assert_eq!(TimeoutPolicy::for_command("node").startup, DEFAULT_STARTUP);
        assert_eq!(
            TimeoutPolicy::for_command("/opt/homebrew/bin/npx").startup,
            PACKAGE_RUNNER_STARTUP
        );
        assert_eq!(
            TimeoutPolicy::for_command("/usr/local/bin/npx").startup,
            PACKAGE_RUNNER_STARTUP
        );
        assert_eq!(
            TimeoutPolicy::for_command("pnpm.cmd").startup,
            PACKAGE_RUNNER_STARTUP
        );
        assert!(!is_package_runner("npm"));
        assert!(!is_package_runner("/tmp/npx-helper"));
    }

    #[test]
    fn handshake_budget_never_shortens_the_initialize_timeout() {
        let policy = TimeoutPolicy {
            startup: Duration::from_secs(2),
            initialize: Duration::from_secs(20),
            ..TimeoutPolicy::default()
        };
        assert_eq!(policy.handshake_budget(), Duration::from_secs(20));
    }

    #[test]
    fn stderr_ring_keeps_the_newest_bytes() {
        let buffer = StderrBuffer::new(8);
        buffer.push(b"0123456789");
        assert_eq!(buffer.len(), 8);
        assert_eq!(buffer.text(), "23456789");
        buffer.push(b"ab");
        assert_eq!(buffer.text(), "456789ab");
        assert_eq!(buffer.total_bytes(), 12);
        assert!(!buffer.is_empty());

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.kept_bytes, 8);
        assert_eq!(snapshot.total_bytes, 12);
    }

    #[test]
    fn a_megabyte_to_stderr_keeps_the_tail_and_the_count() {
        let buffer = StderrBuffer::new(STDERR_RING_BYTES);
        let chunk = vec![b'x'; 4096];
        for _ in 0..256 {
            buffer.push(&chunk);
        }
        buffer.push(b"the-end");
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.kept_bytes, STDERR_RING_BYTES);
        assert!(snapshot.total_bytes > 1024 * 1024);
        assert!(snapshot.text.ends_with("the-end"));
    }

    #[test]
    fn launch_debug_never_prints_arguments_or_environment_values() {
        let launch = StdioLaunch::new("npx")
            .with_args(["-y", "server@1.2.3", "--token", "plain-argument-token"])
            .with_env(BTreeMap::from([(
                "API_KEY".to_owned(),
                EnvValue::Literal("literal-secret".to_owned()),
            )]));
        let debug = format!("{launch:?}");
        assert!(debug.contains("npx"));
        assert!(debug.contains("API_KEY"));
        assert!(!debug.contains("plain-argument-token"));
        assert!(!debug.contains("literal-secret"));
        assert!(!debug.contains("server@1.2.3"));
    }

    #[test]
    fn consent_is_bound_to_the_exact_launch() {
        let launch = StdioLaunch::new("npx").with_args(["-y", "server"]);
        let consent = LaunchConsent::user_approved(&launch);
        assert!(consent.matches(&launch));
        assert!(!consent.matches(&launch.clone().with_args(["-y", "other"])));
        assert!(!consent.matches(&launch.clone().with_cwd("/tmp")));
    }

    #[test]
    fn http_transports_have_no_stdio_launch() {
        let transport = McpTransport::Http {
            url: "https://example.invalid/mcp".parse().expect("url"),
            headers: BTreeMap::new(),
            oauth: None,
        };
        let error = StdioLaunch::from_transport(&transport).expect_err("http is not stdio");
        assert_eq!(error.kind(), SessionErrorKind::NotStdio);
    }

    #[test]
    fn a_resolved_secret_in_argv_is_rejected_without_naming_it() {
        let (launch, store) = launch_with_secret();
        let launch = launch.with_args(["--token=sk-live-sentinel-0123456789"]);
        let paths = PathResolver::new();
        let error = prepare(&launch, &paths, store.as_ref()).expect_err("secret in argv");
        assert_eq!(error.kind(), SessionErrorKind::SecretInArguments);
        assert!(!error.to_string().contains("sk-live-sentinel"));
    }

    #[test]
    fn prepare_resolves_secrets_into_environment_entries_only() {
        let (launch, store) = launch_with_secret();
        let paths = PathResolver::new();
        let prepared = prepare(&launch, &paths, store.as_ref()).expect("prepare");
        assert_eq!(prepared.program, PathBuf::from("/bin/sh"));
        assert_eq!(prepared.source, PathSource::Explicit);
        let map: BTreeMap<_, _> = prepared
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.expose_secret().to_owned()))
            .collect();
        assert_eq!(
            map.get("GITHUB_TOKEN").map(String::as_str),
            Some("sk-live-sentinel-0123456789")
        );
        assert_eq!(map.get("LOG_LEVEL").map(String::as_str), Some("debug"));
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("sk-live-sentinel"));
        assert!(!debug.contains("debug\""));
    }

    #[test]
    fn an_ordinary_literal_in_argv_is_not_mistaken_for_a_secret() {
        let mut env = BTreeMap::new();
        env.insert("LOG_LEVEL".to_owned(), EnvValue::Literal("info".to_owned()));
        let launch = StdioLaunch::new("/bin/sh")
            .with_args(["--log=info"])
            .with_env(env);
        let prepared = prepare(&launch, &PathResolver::new(), &MemoryStore::new())
            .expect("a literal is not a secret");
        assert_eq!(prepared.env.len(), 1);
    }

    #[test]
    fn a_missing_keychain_entry_names_the_variable_only() {
        let empty = MemoryStore::new();
        let mut env = BTreeMap::new();
        env.insert(
            "API_TOKEN".to_owned(),
            EnvValue::Secret(at_model::SecretRef {
                env_name: "API_TOKEN".to_owned(),
                keychain_account: "absent".to_owned(),
            }),
        );
        let launch = StdioLaunch::new("/bin/sh").with_env(env);
        let error = prepare(&launch, &PathResolver::new(), &empty).expect_err("missing");
        assert_eq!(error.kind(), SessionErrorKind::MissingSecret);
        assert!(error.to_string().contains("API_TOKEN"));
    }

    #[test]
    fn a_missing_working_directory_is_reported_before_spawning() {
        let launch = StdioLaunch::new("/bin/sh").with_cwd("/definitely/not/a/directory");
        let error = prepare(&launch, &PathResolver::new(), &MemoryStore::new()).expect_err("cwd");
        assert_eq!(error.kind(), SessionErrorKind::InvalidWorkingDirectory);
    }

    #[test]
    fn tail_chars_keeps_whole_characters() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("abc", 10), "abc");
        assert_eq!(tail_chars("héllo", 2), "lo");
        assert_eq!(tail_chars("", 3), "");
    }

    #[test]
    fn stderr_excerpt_is_redacted_and_bounded() {
        let mut redactor = Redactor::new();
        redactor.insert(SecretString::new("sk-live-sentinel-0123456789"));
        let text = format!("{} token=sk-live-sentinel-0123456789", "noise ".repeat(200));
        let excerpt = excerpt(&text, &redactor).expect("excerpt");
        assert!(!excerpt.contains("sk-live-sentinel"));
        assert!(excerpt.contains(at_model::REDACTION_MASK));
        assert!(excerpt.chars().count() <= STDERR_EXCERPT_CHARS);
    }

    #[test]
    fn an_empty_stderr_yields_no_excerpt() {
        assert_eq!(excerpt("   \n", &Redactor::new()), None);
    }
}
