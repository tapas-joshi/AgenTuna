//! Git CLI integration, source management, and remote backup repos.
//!
//! Two halves with a hard boundary between them: [`policy`] is the only path that may reach the
//! network, and [`local`] runs read-mostly commands (`rev-parse`, `status`, `check-ignore`,
//! `add`) that stay on the local machine. Both spawn git with argv arrays only, never a shell.

pub mod import;
pub mod local;
pub mod marketplace;
pub mod policy;
pub mod remote;
pub mod source_check;

pub use local::{
    IgnoreRule, LOCAL_GIT_TIMEOUT, LocalGitCall, LocalGitError, LocalGitRunner,
    MAX_DIAGNOSTIC_CHARS, ProcessLocalGitRunner, Repository, StatusEntry, add_paths,
    detect_repository, explain_ignore, ignored_paths, status_entries,
};
pub use policy::{
    AuthorizedGitCall, GIT_NETWORK_TIMEOUT, GitError, GitOutput, GitReport, GitRunner,
    MAX_GIT_OUTPUT_BYTES, NetworkGitCall, ProcessGitRunner, run_network_git,
};
pub use remote::{
    GitAuth, MAX_REMOTE_DIAGNOSTIC_CHARS, ProcessRemoteGitRunner, REMOTE_GIT_TIMEOUT,
    RemoteContext, RemoteError, RemoteGitCall, RemoteGitRunner, RemoteHead, commit_local,
    current_branch, has_changes, init_repo, looks_like_auth_failure, looks_like_non_fast_forward,
    looks_like_offline, remote_diagnostic, remote_git, rev_parse_head, run_remote_git,
    set_remote_url, stage_all, validate_repo_url,
};
