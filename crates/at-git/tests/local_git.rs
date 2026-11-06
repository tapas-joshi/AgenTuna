//! Integration tests for the local git wrapper: real repositories, real `git`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use at_git::local::{
    LocalGitError, ProcessLocalGitRunner, add_paths, detect_repository, explain_ignore,
    ignored_paths, status_entries,
};
use tempfile::TempDir;

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

    fn repository(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(&path).expect("repository directory");
        self.git(&path, &["init", "-q"]);
        path
    }

    fn runner(&self) -> ProcessLocalGitRunner {
        ProcessLocalGitRunner::git()
            .with_env("HOME", self.home())
            .with_env("XDG_CONFIG_HOME", self.home().join(".config"))
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
    }
}

#[test]
fn a_repository_is_found_from_a_nested_directory_and_a_plain_directory_is_not() {
    let sandbox = Sandbox::new();
    let repository = sandbox.repository("repo");
    let nested = repository.join("apps/web");
    fs::create_dir_all(&nested).expect("nested");
    let runner = sandbox.runner();

    let found = detect_repository(&runner, &nested).expect("repository found");
    assert_eq!(
        fs::canonicalize(found.root()).expect("canonical"),
        fs::canonicalize(&repository).expect("canonical")
    );

    let plain = sandbox.path("plain");
    fs::create_dir_all(&plain).expect("plain");
    let error = detect_repository(&runner, &plain).expect_err("no repository");
    assert!(error.is_not_a_repository(), "unexpected error: {error}");

    let missing = sandbox.path("absent");
    let error = detect_repository(&runner, &missing).expect_err("missing directory");
    assert!(matches!(error, LocalGitError::MissingDirectory { .. }));
}

#[test]
fn status_ignore_checks_and_adding_behave_like_git() {
    let sandbox = Sandbox::new();
    let repository = sandbox.repository("repo");
    fs::create_dir_all(repository.join(".site-store/.state")).expect("state");
    fs::write(repository.join(".site-store/.state/journal.json"), "{}\n").expect("journal");
    fs::write(repository.join(".site-store/store.json"), "{}\n").expect("store");
    fs::write(repository.join(".site-store/.gitignore"), ".state/\n").expect("gitignore");
    let runner = sandbox.runner();
    let repo = detect_repository(&runner, &repository).expect("repository");

    let status = status_entries(&runner, &repo, Some(".site-store")).expect("status");
    let paths: Vec<&str> = status.iter().map(|entry| entry.path.as_str()).collect();
    assert!(!paths.is_empty(), "untracked store content is reported");
    assert!(
        paths.iter().all(|path| path.starts_with(".site-store")),
        "status is limited to the requested path: {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.contains(".state")),
        "the ignore rule hides machine state: {paths:?}"
    );
    assert!(status.iter().all(|entry| !entry.is_staged()));

    let ignored = ignored_paths(
        &runner,
        &repo,
        &[".site-store/.state/journal.json", ".site-store/store.json"],
    )
    .expect("check-ignore");
    assert_eq!(ignored, vec![".site-store/.state/journal.json".to_owned()]);

    let rule = explain_ignore(&runner, &repo, ".site-store/.state/journal.json")
        .expect("explain")
        .expect("a rule");
    assert_eq!(rule.source, ".site-store/.gitignore");
    assert_eq!(rule.line, Some(1));
    assert_eq!(rule.pattern, ".state/");
    assert!(
        explain_ignore(&runner, &repo, ".site-store/store.json")
            .expect("explain")
            .is_none()
    );

    add_paths(
        &runner,
        &repo,
        &[".site-store/.gitignore", ".site-store/store.json"],
    )
    .expect("add");
    let status = status_entries(&runner, &repo, Some(".site-store")).expect("status");
    assert!(status.iter().all(at_git::local::StatusEntry::is_staged));
}

#[test]
fn git_refuses_an_ignored_path_so_it_can_never_be_forced_in() {
    let sandbox = Sandbox::new();
    let repository = sandbox.repository("repo");
    fs::write(repository.join(".gitignore"), "secret.txt\n").expect("gitignore");
    fs::write(repository.join("secret.txt"), "x\n").expect("secret");
    let runner = sandbox.runner();
    let repo = detect_repository(&runner, &repository).expect("repository");

    let error = add_paths(&runner, &repo, &["secret.txt"]).expect_err("git refuses");
    let message = error.to_string();
    assert!(
        message.contains("ignored"),
        "the refusal explains why: {message}"
    );
    let status = status_entries(&runner, &repo, None).expect("status");
    assert!(
        status
            .iter()
            .all(|entry| !(entry.path == "secret.txt" && entry.is_staged())),
        "nothing was staged"
    );
}

#[test]
fn unsafe_pathspecs_are_rejected_before_git_runs() {
    let sandbox = Sandbox::new();
    let repository = sandbox.repository("repo");
    let runner = sandbox.runner();
    let repo = detect_repository(&runner, &repository).expect("repository");

    for path in ["../outside", "/etc/passwd", "", "./relative"] {
        let error = add_paths(&runner, &repo, &[path]).expect_err("rejected");
        assert!(
            matches!(error, LocalGitError::UnsafePathspec { .. }),
            "{path:?} produced {error}"
        );
    }
}
