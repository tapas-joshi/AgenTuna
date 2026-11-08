//! The Local scope layer: precedence, availability, store paths and Git bookkeeping.
//!
//! Every test runs in a temporary directory with an injected home; none touches the real
//! user's stores, agent folders or Git configuration.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use at_engine::local_scope::{
    ExcludeChange, GitignoreChange, LocalLayer, LocalScope, LocalScopeError, LocalScopeStatus,
    LocalUnavailableReason, deployment_pattern, ensure_git_exclude, ensure_gitignore,
    resolve_with_local,
};
use at_model::{Definition, ItemKind, Scope, ScopeSnapshot};
use at_store::layout::StoreLayout;
use at_store::local_store::LocalStorePaths;
use at_store::paths::{AppPaths, PathEnvironment};

fn project_store(project: &Path) -> PathBuf {
    project.join(".agentuna")
}

/// A Local scope for `project` with a store layout built the way production builds it.
fn local_scope(project: &Path) -> LocalScope {
    let project_layout = StoreLayout::new(Scope::Project, project_store(project));
    let store = LocalStorePaths::from_project(&project_layout).expect("project layout is local");
    LocalScope::detect(project, store)
}

fn app_paths(home: &Path, project: &Path) -> AppPaths {
    AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(home)
            .with_project_root(project),
    )
    .expect("app paths resolve")
}

fn git_repository(project: &Path) {
    fs::create_dir_all(project.join(".git/info")).expect("create .git/info");
    fs::write(project.join(".git/info/exclude"), "").expect("create exclude");
}

fn skill(name: &str, path: &Path) -> Definition {
    Definition {
        kind: ItemKind::Skill,
        name: name.to_owned(),
        declared_id: None,
        item_id: None,
        path: path.to_path_buf(),
    }
}

fn definition(kind: ItemKind, name: &str, path: &Path) -> Definition {
    Definition {
        kind,
        name: name.to_owned(),
        declared_id: None,
        item_id: None,
        path: path.to_path_buf(),
    }
}

fn layer(scope: Scope, definitions: Vec<Definition>) -> ScopeSnapshot {
    ScopeSnapshot { scope, definitions }
}

#[test]
fn local_definitions_win_over_project_and_report_their_source_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    git_repository(project);
    let scope = local_scope(project);
    assert_eq!(scope.status(), LocalScopeStatus::Available);

    let local_path = scope
        .item_path(ItemKind::Skill, "code-review")
        .expect("local skill path")
        .join("SKILL.md");
    let project_path = project_store(project).join("skills/code-review/SKILL.md");
    let local_definition = skill("Code Review", &local_path);
    let project_definition = skill("Code Review", &project_path);

    let local =
        LocalLayer::new(scope.clone(), vec![local_definition.clone()]).expect("local layer");
    let outcome = resolve_with_local(&[layer(Scope::Project, vec![project_definition])], &local);

    assert_eq!(outcome.local, LocalScopeStatus::Available);
    let winner = outcome
        .resolved
        .get(&local_definition.logical_id())
        .expect("logical id resolves");
    assert_eq!(winner.scope, Scope::Local);
    assert_eq!(winner.definition.path, local_path);
    assert!(winner.definition.path.starts_with(scope.store().root()));

    assert_eq!(outcome.resolved.shadowed.len(), 1);
    assert_eq!(outcome.resolved.shadowed[0].scope, Scope::Project);
    assert_eq!(outcome.resolved.shadowed[0].shadowed_by.scope, Scope::Local);
    assert_eq!(outcome.resolved.shadowed[0].shadowed_by.path, local_path);
}

