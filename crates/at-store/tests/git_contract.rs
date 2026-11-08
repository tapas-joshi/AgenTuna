//! Acceptance tests for TC-004: the Project store's git contract.
//!
//! Every test builds a real temporary git repository and runs the real `git` binary, with
//! `HOME`, `XDG_CONFIG_HOME` and git's system/global configuration pointed at the sandbox, so a
//! developer's own ignore rules cannot change the result.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use at_git::local::ProcessLocalGitRunner;
use at_model::Scope;
use at_store::git_contract::{
    COMMITTED_ENTRIES, GITIGNORE_RULES, GitContractError, GitWarning, ProjectGitContract,
};
use at_store::layout::{StoreLayout, init_store};
use tempfile::TempDir;

/// A sandbox with a temporary home, a project directory and git configuration of its own.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("temp sandbox");
        fs::create_dir_all(root.path().join("home")).expect("home");
        fs::create_dir_all(root.path().join("home/.config")).expect("config dir");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Creates a directory inside the sandbox.
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(&path).expect("sandbox directory");
        path
    }

    /// Runs git in `cwd` with the sandbox's environment.
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs")
    }

    /// Initialises a repository at `path`.
    fn init_repository(&self, path: &Path) {
        let output = self.git(path, &["init", "-q"]);
        assert!(output.status.success(), "git init: {output:?}");
    }

    /// A runner configured the way the application configures it for a sandbox.
    fn runner(&self) -> ProcessLocalGitRunner {
        ProcessLocalGitRunner::git()
            .with_env("HOME", self.home())
            .with_env("XDG_CONFIG_HOME", self.home().join(".config"))
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
    }

    /// Repository-relative paths reported by `git status --porcelain -uall` inside `cwd`.
    fn status(&self, cwd: &Path, pathspec: &str) -> Vec<String> {
        let output = self.git(cwd, &["status", "--porcelain=v1", "-uall", "--", pathspec]);
        assert!(output.status.success(), "git status: {output:?}");
        String::from_utf8(output.stdout)
            .expect("utf-8 status")
            .lines()
            .map(|line| line[3..].to_owned())
            .collect()
    }

    /// Repository-relative paths in the index.
    fn staged_files(&self, cwd: &Path) -> BTreeSet<String> {
        let output = self.git(cwd, &["ls-files", "-z"]);
        assert!(output.status.success(), "git ls-files: {output:?}");
        String::from_utf8(output.stdout)
            .expect("utf-8 index")
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Whether the repository has a commit.
    fn has_commit(&self, cwd: &Path) -> bool {
        self.git(cwd, &["rev-parse", "--verify", "HEAD"])
            .status
            .success()
    }
}

fn project_contract(project: &Path) -> ProjectGitContract {
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    ProjectGitContract::new(layout).expect("project contract")
}

/// Every path under `root`, for a "nothing was written" assertion.
fn snapshot(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("entry");
            let path = entry.path();
            out.insert(path.clone());
            if entry.file_type().expect("file type").is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, &mut out);
    out
}

/// Result of writing a plausible post-migration store.
fn populate_store(layout: &StoreLayout) {
    fs::create_dir_all(layout.skills_dir().join("demo")).expect("skill dir");
    fs::write(
        layout.skills_dir().join("demo").join("SKILL.md"),
        "---\nname: demo\ndescription: fixture\n---\n",
    )
    .expect("skill");
    fs::write(layout.rules_file(), "# rules\n").expect("rules");
    fs::write(layout.journals_dir().join("journal.json"), "{}\n").expect("journal");
    fs::create_dir_all(layout.backups_dir().join("backup-1")).expect("backup dir");
    fs::write(
        layout.backups_dir().join("backup-1").join("file.bak"),
        "old\n",
    )
    .expect("backup");
    if let Some(local) = layout.local_dir() {
        fs::create_dir_all(&local).expect("local dir");
        fs::write(local.join("dev.md"), "local only\n").expect("local content");
    }
    fs::write(layout.root().join("scratch.tmp"), "tmp\n").expect("tmp file");
    fs::write(layout.root().join(".DS_Store"), "finder\n").expect("ds store");
    fs::write(layout.root().join("settings.local.json"), "{}\n").expect("override");
}

