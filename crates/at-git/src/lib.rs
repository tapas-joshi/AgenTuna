//! Git CLI integration, source management, and remote backup repos.

pub mod policy;

pub use policy::{
    AuthorizedGitCall, GIT_NETWORK_TIMEOUT, GitError, GitOutput, GitReport, GitRunner,
    MAX_GIT_OUTPUT_BYTES, NetworkGitCall, ProcessGitRunner, run_network_git,
};
