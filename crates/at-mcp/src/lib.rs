//! MCP server client sessions, health checks, and tool listing.

pub mod path;

pub use path::{
    Clock, CommandLocation, ManagerHint, ManagerRoots, ManualClock, PathPlatform, PathResolver,
    PathResolverBuilder, PathSource, PlatformKind, PosixPlatform, ResolveError, ResolvedPath,
    SearchDir, SystemClock, VersionManager, WELL_KNOWN_DIRS, WindowsPlatform, default_search_dirs,
};