#[test]
fn the_gitignore_keeps_machine_local_content_out_of_status() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);

    let contract = project_contract(&project);
    let install = contract.install_gitignore().expect("gitignore written");
    assert!(install.created, "the first install creates the file");
    assert_eq!(
        install.appended,
        GITIGNORE_RULES
            .iter()
            .map(|rule| (*rule).to_owned())
            .collect::<Vec<_>>()
    );
    assert!(install.changed());

    let bytes = fs::read(contract.gitignore_path()).expect("read gitignore");
    let text = String::from_utf8(bytes).expect("utf-8 gitignore");
    for rule in GITIGNORE_RULES {
        assert!(
            text.lines().any(|line| line.trim() == rule),
            "{rule} must be a line of the store .gitignore"
        );
    }

    // A migration leaves exactly the committed set visible to git.
    let mut status = sandbox.status(&project, ".agentuna");
    status.sort();
    assert_eq!(
        status,
        vec![
            ".agentuna/.gitignore".to_owned(),
            ".agentuna/mcp.json".to_owned(),
            ".agentuna/rules/AGENTS.md".to_owned(),
            ".agentuna/skills/demo/SKILL.md".to_owned(),
            ".agentuna/store.json".to_owned(),
        ],
        "backups, local content and machine state stay invisible to git"
    );

    for entry in COMMITTED_ENTRIES {
        let prefix = format!(".agentuna/{entry}");
        assert!(
            status
                .iter()
                .any(|path| path == &prefix || path.starts_with(&format!("{prefix}/"))),
            "{entry} is part of the committed set"
        );
    }
    let second = contract.install_gitignore().expect("second install");
    assert!(!second.changed(), "installing twice changes nothing");
}

#[test]
fn a_hand_edited_gitignore_keeps_its_lines_and_gains_the_missing_rules() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    let contract = project_contract(&project);
    fs::write(
        contract.gitignore_path(),
        "# mine\nnode_modules/\n.DS_Store\n",
    )
    .expect("hand edited");

    let install = contract.install_gitignore().expect("merge");
    assert!(!install.created);
    assert!(!install.appended.contains(&".DS_Store".to_owned()));
    assert!(install.appended.contains(&".state/".to_owned()));

    let text = fs::read_to_string(contract.gitignore_path()).expect("read");
    assert!(
        text.starts_with("# mine\nnode_modules/\n.DS_Store\n"),
        "existing lines are preserved: {text}"
    );
    assert_eq!(text.matches(".DS_Store").count(), 1, "no duplicates");
    for rule in GITIGNORE_RULES {
        assert!(
            text.lines().any(|line| line.trim() == rule),
            "{rule} present"
        );
    }
}

#[test]
fn staging_adds_exactly_the_committed_files_and_never_commits() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);
    let contract = project_contract(&project);
    contract.install_gitignore().expect("gitignore");
    let runner = sandbox.runner();

    let outcome = contract.stage(&runner).expect("stage");
    assert_eq!(outcome.store_path, ".agentuna");
    assert_eq!(
        outcome.staged,
        vec![
            ".agentuna/.gitignore".to_owned(),
            ".agentuna/mcp.json".to_owned(),
            ".agentuna/rules/AGENTS.md".to_owned(),
            ".agentuna/skills/demo/SKILL.md".to_owned(),
            ".agentuna/store.json".to_owned(),
        ],
        "the exact paths are reported"
    );
    assert!(outcome.skipped_ignored.is_empty());

    let index = sandbox.staged_files(&project);
    assert_eq!(index, outcome.staged.iter().cloned().collect());
    for forbidden in [
        ".agentuna/.state/journals/journal.json",
        ".agentuna/backups",
    ] {
        assert!(
            !index.iter().any(|path| path.starts_with(forbidden)),
            "{forbidden} must never be staged"
        );
    }
    assert!(
        !sandbox.has_commit(&project),
        "staging never creates a commit"
    );
}

#[test]
fn staging_skips_a_committed_path_a_parent_rule_ignores() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);
    let contract = project_contract(&project);
    contract.install_gitignore().expect("gitignore");
    fs::write(project.join(".gitignore"), "*.json\n").expect("parent rule");
    let runner = sandbox.runner();

    let outcome = contract.stage(&runner).expect("stage");
    assert_eq!(
        outcome.skipped_ignored,
        vec![
            ".agentuna/mcp.json".to_owned(),
            ".agentuna/store.json".to_owned()
        ],
        "ignored committed files are reported, not forced in"
    );
    assert!(outcome.staged.iter().all(|path| !path.ends_with(".json")));
    let index = sandbox.staged_files(&project);
    assert!(!index.iter().any(|path| path.ends_with(".json")));
}

