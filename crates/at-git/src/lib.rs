//! Git CLI integration, source management, and remote backup repos.
//!
//! Two halves with a hard boundary between them: [`policy`] is the only path that may reach the
//! network, and [`local`] runs read-mostly commands (`rev-parse`, `status`, `check-ignore`,
//! `add`) that stay on the local machine. Both spawn git with argv arrays only, never a shell.

pub mod local;
pub mod policy;

pub use local::{
    IgnoreRule, LOCAL_GIT_TIMEOUT, LocalGitCall, LocalGitError, LocalGitRunner,
    MAX_DIAGNOSTIC_CHARS, ProcessLocalGitRunner, Repository, StatusEntry, add_paths,
    detect_repository, explain_ignore, ignored_paths, status_entries,
};
pub use policy::{
    AuthorizedGitCall, GIT_NETWORK_TIMEOUT, GitError, GitOutput, GitReport, GitRunner,
    MAX_GIT_OUTPUT_BYTES, NetworkGitCall, ProcessGitRunner, run_network_git,
};
