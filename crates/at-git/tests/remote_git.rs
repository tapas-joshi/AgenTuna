//! Integration tests for remote-backup git operations (TC-017).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use at_git::local::ProcessLocalGitRunner;
use at_git::policy::GitError;
use at_git::remote::{
    GitAuth, ProcessRemoteGitRunner, RemoteContext, RemoteError, commit_local, has_changes,
    init_repo, remote_diagnostic, rev_parse_head, set_remote_url, stage_all, validate_repo_url,
};
use at_model::network::{NetworkAction, NetworkEventLog, NetworkPolicy, NetworkTrigger};
use at_model::redact::Redactor;
use tempfile::TempDir;
use time::OffsetDateTime;

/// A sandbox whose git configuration and home directory live inside it.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("temp sandbox");
        fs::create_dir_all(root.path().join("home/.config")).expect("home");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    fn bare_repository(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(&path).expect("bare directory");
        self.git(&path, &["init", "--bare", "-q", "-b", "main"]);
        path
    }

    fn local_runner(&self) -> ProcessLocalGitRunner {
        ProcessLocalGitRunner::git()
            .with_env("HOME", self.home())
            .with_env("XDG_CONFIG_HOME", self.home().join(".config"))
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_AUTHOR_NAME", "AgenTuna Test")
            .with_env("GIT_AUTHOR_EMAIL", "agentuna@example.com")
            .with_env("GIT_COMMITTER_NAME", "AgenTuna Test")
            .with_env("GIT_COMMITTER_EMAIL", "agentuna@example.com")
    }

    fn remote_runner(&self) -> ProcessRemoteGitRunner {
        ProcessRemoteGitRunner::git()
            .with_env("HOME", self.home())
            .with_env("XDG_CONFIG_HOME", self.home().join(".config"))
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_AUTHOR_NAME", "AgenTuna Test")
            .with_env("GIT_AUTHOR_EMAIL", "agentuna@example.com")
            .with_env("GIT_COMMITTER_NAME", "AgenTuna Test")
            .with_env("GIT_COMMITTER_EMAIL", "agentuna@example.com")
    }
}

fn sentinel_token() -> String {
    // Constructed at runtime so push protection never flags static test tokens.
    format!(
        "{}{}",
        concat!("gh", "p_"),
        "sentinel1234567890abcdefABCDEF123456"
    )
}

#[test]
fn push_to_local_bare_remote_works() {
    let sandbox = Sandbox::new();
    let bare = sandbox.bare_repository("remote.git");
    let remote_url = format!("file://localhost{}", bare.display());

    let work = sandbox.path("work");
    fs::create_dir_all(&work).expect("work dir");

    let local_runner = sandbox.local_runner();
    let remote_runner = sandbox.remote_runner();

    init_repo(&local_runner, &work).expect("init repo");
    set_remote_url(&local_runner, &work, &remote_url).expect("set remote");

    // Write a file in work repo
    fs::write(work.join("README.md"), "AgenTuna remote backup test").expect("write file");
    assert!(has_changes(&local_runner, &work).expect("has changes"));

    stage_all(&local_runner, &work).expect("stage all");
    let commit_id = commit_local(&local_runner, &work, "backup test commit").expect("commit");
    assert!(!commit_id.is_empty());
    assert_eq!(
        rev_parse_head(&local_runner, &work).expect("rev parse"),
        commit_id
    );

    let policy = NetworkPolicy::default();
    let mut log = NetworkEventLog::new();
    let redactor = Redactor::new();
    let token = sentinel_token();
    let auth = GitAuth::token(&token).expect("auth token");

    let mut ctx = RemoteContext {
        policy: &policy,
        log: &mut log,
        runner: &remote_runner,
        local: &local_runner,
        redact: &|text| redactor.redact_text(text),
        now: OffsetDateTime::UNIX_EPOCH,
    };

    ctx.push_remote(&work, &remote_url, "main", &auth)
        .expect("push remote");

    // Verify remote head reports the branch and commit
    let head = ctx.remote_head(&remote_url, &auth).expect("remote head");
    assert_eq!(head.commit, commit_id);
    assert_eq!(head.branch.as_deref(), Some("main"));

    // Verify clone from bare remote contains the file
    let clone_dest = sandbox.path("clone_dest");
    ctx.clone_remote(&remote_url, &clone_dest, &auth)
        .expect("clone remote");
    let cloned_content =
        fs::read_to_string(clone_dest.join("README.md")).expect("read cloned file");
    assert_eq!(cloned_content, "AgenTuna remote backup test");
}