#[test]
fn local_mcp_servers_and_rules_win_over_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    git_repository(project);
    let scope = local_scope(project);

    let mut local_mcp = definition(ItemKind::McpServer, "Docs", &scope.store().mcp_file());
    local_mcp.declared_id = Some("docs".to_owned());
    let mut project_mcp = definition(
        ItemKind::McpServer,
        "Docs",
        &project_store(project).join("mcp.json"),
    );
    project_mcp.declared_id = Some("docs".to_owned());

    let local_rule = definition(
        ItemKind::Rule,
        "Agents",
        &scope
            .item_path(ItemKind::Rule, "agents")
            .expect("local rule path"),
    );
    let project_rule = definition(
        ItemKind::Rule,
        "Agents",
        &project_store(project).join("rules/agents.md"),
    );

    let local =
        LocalLayer::new(scope, vec![local_mcp.clone(), local_rule.clone()]).expect("local layer");
    let outcome = resolve_with_local(
        &[layer(Scope::Project, vec![project_mcp, project_rule])],
        &local,
    );

    for local_definition in [&local_mcp, &local_rule] {
        let winner = outcome
            .resolved
            .get(&local_definition.logical_id())
            .expect("Local definition wins");
        assert_eq!(winner.scope, Scope::Local);
        assert_eq!(winner.definition.path, local_definition.path);
    }
    assert_eq!(outcome.resolved.shadowed.len(), 2);
}

#[test]
fn local_layer_is_skipped_and_flagged_without_a_git_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    let scope = local_scope(project);
    assert_eq!(
        scope.status(),
        LocalScopeStatus::NotAvailable {
            reason: LocalUnavailableReason::NotAGitRepository
        }
    );
    assert_eq!(
        scope.status().reason(),
        Some(LocalUnavailableReason::NotAGitRepository)
    );

    let local_path = scope
        .item_path(ItemKind::Skill, "code-review")
        .expect("local skill path")
        .join("SKILL.md");
    let local_definition = skill("Code Review", &local_path);
    let project_definition = skill(
        "Code Review",
        &project_store(project).join("skills/code-review/SKILL.md"),
    );
    let local = LocalLayer::new(scope, vec![local_definition.clone()])
        .expect("local layer is constructible");
    assert!(local.snapshot().is_none());

    // A Local snapshot smuggled into the base layers must not bypass the gate.
    let smuggled = layer(Scope::Local, vec![local_definition.clone()]);
    let outcome = resolve_with_local(
        &[
            layer(Scope::Project, vec![project_definition.clone()]),
            smuggled,
        ],
        &local,
    );

    assert_eq!(
        outcome.local.reason(),
        Some(LocalUnavailableReason::NotAGitRepository)
    );
    assert!(matches!(
        outcome.local,
        LocalScopeStatus::NotAvailable { .. }
    ));
    let winner = outcome
        .resolved
        .get(&project_definition.logical_id())
        .expect("project definition still resolves");
    assert_eq!(winner.scope, Scope::Project);
    assert_eq!(winner.definition.path, project_definition.path);
    assert!(outcome.resolved.shadowed.is_empty());
}

#[test]
fn local_layer_only_accepts_definitions_inside_the_local_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    let paths = app_paths(&dir.path().join("home"), project);
    let project_layout = StoreLayout::new(Scope::Project, project_store(project));
    let store = LocalStorePaths::from_project(&project_layout).expect("local layout");
    let scope = LocalScope::detect(project, store);

    let outside = paths.user_store().join("skills/demo/SKILL.md");
    let error = LocalLayer::new(scope.clone(), vec![skill("Demo", &outside)])
        .expect_err("user store definition is not Local");
    assert!(matches!(
        error,
        LocalScopeError::OutsideLocalStore { ref path, .. } if path == &outside
    ));

    let inside = scope
        .item_path(ItemKind::Skill, "demo")
        .expect("local skill path")
        .join("SKILL.md");
    assert!(LocalLayer::new(scope, vec![skill("Demo", &inside)]).is_ok());
}

#[test]
fn local_item_paths_never_leave_the_local_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    git_repository(project);
    let paths = app_paths(&dir.path().join("home"), project);
    let store = LocalStorePaths::for_paths(&paths).expect("local store for project");
    let scope = LocalScope::detect(project, store);

    for kind in [ItemKind::Skill, ItemKind::McpServer, ItemKind::Rule] {
        let path = scope.item_path(kind, "demo").expect("local item path");
        assert!(path.starts_with(scope.store().root()), "{path:?}");
        assert!(path.starts_with(paths.project_store().expect("project store")));
        assert!(!path.starts_with(paths.user_store()), "{path:?}");
        assert!(!path.starts_with(paths.global_store()), "{path:?}");
    }

    for slug in ["", "../outside", "a/b", ".", "..", "Upper", "tail.", "a b"] {
        let error = scope
            .item_path(ItemKind::Skill, slug)
            .expect_err("unsafe slug is refused");
        assert!(
            matches!(error, LocalScopeError::InvalidSlug { .. }),
            "{slug:?} produced {error:?}"
        );
    }
}

