//! MCP server client sessions, health checks, and tool listing.

pub mod http;
pub mod oauth;
pub mod path;
pub mod policy;
pub mod session;
pub mod test_connection;
pub mod tls;
pub use test_connection::{McpError, McpErrorKind, TestResult, test_connection};
pub use tls::ensure_crypto_provider;

pub use http::{
    HTTP_INITIALIZE_TIMEOUT, HTTP_TOOL_LIMIT, HTTP_TOOL_PAGE_TIMEOUT, HttpClient, HttpError,
    HttpErrorKind, HttpSession,
};
pub use oauth::{
    BrowserOpener, CallbackRequest, KeychainCredentialStore, LoopbackListener,
    OAUTH_KEYCHAIN_SERVICE, OAuthClient, PolicyOAuthHttpClient, REFRESH_SKEW, SIGN_IN_TIMEOUT,
    SignInError, SignInErrorKind, SignInOutcome, SystemBrowser,
};
pub use path::{
    Clock, CommandLocation, ManagerHint, ManagerRoots, ManualClock, PathPlatform, PathResolver,
    PathResolverBuilder, PathSource, PlatformKind, PosixPlatform, ResolveError, ResolvedPath,
    SearchDir, SystemClock, VersionManager, WELL_KNOWN_DIRS, WindowsPlatform, default_search_dirs,
};
pub use policy::{
    AuthorizedHttpConnect, ConnectIntent, MCP_TCP_PROBE_TIMEOUT, McpAccess, McpConnectError,
    SystemTcpConnector, TcpConnector, authorize_connect, probe_endpoint,
};
pub use session::{
    DEFAULT_STARTUP, ExitSummary, INITIALIZE_TIMEOUT, INTERRUPT_GRACE, LaunchConsent,
    PACKAGE_RUNNER_COMMANDS, PACKAGE_RUNNER_STARTUP, STDERR_RING_BYTES, ServerIdentity,
    SessionError, SessionErrorKind, ShutdownReport, ShutdownStep, StderrBuffer, StderrSnapshot,
    StdioClient, StdioLaunch, StdioSession, TERMINATE_GRACE, TOOL_LIMIT, TOOL_PAGE_TIMEOUT,
    TimeoutPolicy, ToolListing, ToolSummary, is_package_runner,
};
