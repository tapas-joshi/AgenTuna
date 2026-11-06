//! Policy-gated, temporary clones for skill import previews.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use at_model::network::{NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger};
use time::OffsetDateTime;

use crate::remote::{
    ProcessRemoteGitRunner, RemoteError, RemoteGitCall, RemoteGitRunner, run_remote_git,
};

/// Maximum characters retained from a failed git diagnostic.
pub const IMPORT_GIT_DIAGNOSTIC_TAIL_CHARS: usize = 2_048;

/// A temporary clone, removed recursively when dropped.
pub struct ImportedRepository {
    directory: tempfile::TempDir,
    /// Repository root inside the temporary directory.
    pub repository: PathBuf,
    /// Resolved commit checked out by the clone.
    pub commit_sha: String,
}

impl ImportedRepository {
    /// Root of the private temporary directory containing the clone.
    #[must_use]
    pub fn temp_root(&self) -> &Path {
        self.directory.path()
    }
}

/// Failure to clone or inspect a repository for import.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportCloneError {
    /// An unsupported or malformed source URL was supplied.
    #[error("repository URL is not supported for skill import")]
    InvalidUrl,
    /// A ref could not safely be used as a git argument.
    #[error("repository ref is invalid")]
    InvalidRef,
    /// A clone failed; stderr is bounded and contains no credential prompt.
    #[error("CloneFailed: {message}")]
    CloneFailed {
        /// Bounded tail of git stderr with authentication prompts disabled.
        message: String,
    },
    /// Git requested authentication, but this flow never prompts or accesses a keychain.
    #[error("AuthRequired: repository requires credentials; no credentials were requested")]
    AuthRequired,
    /// The central policy denied the network action.
    #[error(transparent)]
    Policy(#[from] RemoteError),
    /// Temporary directory creation failed.
    #[error("could not create temporary git import directory: {0}")]
    Temp(#[from] std::io::Error),
}

/// Shallow-clones a source into `cache_dir`, resolves `HEAD` and returns the owned temp clone.
///
/// The process uses argv only, has prompts disabled in `ProcessRemoteGitRunner`, and is routed
/// through `NetworkAction::GitClone` policy. `file://` URLs are intended only for offline tests.
///
/// # Errors
/// Returns [`ImportCloneError`] for denied policy, invalid input, authentication, clone failure,
/// or temporary filesystem errors. The temporary directory is removed on every failure.
pub fn clone_for_import(
    url: &str,
    reference: Option<&str>,
    cache_dir: &Path,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
    runner: &dyn RemoteGitRunner,
) -> Result<ImportedRepository, ImportCloneError> {
    validate_import_url(url)?;
    if reference.is_some_and(|value| {
        value.is_empty() || value.starts_with('-') || value.contains(['\0', '\n', '\r'])
    }) {
        return Err(ImportCloneError::InvalidRef);
    }
    std::fs::create_dir_all(cache_dir)?;
    let directory = tempfile::Builder::new()
        .prefix("git-skill-import-")
        .tempdir_in(cache_dir)?;
    let repository = directory.path().join("repo");
    let repository_arg = repository.to_string_lossy().into_owned();
    let mut args = vec![
        "-c",
        "credential.helper=",
        "clone",
        "--depth",
        "1",
        "--no-tags",
    ];
    if let Some(reference) = reference {
        args.extend(["--branch", reference]);
    }
    args.extend(["--", url, &repository_arg]);
    let env = [
        (OsString::from("GIT_ASKPASS"), OsString::new()),
        (OsString::from("SSH_ASKPASS"), OsString::new()),
        (
            OsString::from("GIT_SSH_COMMAND"),
            OsString::from("ssh -o BatchMode=yes -o IdentityAgent=none"),
        ),
    ];
    let clone_call = RemoteGitCall {
        action: NetworkAction::GitClone,
        trigger: NetworkTrigger::User,
        remote: url,
        args: &args,
        cwd: directory.path(),
        stdin: None,
        env: &env,
        at: OffsetDateTime::now_utc(),
    };
    let output = run_remote_git(policy, log, &clone_call, runner).map_err(classify_remote_error)?;
    if !output.success {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        if is_auth_failure(&diagnostic) {
            return Err(ImportCloneError::AuthRequired);
        }
        return Err(ImportCloneError::CloneFailed {
            message: diagnostic_tail(&diagnostic),
        });
    }
    let head_args = ["rev-parse", "--verify", "HEAD"];
    let head_call = RemoteGitCall {
        action: NetworkAction::GitClone,
        trigger: NetworkTrigger::User,
        remote: url,
        args: &head_args,
        cwd: &repository,
        stdin: None,
        env: &env,
        at: OffsetDateTime::now_utc(),
    };
    let head = run_remote_git(policy, log, &head_call, runner).map_err(classify_remote_error)?;
    if !head.success {
        return Err(ImportCloneError::CloneFailed {
            message: diagnostic_tail(&String::from_utf8_lossy(&head.stderr)),
        });
    }
    let commit_sha = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if !matches!(commit_sha.len(), 40 | 64)
        || !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ImportCloneError::CloneFailed {
            message: "git returned an invalid commit identifier".to_owned(),
        });
    }
    Ok(ImportedRepository {
        directory,
        repository,
        commit_sha,
    })
}

/// Uses the application runner with the same policy and cleanup contract.
pub fn clone_for_import_with_process(
    url: &str,
    reference: Option<&str>,
    cache_dir: &Path,
    policy: &NetworkPolicy,
    log: &mut NetworkEventLog,
) -> Result<ImportedRepository, ImportCloneError> {
    let runner = ProcessRemoteGitRunner::default();
    clone_for_import(url, reference, cache_dir, policy, log, &runner)
}

