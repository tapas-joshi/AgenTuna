//! Local scope end to end: temp HOME, real Git work tree, store files on disk (TA-096).
//!
//! Every test builds its own [`at_fs::testkit::TestHome`], a project fixture and a throwaway
//! Git repository inside it. Nothing here reads the real home, the real agent folders or global
//! Git configuration; `git` is invoked with `GIT_CONFIG_GLOBAL=/dev/null` and an explicit
//! identity so a developer's `~/.gitconfig` cannot change the result.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use at_engine::local_scope::{
    GitignoreChange, LOCAL_IGNORE_LINE, LocalLayer, LocalScope, LocalScopeError, LocalScopeStatus,
    LocalUnavailableReason, resolve_with_local,
};
use at_fs::testkit::TestHome;
use at_model::{Definition, ItemId, ItemKind, Scope, ScopeSnapshot};
use at_store::layout::LOCAL_DIR;
use at_store::local_store::LocalStorePaths;
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use at_store::skill::read_skill_doc;

/// Git identity forced on every invocation so the developer's configuration is irrelevant.
const IDENTITY: [&str; 6] = [
    "-c",
    "user.name=AgenTuna Tests",
    "-c",
    "user.email=tests@example.invalid",
    "-c",
    "commit.gpgsign=false",
];

// ---------------------------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------------------------

fn app_paths(home: &Path, project: &Path) -> AppPaths {
    AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(home)
            .with_project_root(project)
            .with_overrides(PathOverrides {
                global_store: Some(home.join("global")),
                ..PathOverrides::default()
            }),
    )
    .expect("fixture app paths resolve")
}

fn local_scope(home: &Path, project: &Path) -> LocalScope {
    let paths = app_paths(home, project);
    let store = LocalStorePaths::for_paths(&paths).expect("project has a Local store");
    LocalScope::detect(project.to_path_buf(), store)
}

fn skill_document(name: &str, description: &str) -> Vec<u8> {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n").into_bytes()
}

fn write_skill(root: &Path, slug: &str, name: &str, description: &str) -> PathBuf {
    let path = root.join("skills").join(slug).join("SKILL.md");
    fs::create_dir_all(path.parent().expect("skill path has a parent")).expect("create skill dir");
    fs::write(&path, skill_document(name, description)).expect("write SKILL.md");
    path
}