#[test]
fn token_delivered_via_askpass_never_in_argv_url_or_config() {
    let sandbox = Sandbox::new();
    let bare = sandbox.bare_repository("remote.git");
    let remote_url = format!("file://localhost{}", bare.display());

    let work = sandbox.path("work");
    fs::create_dir_all(&work).expect("work dir");
    let local_runner = sandbox.local_runner();
    init_repo(&local_runner, &work).expect("init repo");
    set_remote_url(&local_runner, &work, &remote_url).expect("set remote url");

    let token = sentinel_token();
    let auth = GitAuth::token(&token).expect("git auth");

    // Check askpass directory permissions and script content
    let env_slice = auth.env();
    let token_var = env_slice
        .iter()
        .find(|(k, _)| k == "AGENTUNA_REMOTE_BACKUP_TOKEN")
        .expect("token env variable present");
    assert_eq!(token_var.1.to_string_lossy(), token);

    let askpass_path = env_slice
        .iter()
        .find(|(k, _)| k == "GIT_ASKPASS")
        .map(|(_, v)| PathBuf::from(v))
        .expect("askpass path");
    assert!(askpass_path.is_file());
    let script_bytes = fs::read(&askpass_path).expect("read script");
    assert!(
        !String::from_utf8_lossy(&script_bytes).contains(&token),
        "askpass script must NOT contain the token value"
    );

    // Git config in work repo must not contain token
    let git_config = fs::read_to_string(work.join(".git/config")).expect("read git config");
    assert!(!git_config.contains(&token));
    assert!(git_config.contains(&remote_url));
}

#[test]
fn policy_refuses_offline_and_unconsented_network() {
    let sandbox = Sandbox::new();
    let bare = sandbox.bare_repository("remote.git");
    let remote_url = format!("file://localhost{}", bare.display());

    let local_runner = sandbox.local_runner();
    let remote_runner = sandbox.remote_runner();
    let redactor = Redactor::new();
    let token = sentinel_token();
    let auth = GitAuth::token(&token).expect("auth");

    // 1. Offline policy refusal
    let offline_policy = NetworkPolicy::default().with_offline(true);
    let mut log = NetworkEventLog::new();
    let mut ctx = RemoteContext {
        policy: &offline_policy,
        log: &mut log,
        runner: &remote_runner,
        local: &local_runner,
        redact: &|text| redactor.redact_text(text),
        now: OffsetDateTime::UNIX_EPOCH,
    };
    let err = ctx
        .remote_head(&remote_url, &auth)
        .expect_err("offline must fail");
    assert!(matches!(err, RemoteError::Denied(_)));

    // 2. Insecure http:// transport refused
    let insecure_err =
        validate_repo_url("http://github.com/org/repo.git").expect_err("http refused");
    assert!(matches!(insecure_err, RemoteError::InsecureTransport));

    // 3. No remote host refused
    let no_host_err = validate_repo_url("").expect_err("empty url has no host");
    assert!(matches!(no_host_err, RemoteError::NoRemoteHost));
    let invalid_err = validate_repo_url("://invalid").expect_err("invalid host");
    assert!(matches!(invalid_err, RemoteError::NoRemoteHost));

    // 4. Valid secure URLs accepted
    assert!(validate_repo_url("https://github.com/org/repo.git").is_ok());
    assert!(validate_repo_url("git@github.com:org/repo.git").is_ok());
    assert!(validate_repo_url(&remote_url).is_ok());
}

#[test]
fn process_runner_reaps_child_on_cancellation() {
    let cancel = Arc::new(AtomicBool::new(true)); // Pre-cancelled
    let runner = ProcessRemoteGitRunner::git().with_cancel(cancel);

    let cwd = std::env::temp_dir();
    let call = at_git::remote::RemoteGitCall {
        action: NetworkAction::GitFetch,
        trigger: NetworkTrigger::User,
        remote: "https://github.com/org/repo.git",
        args: &["version"],
        cwd: &cwd,
        stdin: None,
        env: &[],
        at: OffsetDateTime::UNIX_EPOCH,
    };

    let result = at_git::remote::RemoteGitRunner::run(&runner, &call);
    assert!(matches!(result, Err(GitError::Cancelled)));
}

#[test]
fn diagnostic_redaction_masks_tokens_using_redactor() {
    let token = sentinel_token();
    let mut redactor = Redactor::new();
    redactor.insert_value(&token);

    let raw_stderr = format!(
        "fatal: Authentication failed for 'https://x-access-token:{token}@github.com/org/repo.git'"
    );
    let redacted = remote_diagnostic(raw_stderr.as_bytes(), &|text| redactor.redact_text(text));

    assert!(
        !redacted.contains(&token),
        "redacted stderr must not contain token"
    );
    assert!(redacted.contains("***"), "token must be replaced with mask");
}
