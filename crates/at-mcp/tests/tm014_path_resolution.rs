//! TM-014 acceptance tests: login-shell PATH discovery, caching, fallback and command
//! lookup. Every test uses a fake shell and a temporary home; nothing here reads or
//! modifies the developer's real environment.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use at_mcp::path::{
    LOGIN_SHELL_TIMEOUT, ManagerRoots, ManualClock, PathResolver, PathSource, ResolveError,
    VersionManager, WELL_KNOWN_DIRS, default_search_dirs,
};
use tempfile::TempDir;

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture dir");
    }
    fs::write(path, body).expect("write fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

fn write_non_executable(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture dir");
    }
    fs::write(path, b"not executable").expect("write fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

/// A fake login shell that prints `path_list` and ignores its arguments.
fn fake_shell(dir: &Path, path_list: &str) -> PathBuf {
    let shell = dir.join("fake-shell");
    write_executable(&shell, &format!("#!/bin/sh\nprintf '%s' '{path_list}'\n"));
    shell
}

/// A fake login shell that prints banners before the PATH value.
fn noisy_fake_shell(dir: &Path, path_list: &str) -> PathBuf {
    let shell = dir.join("noisy-shell");
    write_executable(
        &shell,
        &format!(
            "#!/bin/sh\necho 'Welcome back'\necho 'profile loaded'\nprintf '%s' '{path_list}'\n"
        ),
    );
    shell
}

fn resolver_with_shell(shell: &Path, defaults: Vec<PathBuf>) -> PathResolver {
    PathResolver::builder()
        .shell(shell)
        .default_dirs(defaults)
        .manager_roots(ManagerRoots::default())
        .build()
}

#[test]
fn which_resolves_npx_from_the_login_shell_path() {
    let tmp = TempDir::new().expect("tempdir");
    let homebrew = tmp.path().join("opt/homebrew/bin");
    let npx = homebrew.join("npx");
    write_executable(&npx, "#!/bin/sh\nexit 0\n");
    let shell = fake_shell(tmp.path(), &homebrew.display().to_string());

    let resolver = resolver_with_shell(&shell, Vec::new());
    let location = resolver.which("npx").expect("npx resolves");

    assert_eq!(location.path, npx);
    assert_eq!(location.source, PathSource::LoginShell);
    assert_eq!(location.source.as_str(), "login_shell");
}

#[test]
fn login_banners_before_the_path_are_ignored() {
    let tmp = TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    write_executable(&bin.join("npx"), "#!/bin/sh\nexit 0\n");
    let shell = noisy_fake_shell(tmp.path(), &bin.display().to_string());

    let resolver = resolver_with_shell(&shell, Vec::new());
    let search = resolver.search_path();

    assert!(!search.degraded, "{search:?}");
    assert!(search.contains(&bin));
    assert_eq!(search.source, PathSource::LoginShell);
}

#[test]
fn hanging_shell_falls_back_to_defaults_after_two_seconds() {
    let tmp = TempDir::new().expect("tempdir");
    let fallback = tmp.path().join("fallback-bin");
    let shell = tmp.path().join("hanging-shell");
    write_executable(&shell, "#!/bin/sh\nexec sleep 30\n");

    let resolver = PathResolver::builder()
        .shell(&shell)
        .default_dirs(vec![fallback.clone()])
        .manager_roots(ManagerRoots::default())
        .build();

    let started = Instant::now();
    let search = resolver.search_path();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= LOGIN_SHELL_TIMEOUT,
        "returned after {elapsed:?}, before the {} timeout",
        LOGIN_SHELL_TIMEOUT.as_secs_f64()
    );
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    assert!(search.degraded);
    assert_eq!(search.source, PathSource::Defaults);
    assert_eq!(search.paths().collect::<Vec<_>>(), vec![fallback.as_path()]);
    let detail = search.detail.expect("degraded detail");
    assert!(detail.contains("2.0s"), "detail: {detail}");
}

#[test]
fn failing_and_silent_shells_fall_back_and_are_marked_degraded() {
    let tmp = TempDir::new().expect("tempdir");
    let fallback = tmp.path().join("fallback-bin");

    for (name, body) in [
        ("failing-shell", "#!/bin/sh\nexit 3\n"),
        ("silent-shell", "#!/bin/sh\nexit 0\n"),
    ] {
        let shell = tmp.path().join(name);
        write_executable(&shell, body);
        let resolver = PathResolver::builder()
            .shell(&shell)
            .default_dirs(vec![fallback.clone()])
            .manager_roots(ManagerRoots::default())
            .build();
        let search = resolver.search_path();
        assert!(search.degraded, "{name} should be degraded");
        assert_eq!(search.source, PathSource::Defaults);
        assert_eq!(search.paths().collect::<Vec<_>>(), vec![fallback.as_path()]);
        assert!(search.detail.is_some(), "{name} needs a reason");
    }
}

#[test]
fn output_larger_than_the_bound_is_degraded() {
    let tmp = TempDir::new().expect("tempdir");
    let fallback = tmp.path().join("fallback-bin");
    let shell = tmp.path().join("chatty-shell");
    write_executable(
        &shell,
        "#!/bin/sh\nhead -c 70000 /dev/zero | tr '\\0' 'a'\n",
    );

    let resolver = PathResolver::builder()
        .shell(&shell)
        .timeout(Duration::from_secs(1))
        .default_dirs(vec![fallback.clone()])
        .manager_roots(ManagerRoots::default())
        .build();
    let search = resolver.search_path();

    assert!(search.degraded);
    assert_eq!(search.paths().collect::<Vec<_>>(), vec![fallback.as_path()]);
    assert!(search.detail.is_some());
}

#[test]
fn default_directories_are_merged_after_the_login_shell_entries() {
    let tmp = TempDir::new().expect("tempdir");
    let login_bin = tmp.path().join("login-bin");
    fs::create_dir_all(&login_bin).expect("create login bin");
    let shell = fake_shell(tmp.path(), &login_bin.display().to_string());
    let defaults = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        login_bin.clone(),
    ];

    let resolver = resolver_with_shell(&shell, defaults);
    let search = resolver.search_path();

    let expected: Vec<PathBuf> = vec![
        login_bin.clone(),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    assert_eq!(search.paths().collect::<Vec<_>>(), expected);
    assert_eq!(search.dirs[0].source, PathSource::LoginShell);
    assert_eq!(search.dirs[1].source, PathSource::Defaults);
    assert!(!search.degraded);
    assert_eq!(
        default_search_dirs(),
        WELL_KNOWN_DIRS
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    );
}

#[test]
fn non_executable_candidates_are_distinct_from_missing_commands() {
    let tmp = TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    let npx = bin.join("npx");
    write_non_executable(&npx);
    let shell = fake_shell(tmp.path(), &bin.display().to_string());

    let resolver = resolver_with_shell(&shell, Vec::new());
    match resolver.which("npx") {
        Err(ResolveError::NotExecutable { path, .. }) => assert_eq!(path, npx),
        other => panic!("expected NotExecutable, got {other:?}"),
    }
    match resolver.which("definitely-missing-tool") {
        Err(ResolveError::NotFound { hint, .. }) => assert!(hint.is_none()),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_ne!(
        resolver.which("npx").unwrap_err().to_string(),
        resolver
            .which("definitely-missing-tool")
            .unwrap_err()
            .to_string()
    );
}

#[test]
fn nvm_layout_produces_a_hint_naming_the_version_directory() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    for version in ["v9.11.1", "v18.20.0", "v20.11.1"] {
        write_executable(
            &home
                .join(".nvm/versions/node")
                .join(version)
                .join("bin/npx"),
            "#!/bin/sh\nexit 0\n",
        );
    }
    let empty = tmp.path().join("empty-bin");
    fs::create_dir_all(&empty).expect("create empty bin");
    let shell = fake_shell(tmp.path(), &empty.display().to_string());

    let resolver = PathResolver::builder()
        .shell(&shell)
        .home(&home)
        .manager_roots(ManagerRoots::under_home(&home))
        .default_dirs(Vec::new())
        .build();

    let error = resolver
        .which("npx")
        .expect_err("npx is not on the fake PATH");
    let hint = error.hint().expect("an nvm hint");
    let version_dir = home.join(".nvm/versions/node/v20.11.1");

    assert_eq!(hint.manager, VersionManager::Nvm);
    assert_eq!(hint.manager.as_str(), "nvm");
    assert_eq!(hint.version_dir.as_deref(), Some(version_dir.as_path()));
    assert_eq!(hint.executable, version_dir.join("bin/npx"));
    assert!(hint.message.contains("nvm"), "message: {}", hint.message);
    assert!(
        hint.message.contains(&version_dir.display().to_string()),
        "message: {}",
        hint.message
    );
}

#[test]
fn every_known_version_manager_layout_is_detected() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    let roots = ManagerRoots::under_home(&home);
    let resolver = PathResolver::builder()
        .home(&home)
        .manager_roots(roots)
        .default_dirs(Vec::new())
        .shell(fake_shell(tmp.path(), ""))
        .build();

    let cases = [
        (
            VersionManager::Nvm,
            home.join(".nvm/versions/node/v20/bin/nvm-tool"),
            Some(home.join(".nvm/versions/node/v20")),
            "nvm",
        ),
        (
            VersionManager::Fnm,
            home.join(".local/share/fnm/node-versions/v20/installation/bin/fnm-tool"),
            Some(home.join(".local/share/fnm/node-versions/v20")),
            "fnm",
        ),
        (
            VersionManager::Volta,
            home.join(".volta/bin/volta-tool"),
            None,
            "volta",
        ),
        (
            VersionManager::Asdf,
            home.join(".asdf/installs/nodejs/20.11.1/bin/asdf-tool"),
            Some(home.join(".asdf/installs/nodejs/20.11.1")),
            "asdf",
        ),
        (
            VersionManager::Mise,
            home.join(".local/share/mise/installs/node/20.11.1/bin/mise-tool"),
            Some(home.join(".local/share/mise/installs/node/20.11.1")),
            "mise",
        ),
    ];

    for (manager, executable, version_dir, name) in cases {
        write_executable(&executable, "#!/bin/sh\nexit 0\n");
        let command = executable
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture name");
        let hint = resolver.manager_hint(command).expect("layout detected");
        assert_eq!(hint.manager, manager);
        assert_eq!(hint.executable, executable);
        assert_eq!(hint.version_dir, version_dir);
        assert!(hint.message.contains(name), "message: {}", hint.message);
    }
}