fn load_skills(root: &Path, _scope: Scope) -> Vec<Definition> {
    let mut definitions = Vec::new();
    let Ok(entries) = fs::read_dir(root.join("skills")) else {
        return definitions;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let file = dir.join("SKILL.md");
        let doc =
            read_skill_doc(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        definitions.push(Definition {
            kind: ItemKind::Skill,
            name: doc.name,
            declared_id: None,
            item_id: Some(ItemId::new()),
            path: file,
        });
    }
    definitions
}

fn layer(scope: Scope, definitions: Vec<Definition>) -> ScopeSnapshot {
    ScopeSnapshot { scope, definitions }
}

fn local_layer(scope: &LocalScope) -> LocalLayer {
    let definitions = load_skills(scope.store().root(), Scope::Local);
    LocalLayer::new(scope.clone(), definitions).expect("local definitions live in the local store")
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is UTF-8")
}

/// Creates a work tree and commits the project fixture's files so later status is meaningful.
fn init_repository(root: &Path) {
    git(root, &["-c", "init.defaultBranch=main", "init", "-q"]);
    let mut add = IDENTITY.to_vec();
    add.extend(["add", "-A"]);
    git(root, &add);
    let mut commit = IDENTITY.to_vec();
    commit.extend(["commit", "-q", "-m", "fixture"]);
    git(root, &commit);
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn a_local_definition_overrides_project_and_reports_scope_local() {
    let home = TestHome::new().expect("temp home");
    let project = home.project("precedence").expect("project fixture");
    init_repository(project.root());

    let user_root = home.home().join(".agentuna");
    let project_root = project.store();
    let local_root = project_root.join(LOCAL_DIR);
    write_skill(&user_root, "code-review", "code-review", "User copy.");
    write_skill(&project_root, "code-review", "code-review", "Project copy.");
    let local_skill = write_skill(&local_root, "code-review", "code-review", "Local copy.");

    let scope = local_scope(home.home(), project.root());
    let local = local_layer(&scope);
    let layers = [
        layer(Scope::User, load_skills(&user_root, Scope::User)),
        layer(Scope::Project, load_skills(&project_root, Scope::Project)),
    ];
    let outcome = resolve_with_local(&layers, &local);

    assert_eq!(outcome.local, LocalScopeStatus::Available);
    let winner = outcome.resolved.items.first().expect("one resolved skill");
    assert_eq!(winner.scope, Scope::Local);
    assert_eq!(winner.definition.path, local_skill);
    assert_eq!(
        serde_json::to_value(winner.scope).expect("scope serializes"),
        serde_json::Value::String("local".to_owned()),
        "the effective payload spells the winning scope `local`"
    );
    assert_eq!(
        outcome
            .resolved
            .shadowed
            .iter()
            .map(|shadowed| shadowed.scope)
            .collect::<Vec<_>>(),
        vec![Scope::User, Scope::Project],
        "both inherited copies are reported as shadowed"
    );
    assert!(local_skill.exists());
}

#[test]
fn two_gitignore_calls_add_one_line_and_git_never_stages_a_local_file() {
    let home = TestHome::new().expect("temp home");
    let project = home.project("gitignore").expect("project fixture");
    init_repository(project.root());

    let scope = local_scope(home.home(), project.root());
    let local_skill = write_skill(
        scope.store().root(),
        "code-review",
        "code-review",
        "Local copy.",
    );
    let stale_plan = write_skill(
        scope.store().root(),
        "experiment",
        "experiment",
        "Another local skill.",
    );

    assert_eq!(
        scope.ensure_project_gitignore().expect("first call"),
        GitignoreChange::Created
    );
    let after_first =
        fs::read(project.root().join(".gitignore")).expect("read gitignore after the first call");
    assert_eq!(
        scope.ensure_project_gitignore().expect("second call"),
        GitignoreChange::Unchanged,
        "the second call is idempotent"
    );
    assert_eq!(
        fs::read(project.root().join(".gitignore")).expect("read gitignore after the second call"),
        after_first,
        "the second call must not rewrite a single byte"
    );

    let gitignore = fs::read_to_string(project.root().join(".gitignore")).expect("read gitignore");
    assert_eq!(
        gitignore
            .lines()
            .filter(|line| *line == LOCAL_IGNORE_LINE)
            .count(),
        1,
        "exactly one ignore line: {gitignore:?}"
    );

    git(project.root(), &["add", "-A"]);
    let status = git_stdout(project.root(), &["status", "--porcelain"]);
    assert!(
        !status.contains(".agentuna/local"),
        "no Local path may appear in git status:\n{status}"
    );
    let staged = git_stdout(project.root(), &["ls-files"]);
    assert!(
        !staged.contains(".agentuna/local"),
        "no Local path may be staged:\n{staged}"
    );
    for path in [&local_skill, &stale_plan] {
        let relative = path
            .strip_prefix(project.root())
            .expect("local content lives in the project");
        let output = Command::new("git")
            .arg("-C")
            .arg(project.root())
            .args(["check-ignore", "-q", &relative.to_string_lossy()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git is installed");
        assert!(
            output.status.success(),
            "{} must be ignored",
            relative.display()
        );
    }
}

#[test]
fn two_projects_resolve_independently_against_one_user_store() {
    let home = TestHome::new().expect("temp home");
    let first = home.project("first").expect("first project");
    let second = home.project("second").expect("second project");
    init_repository(first.root());
    init_repository(second.root());

    let user_root = home.home().join(".agentuna");
    write_skill(&user_root, "code-review", "code-review", "User copy.");
    write_skill(
        &user_root,
        "release-notes",
        "release-notes",
        "User release notes.",
    );

    let first_scope = local_scope(home.home(), first.root());
    write_skill(
        first_scope.store().root(),
        "code-review",
        "code-review",
        "First project's local copy.",
    );
    let second_scope = local_scope(home.home(), second.root());
    write_skill(
        second_scope.store().root(),
        "release-notes",
        "release-notes",
        "Second project's local copy.",
    );

    // Tombstones are per-project store.json state. TA-003 owns applying them; this test proves
    // each project's Local scope and tombstone list stay inside its own repository.
    let first_tombstone = r#"{"schema_version":1,"disabled":[{"item_id":"itm_01930000-0000-7000-8000-000000000001","kind":"skill","agents":null}]}"#;
    let second_tombstone = r#"{"schema_version":1,"disabled":[{"item_id":"itm_01930000-0000-7000-8000-000000000002","kind":"skill","agents":["codex"]}]}"#;
    fs::write(first_scope.store().store_file(), first_tombstone).expect("first tombstone");
    fs::write(second_scope.store().store_file(), second_tombstone).expect("second tombstone");

    let user_layer = layer(Scope::User, load_skills(&user_root, Scope::User));
    let first_outcome = resolve_with_local(
        std::slice::from_ref(&user_layer),
        &local_layer(&first_scope),
    );
    let second_outcome = resolve_with_local(&[user_layer], &local_layer(&second_scope));

    let first_review = first_outcome
        .resolved
        .items
        .iter()
        .find(|item| item.definition.name == "code-review")
        .expect("first project resolves code-review");
    assert_eq!(first_review.scope, Scope::Local);
    assert!(first_review.definition.path.starts_with(first.root()));

    let second_review = second_outcome
        .resolved
        .items
        .iter()
        .find(|item| item.definition.name == "code-review")
        .expect("second project resolves code-review");
    assert_eq!(
        second_review.scope,
        Scope::User,
        "the first project's Local override must not leak into the second"
    );

    let first_release = first_outcome
        .resolved
        .items
        .iter()
        .find(|item| item.definition.name == "release-notes")
        .expect("first project resolves release-notes");
    assert_eq!(
        first_release.scope,
        Scope::User,
        "the second project's Local override must not leak into the first"
    );
    let second_release = second_outcome
        .resolved
        .items
        .iter()
        .find(|item| item.definition.name == "release-notes")
        .expect("second project resolves release-notes");
    assert_eq!(second_release.scope, Scope::Local);

    for (scope, expected) in [
        (&first_scope, first_tombstone),
        (&second_scope, second_tombstone),
    ] {
        let stored = fs::read_to_string(scope.store().store_file()).expect("read tombstone");
        assert_eq!(
            stored, expected,
            "each project keeps its own tombstone list"
        );
    }
    assert!(
        !first_scope.store().root().starts_with(second.root()),
        "the projects' Local stores are separate directories"
    );
}

#[test]
fn a_project_without_a_work_tree_reports_local_not_available_without_failing() {
    let home = TestHome::new().expect("temp home");
    let project = home.project("no-git").expect("project fixture");

    let project_root = project.store();
    let local_root = project_root.join(LOCAL_DIR);
    write_skill(&project_root, "code-review", "code-review", "Project copy.");
    write_skill(&local_root, "code-review", "code-review", "Local copy.");

    let scope = local_scope(home.home(), project.root());
    assert_eq!(
        scope.status(),
        LocalScopeStatus::NotAvailable {
            reason: LocalUnavailableReason::NotAGitRepository
        }
    );
    assert!(scope.exclude_path().is_none());

    let local = local_layer(&scope);
    let outcome = resolve_with_local(
        &[layer(
            Scope::Project,
            load_skills(&project_root, Scope::Project),
        )],
        &local,
    );
    assert!(!outcome.local.is_available());
    let winner = outcome.resolved.items.first().expect("a winner");
    assert_eq!(winner.scope, Scope::Project);
    assert!(
        !outcome
            .resolved
            .items
            .iter()
            .any(|item| { item.definition.path.starts_with(scope.store().root()) })
    );

    let deployed = local_root.join("skills/code-review");
    let error = scope
        .register_deployments(&[deployed])
        .expect_err("deployment bookkeeping needs a work tree");
    assert!(matches!(error, LocalScopeError::LocalNotAvailable { .. }));
}

#[test]
fn deleting_the_local_store_restores_the_pre_local_winner() {
    let home = TestHome::new().expect("temp home");
    let project = home.project("removal").expect("project fixture");
    init_repository(project.root());

    let user_root = home.home().join(".agentuna");
    let project_root = project.store();
    write_skill(&user_root, "code-review", "code-review", "User copy.");
    write_skill(&project_root, "code-review", "code-review", "Project copy.");
    let local_root = project_root.join(LOCAL_DIR);
    write_skill(&local_root, "code-review", "code-review", "Local copy.");

    let scope = local_scope(home.home(), project.root());
    let layers = [
        layer(Scope::User, load_skills(&user_root, Scope::User)),
        layer(Scope::Project, load_skills(&project_root, Scope::Project)),
    ];
    let before = resolve_with_local(&layers, &local_layer(&scope));
    assert_eq!(before.resolved.items[0].scope, Scope::Local);

    fs::remove_dir_all(&local_root).expect("delete the local store");
    assert!(!local_root.exists());

    let after_scope = local_scope(home.home(), project.root());
    let after = resolve_with_local(&layers, &local_layer(&after_scope));
    let winner = after.resolved.items.first().expect("a winner remains");
    assert_eq!(winner.scope, Scope::Project);
    assert_eq!(
        winner.definition.path,
        project_root.join("skills/code-review/SKILL.md")
    );
    assert!(
        after
            .resolved
            .items
            .iter()
            .all(|item| !item.definition.path.starts_with(&local_root)),
        "a removed Local store must leave no winner referencing it"
    );
    assert!(
        after
            .resolved
            .shadowed
            .iter()
            .all(|shadowed| !shadowed.definition.path.starts_with(&local_root)),
        "a removed Local store must leave no shadow reference behind"
    );
    assert!(
        after_scope.status().is_available(),
        "the work tree is still there"
    );
}