#[test]
fn ensure_gitignore_appends_exactly_one_line_and_then_does_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    fs::write(&gitignore, "target/\n").expect("seed gitignore");

    assert_eq!(
        ensure_gitignore(&gitignore).expect("append"),
        GitignoreChange::Appended
    );
    let text = fs::read_to_string(&gitignore).expect("read gitignore");
    assert_eq!(text, "target/\n.agentuna/local/\n");
    assert_eq!(text.matches(".agentuna/local/").count(), 1);

    assert_eq!(
        ensure_gitignore(&gitignore).expect("no-op"),
        GitignoreChange::Unchanged
    );
    assert_eq!(
        fs::read_to_string(&gitignore).expect("read gitignore"),
        text
    );
}

#[test]
fn ensure_gitignore_creates_a_missing_file_with_one_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    assert_eq!(
        ensure_gitignore(&gitignore).expect("create"),
        GitignoreChange::Created
    );
    assert_eq!(
        fs::read_to_string(&gitignore).expect("read gitignore"),
        ".agentuna/local/\n"
    );
    assert_eq!(
        ensure_gitignore(&gitignore).expect("no-op"),
        GitignoreChange::Unchanged
    );
}

#[test]
fn ensure_gitignore_accepts_every_equivalent_pattern() {
    let equivalents = [
        ".agentuna/local/",
        ".agentuna/local",
        "/.agentuna/local",
        "/.agentuna/local/",
        "**/.agentuna/local",
        "**/.agentuna/local/",
        ".agentuna/local/**",
        ".agentuna/local/*",
        ".agentuna/",
        "/.agentuna",
        "**/.agentuna",
    ];
    for pattern in equivalents {
        let dir = tempfile::tempdir().expect("tempdir");
        let gitignore = dir.path().join(".gitignore");
        let seeded = format!("# project ignores\n{pattern}\n");
        fs::write(&gitignore, &seeded).expect("seed gitignore");
        assert_eq!(
            ensure_gitignore(&gitignore).expect("check"),
            GitignoreChange::Unchanged,
            "pattern {pattern:?} should already ignore Local content"
        );
        assert_eq!(
            fs::read_to_string(&gitignore).expect("read gitignore"),
            seeded
        );
    }
}

#[test]
fn ensure_gitignore_does_not_treat_negation_or_unrelated_patterns_as_covering() {
    for pattern in [
        "!.agentuna/local/",
        "local/",
        ".agentuna/local2/",
        "agentuna",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let gitignore = dir.path().join(".gitignore");
        fs::write(&gitignore, format!("{pattern}\n")).expect("seed gitignore");
        assert_eq!(
            ensure_gitignore(&gitignore).expect("append"),
            GitignoreChange::Appended,
            "pattern {pattern:?} must not count as ignoring Local content"
        );
        let text = fs::read_to_string(&gitignore).expect("read gitignore");
        assert!(text.ends_with(".agentuna/local/\n"));
    }
}

#[test]
fn ensure_gitignore_preserves_line_endings_and_adds_a_separator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    fs::write(&gitignore, "node_modules").expect("seed gitignore");
    assert_eq!(
        ensure_gitignore(&gitignore).expect("append"),
        GitignoreChange::Appended
    );
    assert_eq!(
        fs::read_to_string(&gitignore).expect("read gitignore"),
        "node_modules\n.agentuna/local/\n"
    );

    let crlf = dir.path().join("crlf/.gitignore");
    fs::create_dir_all(crlf.parent().expect("parent")).expect("create dir");
    fs::write(&crlf, "target\r\n").expect("seed gitignore");
    assert_eq!(
        ensure_gitignore(&crlf).expect("append"),
        GitignoreChange::Appended
    );
    assert_eq!(
        fs::read_to_string(&crlf).expect("read gitignore"),
        "target\r\n.agentuna/local/\r\n"
    );
}

