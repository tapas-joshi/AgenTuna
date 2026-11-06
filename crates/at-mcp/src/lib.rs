//! MCP server client sessions, health checks, and tool listing.

pub mod path;
pub mod policy;

pub use path::{
    Clock, CommandLocation, ManagerHint, ManagerRoots, ManualClock, PathPlatform, PathResolver,
    PathResolverBuilder, PathSource, PlatformKind, PosixPlatform, ResolveError, ResolvedPath,
    SearchDir, SystemClock, VersionManager, WELL_KNOWN_DIRS, WindowsPlatform, default_search_dirs,
};
pub use policy::{
    AuthorizedHttpConnect, ConnectIntent, MCP_TCP_PROBE_TIMEOUT, McpAccess, McpConnectError,
    SystemTcpConnector, TcpConnector, authorize_connect, probe_endpoint,
};
