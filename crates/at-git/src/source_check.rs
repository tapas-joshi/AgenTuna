//! Policy-gated checks for updates to imported Git skills.

use std::path::Path;

use at_model::network::{
    DenialReason, NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger,
};
use time::OffsetDateTime;

use crate::remote::{
    ProcessRemoteGitRunner, RemoteError, RemoteGitCall, RemoteGitRunner, run_remote_git,
};

/// Result of checking a source's pinned/default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceCheck {
    /// The remote ref exists and resolves to a commit.
    Found {
        /// Resolved commit SHA.
        remote_sha: String,
    },
    /// The remote ref no longer exists.
    SourceGone,
    /// The check could not determine remote state; stored badge must be preserved.
    Unknown,
}

/// Failure while checking a Git source.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceCheckError {
    /// The source URL/ref is invalid.
    #[error("invalid git source reference")]
    InvalidSource,
    /// Policy blocked the request before git ran.
    #[error(transparent)]
    Policy(#[from] RemoteError),
    /// Git returned malformed ls-remote data.
    #[error("git returned an invalid remote reference")]
    InvalidOutput,
}

/// Resolves a remote's default branch or the explicitly stored branch without fetching objects.
///
/// `git ls-remote` only reads remote refs; no store or clone is modified. Explicit refs are
/// validated as ref names by git, and are passed as a separate argv value.
///
/// # Errors
/// Returns policy errors if the call is denied or git cannot complete, and `InvalidOutput` for
/// malformed successful output.
pub fn check_source(
    url: &str,
    git_ref: Option<&str>,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
    runner: &dyn RemoteGitRunner,
) -> Result<SourceCheck, SourceCheckError> {
    if url.is_empty()
        || url.contains(['\0', '\n', '\r'])
        || git_ref
            .is_some_and(|r| r.is_empty() || r.starts_with('-') || r.contains(['\0', '\n', '\r']))
    {
        return Err(SourceCheckError::InvalidSource);
    }
    let args = if let Some(reference) = git_ref {
        vec!["ls-remote", "--exit-code", "--heads", "--", url, reference]
    } else {
        vec!["ls-remote", "--symref", "--", url, "HEAD"]
    };
    let cwd = Path::new(".");
    let call = RemoteGitCall {
        action: NetworkAction::GitFetch,
        trigger: NetworkTrigger::User,
        remote: url,
        args: &args,
        cwd,
        stdin: None,
        env: &[],
        at: OffsetDateTime::now_utc(),
    };
    let output = match run_remote_git(policy, log, &call, runner) {
        Ok(output) => output,
        Err(RemoteError::Denied(denied)) if denied.reason == DenialReason::Offline => {
            return Ok(SourceCheck::Unknown);
        }
        Err(error @ RemoteError::Denied(_)) => return Err(SourceCheckError::Policy(error)),
        Err(RemoteError::Offline) => return Ok(SourceCheck::Unknown),
        Err(error) => return Err(SourceCheckError::Policy(error)),
    };
    if !output.success {
        if crate::remote::looks_like_offline(&output.stderr) {
            return Ok(SourceCheck::Unknown);
        }
        if git_ref.is_some() && output.code == Some(2) && output.stdout.is_empty() {
            return Ok(SourceCheck::SourceGone);
        }
        if output.stdout.is_empty() {
            return Ok(SourceCheck::SourceGone);
        }
        return Err(SourceCheckError::InvalidOutput);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| SourceCheckError::InvalidOutput)?;
    let mut remote_sha = None;
    for line in text.lines() {
        let Some((left, right)) = line.split_once('\t') else {
            if line.starts_with("ref:") {
                continue;
            }
            return Err(SourceCheckError::InvalidOutput);
        };
        if !valid_sha(left) {
            return Err(SourceCheckError::InvalidOutput);
        }
        if right == "HEAD"
            || git_ref.is_some_and(|reference| right == format!("refs/heads/{reference}"))
        {
            remote_sha = Some(left.to_ascii_lowercase());
            break;
        }
    }
    Ok(
        remote_sha.map_or(SourceCheck::SourceGone, |remote_sha| SourceCheck::Found {
            remote_sha,
        }),
    )
}

/// Checks using the production process runner.
///
/// # Errors
/// See [`check_source`].
pub fn check_source_with_process(
    url: &str,
    git_ref: Option<&str>,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
) -> Result<SourceCheck, SourceCheckError> {
    check_source(
        url,
        git_ref,
        policy,
        log,
        &ProcessRemoteGitRunner::default(),
    )
}

fn valid_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(directory)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("run fixture git");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }

    fn git_output(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(args)
            .output()
            .expect("read fixture commit");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("fixture sha utf8")
            .trim()
            .to_owned()
    }

    #[test]
    fn detects_advanced_and_deleted_branch_in_local_bare_remote() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let bare = directory.path().join("remote.git");
        let work = directory.path().join("work");
        std::fs::create_dir(&work).expect("create work tree");
        git(
            directory.path(),
            &["init", "--bare", bare.to_str().expect("bare path")],
        );
        git(&work, &["init", "-b", "main"]);
        git(&work, &["config", "user.email", "fixture@example.invalid"]);
        git(&work, &["config", "user.name", "Fixture"]);
        std::fs::write(work.join("SKILL.md"), "one").expect("write first revision");
        git(&work, &["add", "SKILL.md"]);
        git(&work, &["commit", "-m", "first"]);
        git(
            &work,
            &["remote", "add", "origin", bare.to_str().expect("bare path")],
        );
        git(&work, &["push", "origin", "main"]);
        let first = git_output(&work, &["rev-parse", "HEAD"]);
        let url = format!("file://{}", bare.display());
        let policy = NetworkPolicy::default();
        let mut log = NetworkEventLog::new();
        let runner = ProcessRemoteGitRunner::default();
        assert_eq!(
            check_source(&url, Some("main"), &policy, &mut log, &runner).expect("first check"),
            SourceCheck::Found {
                remote_sha: first.clone()
            }
        );

        std::fs::write(work.join("SKILL.md"), "two").expect("write advanced revision");
        git(&work, &["add", "SKILL.md"]);
        git(&work, &["commit", "-m", "second"]);
        git(&work, &["push", "origin", "main"]);
        let second = git_output(&work, &["rev-parse", "HEAD"]);
        assert_ne!(first, second);
        assert_eq!(
            check_source(&url, Some("main"), &policy, &mut log, &runner).expect("advanced check"),
            SourceCheck::Found { remote_sha: second }
        );

        git(
            &work,
            &[
                "--git-dir",
                bare.to_str().expect("bare path"),
                "update-ref",
                "-d",
                "refs/heads/main",
            ],
        );
        assert_eq!(
            check_source(&url, Some("main"), &policy, &mut log, &runner).expect("deleted check"),
            SourceCheck::SourceGone
        );
    }

    #[test]
    fn offline_policy_returns_unknown_without_calling_runner() {
        struct NeverRun;
        impl RemoteGitRunner for NeverRun {
            fn run(
                &self,
                _: &RemoteGitCall<'_>,
            ) -> Result<crate::policy::GitOutput, crate::policy::GitError> {
                panic!("offline policy must not invoke git");
            }
        }
        let policy = NetworkPolicy::default().with_offline(true);
        let mut log = NetworkEventLog::new();
        let result = check_source(
            "https://example.invalid/repo.git",
            None,
            &policy,
            &mut log,
            &NeverRun,
        )
        .expect("offline is unknown");
        assert_eq!(result, SourceCheck::Unknown);
        assert_eq!(log.entries().len(), 0);
    }
}