#[test]
fn ensure_gitignore_refuses_a_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real-ignore");
    fs::write(&real, "target/\n").expect("seed real file");
    let link = dir.path().join(".gitignore");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink");
    let error = ensure_gitignore(&link).expect_err("symlink is refused");
    assert!(matches!(error, LocalScopeError::SymlinkRefused { .. }));
    assert_eq!(
        fs::read_to_string(&real).expect("read real file"),
        "target/\n"
    );
}

#[test]
fn ensure_git_exclude_adds_each_deployed_path_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exclude = dir.path().join("exclude");
    fs::write(
        &exclude,
        "# managed by git\nexclude-marker\n/.claude/skills/alpha\n",
    )
    .expect("seed exclude");

    let change = ensure_git_exclude(
        &exclude,
        &[
            "/.claude/skills/alpha",
            "/.claude/skills/beta",
            "/.claude/skills/beta",
        ],
    )
    .expect("append exclude entries");
    assert!(!change.created);
    assert_eq!(change.added, vec!["/.claude/skills/beta".to_owned()]);
    assert_eq!(
        change.already_present,
        vec!["/.claude/skills/alpha".to_owned()]
    );
    assert!(change.wrote());

    let text = fs::read_to_string(&exclude).expect("read exclude");
    assert_eq!(text.matches("/.claude/skills/beta").count(), 1);
    assert_eq!(text.matches("/.claude/skills/alpha").count(), 1);

    let second = ensure_git_exclude(&exclude, &["/.claude/skills/alpha", "/.claude/skills/beta"])
        .expect("check exclude entries");
    assert!(!second.wrote());
    assert!(second.added.is_empty());
    assert_eq!(second.already_present.len(), 2);
    assert_eq!(fs::read_to_string(&exclude).expect("read exclude"), text);
}

#[test]
fn ensure_git_exclude_matches_directory_style_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exclude = dir.path().join("exclude");
    fs::write(&exclude, "/.claude/skills/alpha/\n").expect("seed exclude");
    let change =
        ensure_git_exclude(&exclude, &["/.claude/skills/alpha"]).expect("check exclude entries");
    assert!(!change.wrote());
    assert_eq!(
        change.already_present,
        vec!["/.claude/skills/alpha".to_owned()]
    );
}

#[test]
fn ensure_git_exclude_restores_a_pattern_a_negation_re_included() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exclude = dir.path().join("exclude");
    fs::write(&exclude, "/.claude/skills/alpha\n!/.claude/skills/alpha\n").expect("seed exclude");
    let change =
        ensure_git_exclude(&exclude, &["/.claude/skills/alpha"]).expect("append after negation");
    assert_eq!(change.added, vec!["/.claude/skills/alpha".to_owned()]);
    let text = fs::read_to_string(&exclude).expect("read exclude");
    assert_eq!(text.matches("/.claude/skills/alpha").count(), 3);
    assert!(text.ends_with("!/.claude/skills/alpha\n/.claude/skills/alpha\n"));
}

#[test]
fn ensure_git_exclude_creates_a_missing_file_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exclude = dir.path().join("exclude");
    let change = ensure_git_exclude(&exclude, &["/.claude/skills/alpha"]).expect("create exclude");
    assert!(change.created);
    assert_eq!(change.added, vec!["/.claude/skills/alpha".to_owned()]);
    assert_eq!(
        fs::read_to_string(&exclude).expect("read exclude"),
        "/.claude/skills/alpha\n"
    );

    let empty = dir.path().join("untouched");
    let nothing = ensure_git_exclude(&empty, &[]).expect("empty request");
    assert_eq!(nothing, ExcludeChange::default());
    assert!(!empty.exists());
}

#[test]
fn ensure_git_exclude_rejects_patterns_it_cannot_represent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exclude = dir.path().join("exclude");
    fs::write(&exclude, "").expect("seed exclude");
    for pattern in ["", "/", "../x", "a/../b", "a b", "!keep", "#comment"] {
        let error =
            ensure_git_exclude(&exclude, &[pattern]).expect_err("invalid pattern is refused");
        assert!(
            matches!(error, LocalScopeError::InvalidExcludePattern { .. }),
            "{pattern:?} produced {error:?}"
        );
    }
    assert_eq!(fs::read_to_string(&exclude).expect("read exclude"), "");
}

