//! MCP server client sessions, health checks, and tool listing.

pub mod path;
pub mod policy;
pub mod session;

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