fn validate_import_url(url: &str) -> Result<(), ImportCloneError> {
    if url.is_empty() || url.starts_with('-') || url.contains(['\0', '\n', '\r']) {
        return Err(ImportCloneError::InvalidUrl);
    }
    let allowed = url.starts_with("https://")
        || url.starts_with("ssh://")
        || url.starts_with("git@")
        || url.starts_with("file://");
    if !allowed {
        return Err(ImportCloneError::InvalidUrl);
    }
    if let Some(authority) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
        if let Some((userinfo, _)) = authority.rsplit_once('@')
            && (url.starts_with("https://") || userinfo.contains(':'))
        {
            return Err(ImportCloneError::InvalidUrl);
        }
    }
    Ok(())
}

fn classify_remote_error(error: RemoteError) -> ImportCloneError {
    match &error {
        RemoteError::AuthRejected => ImportCloneError::AuthRequired,
        _ => ImportCloneError::Policy(error),
    }
}

fn is_auth_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("permission denied (publickey)")
        || lower.contains("repository not found")
}

fn diagnostic_tail(value: &str) -> String {
    let start = value
        .char_indices()
        .rev()
        .nth(IMPORT_GIT_DIAGNOSTIC_TAIL_CHARS - 1)
        .map_or(0, |(index, _)| index);
    value[start..].trim().to_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn clones_file_bare_repo_and_removes_clone_on_drop() {
        let temp = tempfile::tempdir().expect("temp");
        let bare = temp.path().join("source.git");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&work).expect("work");
        let status = Command::new("git")
            .args(["init", "--bare", bare.to_str().expect("path")])
            .status()
            .expect("git init bare");
        assert!(status.success());
        let status = Command::new("git")
            .arg("init")
            .arg(&work)
            .status()
            .expect("git init");
        assert!(status.success());
        std::fs::write(work.join("README"), "fixture").expect("file");
        for (key, value) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ] {
            assert!(
                Command::new("git")
                    .args(["-C", work.to_str().expect("path"), "config", key, value])
                    .status()
                    .expect("config")
                    .success()
            );
        }
        assert!(
            Command::new("git")
                .args(["-C", work.to_str().expect("path"), "add", "README"])
                .status()
                .expect("add")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    work.to_str().expect("path"),
                    "commit",
                    "-m",
                    "fixture"
                ])
                .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
                .status()
                .expect("commit")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    work.to_str().expect("path"),
                    "push",
                    bare.to_str().expect("path"),
                    "HEAD:main"
                ])
                .status()
                .expect("push")
                .success()
        );
        let policy = NetworkPolicy::default().with_updates_opt_in(true);
        let mut log = NetworkEventLog::default();
        let url = format!("file://{}", bare.display());
        let cloned = clone_for_import_with_process(
            &url,
            Some("main"),
            &temp.path().join("cache"),
            &policy,
            &mut log,
        )
        .expect("clone");
        assert_eq!(cloned.commit_sha.len(), 40);
        let clone_root = cloned.temp_root().to_path_buf();
        assert!(clone_root.exists());
        drop(cloned);
        assert!(!clone_root.exists());
    }

    struct FailedCloneRunner {
        stderr: Vec<u8>,
    }

    impl RemoteGitRunner for FailedCloneRunner {
        fn run(
            &self,
            call: &RemoteGitCall<'_>,
        ) -> Result<crate::policy::GitOutput, crate::policy::GitError> {
            assert!(
                call.args
                    .windows(2)
                    .any(|pair| pair == ["-c", "credential.helper="])
            );
            assert!(
                call.env
                    .iter()
                    .any(|(key, value)| key == "GIT_ASKPASS" && value.is_empty())
            );
            assert!(
                call.env
                    .iter()
                    .any(|(key, value)| key == "SSH_ASKPASS" && value.is_empty())
            );
            assert!(call.env.iter().any(|(key, value)| key == "GIT_SSH_COMMAND"
                && value.to_string_lossy().contains("IdentityAgent=none")));
            Ok(crate::policy::GitOutput {
                success: false,
                code: Some(128),
                stdout: Vec::new(),
                stderr: self.stderr.clone(),
            })
        }
    }

    #[test]
    fn failed_clone_classifies_auth_without_prompt_or_leaking_temp_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let cache = temp.path().join("cache");
        let policy = NetworkPolicy::default().with_updates_opt_in(true);
        let mut log = NetworkEventLog::default();
        let url = "file:///synthetic/private.git";
        let auth_runner = FailedCloneRunner { stderr: b"fatal: could not read Username for 'https://example.invalid': terminal prompts disabled".to_vec() };
        assert!(matches!(
            clone_for_import(url, None, &cache, &policy, &mut log, &auth_runner),
            Err(ImportCloneError::AuthRequired)
        ));
        assert!(std::fs::read_dir(&cache).expect("cache").next().is_none());

        let long_diagnostic = format!(
            "{}failure at end",
            "x".repeat(IMPORT_GIT_DIAGNOSTIC_TAIL_CHARS + 32)
        );
        let failed_runner = FailedCloneRunner {
            stderr: long_diagnostic.into_bytes(),
        };
        let Err(ImportCloneError::CloneFailed { message }) =
            clone_for_import(url, None, &cache, &policy, &mut log, &failed_runner)
        else {
            panic!("expected CloneFailed");
        };
        assert!(message.ends_with("failure at end"));
        assert!(message.chars().count() <= IMPORT_GIT_DIAGNOSTIC_TAIL_CHARS);
        assert!(std::fs::read_dir(&cache).expect("cache").next().is_none());
    }
}