#[test]
fn explicit_paths_resolve_without_a_path_search() {
    let tmp = TempDir::new().expect("tempdir");
    let tool = tmp.path().join("tools/explicit");
    write_executable(&tool, "#!/bin/sh\nexit 0\n");
    let shell = fake_shell(tmp.path(), "");
    let resolver = resolver_with_shell(&shell, Vec::new());

    let location = resolver
        .which(tool.to_str().expect("utf-8 fixture path"))
        .expect("explicit path resolves");
    assert_eq!(location.path, tool);
    assert_eq!(location.source, PathSource::Explicit);

    let missing = tmp.path().join("tools/missing");
    match resolver.which(missing.to_str().expect("utf-8 fixture path")) {
        Err(ResolveError::NotFound { searched, .. }) => {
            assert_eq!(searched, vec![missing]);
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn spawn_uses_the_resolved_absolute_path() {
    let tmp = TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    write_executable(&bin.join("fake-mcp"), "#!/bin/sh\necho ok\n");
    let shell = fake_shell(tmp.path(), &bin.display().to_string());
    let resolver = resolver_with_shell(&shell, Vec::new());

    let output = resolver
        .command("fake-mcp")
        .expect("command resolves")
        .output()
        .expect("fixture runs");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
}

#[test]
fn empty_or_nul_commands_are_rejected_before_any_search() {
    let tmp = TempDir::new().expect("tempdir");
    let shell = fake_shell(tmp.path(), "");
    let resolver = resolver_with_shell(&shell, Vec::new());

    match resolver.which("") {
        Err(ResolveError::InvalidCommand { reason, .. }) => assert_eq!(reason, "empty"),
        other => panic!("expected InvalidCommand, got {other:?}"),
    }
    match resolver.which("bad\0name") {
        Err(ResolveError::InvalidCommand { reason, .. }) => {
            assert!(reason.contains("NUL"), "reason: {reason}");
        }
        other => panic!("expected InvalidCommand, got {other:?}"),
    }
}

#[test]
fn repeated_lookups_reuse_the_cached_search_path_for_sixty_seconds() {
    let tmp = TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    write_executable(&bin.join("npx"), "#!/bin/sh\nexit 0\n");
    let counter = tmp.path().join("probe-count.log");
    let shell = tmp.path().join("counting-shell");
    write_executable(
        &shell,
        &format!(
            "#!/bin/sh\nprintf 'x\\n' >> '{}'\nprintf '%s' '{}'\n",
            counter.display(),
            bin.display()
        ),
    );

    let clock = Arc::new(ManualClock::new());
    let resolver = PathResolver::builder()
        .shell(&shell)
        .default_dirs(Vec::new())
        .manager_roots(ManagerRoots::default())
        .clock(clock.clone())
        .build();

    let probes = || {
        fs::read_to_string(&counter)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };

    resolver.which("npx").expect("first lookup");
    resolver.which("npx").expect("second lookup");
    assert_eq!(
        probes(),
        1,
        "the second lookup must not fork the login shell"
    );

    clock.advance(Duration::from_secs(59));
    resolver.which("npx").expect("still cached");
    assert_eq!(probes(), 1);

    clock.advance(Duration::from_secs(2));
    resolver.which("npx").expect("cache expired");
    assert_eq!(probes(), 2);

    resolver.invalidate();
    resolver.which("npx").expect("invalidated");
    assert_eq!(probes(), 3);
}