#[test]
fn register_deployments_writes_project_relative_patterns_into_the_exclude_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    git_repository(project);
    let scope = local_scope(project);
    let deployed = project.join(".claude/skills/local-skill");

    let change = scope
        .register_deployments(std::slice::from_ref(&deployed))
        .expect("register deployment");
    assert!(!change.created);
    assert_eq!(change.added, vec!["/.claude/skills/local-skill".to_owned()]);
    let exclude = scope.exclude_path().expect("exclude path");
    assert_eq!(
        fs::read_to_string(&exclude).expect("read exclude"),
        "/.claude/skills/local-skill\n"
    );

    let again = scope
        .register_deployments(std::slice::from_ref(&deployed))
        .expect("register deployment again");
    assert!(!again.wrote());
    assert_eq!(
        again.already_present,
        vec!["/.claude/skills/local-skill".to_owned()]
    );

    let outside = dir.path().parent().expect("parent").join("elsewhere/skill");
    let error = scope
        .register_deployments(std::slice::from_ref(&outside))
        .expect_err("outside project is refused");
    assert!(matches!(error, LocalScopeError::OutsideProject { .. }));

    let up = project.join(".claude/../skills/x");
    let error = deployment_pattern(project, &up).expect_err("traversal is refused");
    assert!(matches!(error, LocalScopeError::OutsideProject { .. }));
    let error = deployment_pattern(project, &project.join("../outside")).expect_err("traversal");
    assert!(matches!(error, LocalScopeError::OutsideProject { .. }));
    let error = deployment_pattern(project, Path::new("/elsewhere/skill"))
        .expect_err("absolute outside path");
    assert!(matches!(error, LocalScopeError::OutsideProject { .. }));
}

#[test]
fn register_deployments_refuses_without_a_git_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    let scope = local_scope(project);
    let error = scope
        .register_deployments(&[project.join(".claude/skills/local-skill")])
        .expect_err("Local is unavailable without Git");
    assert!(matches!(
        error,
        LocalScopeError::LocalNotAvailable {
            reason: LocalUnavailableReason::NotAGitRepository
        }
    ));
}

#[test]
fn detect_finds_the_work_tree_of_an_ancestor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git_repository(repo);
    let project = repo.join("nested/project");
    fs::create_dir_all(&project).expect("create project");

    let scope = local_scope(&project);
    assert!(scope.status().is_available());
    assert_eq!(scope.git_dir(), Some(repo.join(".git").as_path()));
    assert_eq!(scope.exclude_path(), Some(repo.join(".git/info/exclude")));
}

#[test]
fn detect_follows_a_linked_worktree_to_its_common_exclude() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    let worktree = dir.path().join("worktree");
    fs::create_dir_all(repo.join(".git/info")).expect("create repo git dir");
    fs::create_dir_all(repo.join(".git/worktrees/wt")).expect("create worktree metadata");
    fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").expect("write commondir");
    fs::create_dir_all(&worktree).expect("create worktree");
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
    )
    .expect("write git file");

    let scope = local_scope(&worktree);
    assert!(scope.status().is_available());
    assert_eq!(
        scope.git_dir(),
        Some(repo.join(".git/worktrees/wt").as_path())
    );
    assert_eq!(scope.exclude_path(), Some(repo.join(".git/info/exclude")));
}

#[test]
fn ensure_project_gitignore_uses_the_project_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    git_repository(project);
    let scope = local_scope(project);
    assert_eq!(
        scope.ensure_project_gitignore().expect("ensure gitignore"),
        GitignoreChange::Created
    );
    assert_eq!(
        fs::read_to_string(project.join(".gitignore")).expect("read gitignore"),
        ".agentuna/local/\n"
    );
}

#[test]
fn local_store_layout_is_the_local_directory_of_the_project_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path();
    let paths = app_paths(&dir.path().join("home"), project);
    let store = LocalStorePaths::for_paths(&paths).expect("local store");
    assert_eq!(
        store.root(),
        paths.project_store().expect("project store").join("local")
    );
    assert_eq!(store.store_file(), store.root().join("store.json"));
}