#[test]
fn staging_without_a_repository_reports_it_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("plain");
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);
    let contract = project_contract(&project);
    let before = snapshot(layout.root());

    let error = contract
        .stage(&sandbox.runner())
        .expect_err("no repository");
    assert!(
        matches!(error, GitContractError::NotAGitRepository { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(snapshot(layout.root()), before, "nothing was written");
    assert!(!project.join(".git").exists());
}

#[test]
fn staging_refuses_while_the_store_gitignore_is_missing() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    let contract = project_contract(&project);

    let error = contract.stage(&sandbox.runner()).expect_err("no gitignore");
    assert!(
        matches!(error, GitContractError::GitignoreMissing { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_parent_rule_that_ignores_the_store_is_warned_about_with_its_source() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    sandbox.init_repository(&project);
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);
    let contract = project_contract(&project);
    contract.install_gitignore().expect("gitignore");
    fs::write(
        project.join(".gitignore"),
        "# ignore the store\n.agentuna/\n",
    )
    .expect("rule");
    let runner = sandbox.runner();

    let state = contract.inspect(&runner).expect("inspect");
    assert!(state.has_repository());
    assert!(!state.can_stage(), "an ignored store cannot be staged");
    let rule = state.ignored.expect("the rule is reported");
    assert_eq!(rule.source, ".gitignore");
    assert_eq!(rule.line, Some(2));
    assert_eq!(rule.pattern, ".agentuna/");

    let warning = state
        .warnings
        .iter()
        .find(|warning| matches!(warning, GitWarning::StoreIgnored { .. }))
        .expect("a store-ignored warning");
    let text = warning.to_string();
    assert!(
        text.contains(".gitignore:2:.agentuna/"),
        "the warning names the rule source: {text}"
    );

    let error = contract.stage(&runner).expect_err("refused");
    assert!(
        matches!(error, GitContractError::StoreIgnored { .. }),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains(".gitignore:2:.agentuna/"));
}

#[test]
fn a_nested_project_is_reported_with_the_repository_above_it() {
    let sandbox = Sandbox::new();
    let repository = sandbox.dir("monorepo");
    sandbox.init_repository(&repository);
    let project = sandbox.dir("monorepo/apps/web");
    let layout = StoreLayout::new(Scope::Project, project.join(".agentuna"));
    init_store(Scope::Project, layout.root()).expect("store init");
    populate_store(&layout);
    let contract = project_contract(&project);
    contract.install_gitignore().expect("gitignore");

    let state = contract.inspect(&sandbox.runner()).expect("inspect");
    let nested = state.nested_in.expect("nested repository");
    assert_eq!(
        fs::canonicalize(&nested).expect("canonical"),
        fs::canonicalize(&repository).expect("canonical")
    );
    assert_eq!(state.store_path.as_deref(), Some("apps/web/.agentuna"));
    assert!(state.dirty, "the fresh store has uncommitted changes");
    assert!(
        state
            .warnings
            .iter()
            .any(|warning| matches!(warning, GitWarning::NestedRepository { .. }))
    );
}

#[test]
fn a_symlinked_store_is_rejected_before_any_write() {
    let sandbox = Sandbox::new();
    let project = sandbox.dir("repo");
    let target = sandbox.dir("elsewhere/store");
    let store = project.join(".agentuna");
    symlink(&target, &store).expect("store symlink");

    let layout = StoreLayout::new(Scope::Project, &store);
    let error = ProjectGitContract::new(layout).expect_err("symlinked store");
    assert!(
        matches!(error, GitContractError::SymlinkedStore { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read_dir(&target).expect("read target").count(),
        0,
        "nothing was written through the symlink"
    );
}

#[test]
fn only_the_project_scope_has_a_git_contract() {
    let sandbox = Sandbox::new();
    let layout = StoreLayout::new(Scope::User, sandbox.dir("user-store"));
    let error = ProjectGitContract::new(layout).expect_err("user store");
    assert!(matches!(
        error,
        GitContractError::NotProjectScope { scope: Scope::User }
    ));
}
