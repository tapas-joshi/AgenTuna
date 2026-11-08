//! Acceptance tests for TU-012: project detection.
//!
//! Fixtures are temporary trees built per test; nothing outside the sandbox is read or written
//! and no file inside a fixture is ever executed.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use at_db::projects::{ProjectRegistry, ProjectRegistryError};
use at_store::project_scan::{
    CancelFlag, PROJECT_MARKERS, ProjectDiff, ProjectMarker, SKIPPED_DIR_NAMES, ScanConfig,
    ScanObserver, ScanProgress, ScanWarningKind, apply_diff, scan, scan_with, spawn_scan,
};
use tempfile::TempDir;

/// Creates `<root>/<relative>` and returns it.
fn dir(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(&path).expect("fixture directory");
    path
}

/// Writes an empty marker file at `<root>/<relative>`.
fn marker(root: &Path, relative: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("marker parent");
    }
    fs::write(path, b"").expect("marker file");
}

fn canonical(root: &Path) -> PathBuf {
    fs::canonicalize(root).expect("canonical fixture path")
}

/// Names of the scanned projects relative to `root`, sorted.
fn names(outcome: &at_store::project_scan::ScanOutcome, root: &Path) -> Vec<String> {
    let root = canonical(root);
    let mut names: Vec<String> = outcome
        .projects
        .iter()
        .map(|project| {
            project
                .path
                .strip_prefix(&root)
                .expect("project under root")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The standard fixture: three projects at different depths plus everything that must be
/// skipped.
fn fixture(root: &Path) {
    // Depth 1 project, with a nested project inside it that must not be listed separately.
    marker(root, "alpha/.git/HEAD");
    marker(root, "alpha/nested/.agentuna/store.json");

    // Depth 3 project signalled through Claude Code configuration.
    marker(root, "beta/gamma/delta/.claude/settings.json");

    // A project signalled only by an MCP file.
    marker(root, "epsilon/.mcp.json");

    // Heavy directories with projects inside them: all skipped.
    marker(root, "node_modules/pkg/.git/HEAD");
    marker(root, "target/debug/.git/HEAD");
    marker(root, ".venv/lib/.git/HEAD");
    marker(root, "dist/.git/HEAD");
    dir(root, ".hidden-repo/.git");
    dir(root, "plain/src");
    fs::write(root.join("plain/README.md"), b"# plain\n").expect("readme");
}

#[test]
fn a_three_level_tree_yields_exactly_the_expected_projects() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("Projects");
    fs::create_dir_all(&root).expect("root");
    fixture(&root);

    let config = ScanConfig::new([root.clone()]);
    assert_eq!(
        config.max_depth(),
        at_store::project_scan::DEFAULT_MAX_DEPTH
    );
    let outcome = scan(&config);

    assert!(outcome.is_complete());
    assert!(
        outcome.warnings.is_empty(),
        "a clean fixture produces no warnings: {:?}",
        outcome.warnings
    );
    assert_eq!(
        names(&outcome, &root),
        vec![
            "alpha".to_owned(),
            "beta/gamma/delta".to_owned(),
            "epsilon".to_owned()
        ],
        "nested repositories collapse into the shallowest match and heavy directories disappear"
    );
    assert!(
        !names(&outcome, &root)
            .iter()
            .any(|name| name.starts_with(".hidden")),
        "hidden directories are not scanned"
    );
    assert!(outcome.skipped_directories >= 5);

    let alpha = outcome
        .projects
        .iter()
        .find(|project| project.name == "alpha")
        .expect("alpha");
    assert_eq!(alpha.depth, 1);
    assert_eq!(alpha.markers, vec![ProjectMarker::Git]);
    assert_eq!(alpha.source_root, canonical(&root));

    let delta = outcome
        .projects
        .iter()
        .find(|project| project.name == "delta")
        .expect("delta");
    assert_eq!(delta.depth, 3);
    assert_eq!(delta.markers, vec![ProjectMarker::ClaudeCode]);
}

#[test]
fn every_marker_counts_and_markers_are_reported_together() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    for (index, marker_name) in PROJECT_MARKERS.iter().enumerate() {
        let project = format!("p{index}");
        marker(&root, &format!("{project}/{marker_name}"));
    }
    let outcome = scan(&ScanConfig::new([root.clone()]));
    assert_eq!(outcome.projects.len(), PROJECT_MARKERS.len());

    // One folder with two markers lists both, in contract order.
    let both = dir(&root, "both");
    marker(&both, ".git/HEAD");
    marker(&both, ".agents/skills/x/SKILL.md");
    let outcome = scan(&ScanConfig::new([root.clone()]));
    let project = outcome
        .projects
        .iter()
        .find(|project| project.name == "both")
        .expect("both");
    assert_eq!(
        project.markers,
        vec![ProjectMarker::Git, ProjectMarker::Agents]
    );
}

#[test]
fn a_nested_repository_is_listed_once_and_an_overlapping_root_does_not_duplicate_it() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "outer/.git/HEAD");
    marker(&root, "outer/inner/.git/HEAD");

    let outcome = scan(&ScanConfig::new([root.clone()]));
    assert_eq!(names(&outcome, &root), vec!["outer".to_owned()]);

    // `outer` is also configured as a root; the project must not appear twice.
    let outcome = scan(&ScanConfig::new([root.clone(), root.join("outer")]));
    assert_eq!(names(&outcome, &root), vec!["outer".to_owned()]);
}

#[test]
fn the_depth_bound_is_respected_and_reported() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "a/b/c/deep/.git/HEAD");

    let deep = scan(&ScanConfig::new([root.clone()]).with_max_depth(4));
    assert_eq!(names(&deep, &root), vec!["a/b/c/deep".to_owned()]);
    assert_eq!(deep.depth_limited, 0);

    let shallow = scan(&ScanConfig::new([root.clone()]).with_max_depth(2));
    assert!(shallow.projects.is_empty());
    assert!(shallow.depth_limited > 0, "the bound was hit");
}

#[test]
fn a_symlink_never_causes_a_cycle_and_a_symlinked_root_still_scans() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "project/.git/HEAD");
    symlink(&root, root.join("loop")).expect("cycle symlink");
    symlink(root.join("project"), root.join("alias")).expect("directory symlink");

    let outcome = scan(&ScanConfig::new([root.clone()]));
    assert_eq!(
        names(&outcome, &root),
        vec!["project".to_owned()],
        "the symlinks are skipped, so the walk terminates and nothing is duplicated"
    );
    assert!(outcome.skipped_symlinks >= 2);

    // A root reached through a symlink is resolved once and scanned normally.
    let outcome = scan(&ScanConfig::new([root.join("loop")]));
    assert_eq!(names(&outcome, &root), vec!["project".to_owned()]);
}

#[test]
fn permission_denied_and_missing_roots_become_warnings() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "visible/.git/HEAD");
    let blocked = dir(&root, "blocked");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).expect("chmod");

    let missing = temp.path().join("does-not-exist");
    let file = temp.path().join("not-a-directory");
    fs::write(&file, b"x").expect("file");

    let outcome = scan(&ScanConfig::new([
        root.clone(),
        missing.clone(),
        file.clone(),
    ]));

    let kinds: BTreeSet<ScanWarningKind> = outcome.warnings.iter().map(|w| w.kind).collect();
    assert!(
        kinds.contains(&ScanWarningKind::PermissionDenied),
        "unreadable directories are reported: {:?}",
        outcome.warnings
    );
    assert!(kinds.contains(&ScanWarningKind::MissingRoot));
    assert!(
        kinds.contains(&ScanWarningKind::NotADirectory),
        "a file configured as a root is reported"
    );
    let blocked_name = canonical(&blocked);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.path == blocked_name),
        "the blocked directory is named: {:?}",
        outcome.warnings
    );
    assert_eq!(
        names(&outcome, &root),
        vec!["visible".to_owned()],
        "the scan continues past every problem"
    );

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("restore mode");
}

/// Records progress reports and can cancel itself.
#[derive(Default)]
struct Recorder {
    reports: Mutex<Vec<ScanProgress>>,
    cancel_after: Option<usize>,
    cancelled: AtomicBool,
}

impl ScanObserver for Recorder {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn progress(&self, progress: &ScanProgress) {
        let reports = self
            .reports
            .lock()
            .map(|reports| reports.len())
            .unwrap_or_default();
        if let Ok(mut reports) = self.reports.lock() {
            reports.push(progress.clone());
        }
        if let Some(limit) = self.cancel_after
            && reports + 1 >= limit
        {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn progress_is_reported_every_hundred_entries_by_default_and_every_configured_count() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    for index in 0..60 {
        dir(&root, &format!("dir{index:02}/child"));
    }
    // 60 top-level directories, each holding one child: 120 entries, so one report at 100.
    let recorder = Recorder::default();
    let outcome = scan_with(&ScanConfig::new([root.clone()]), &recorder);
    assert!(!outcome.cancelled);
    let reports = recorder.reports.lock().expect("reports");
    assert!(
        !reports.is_empty(),
        "120 entries produce a progress report at the 100th"
    );
    assert!(
        reports
            .iter()
            .all(|report| report.visited.is_multiple_of(100)),
        "reports land on the interval boundary"
    );
    drop(reports);

    let recorder = Recorder::default();
    let outcome = scan_with(
        &ScanConfig::new([root.clone()]).with_progress_every(7),
        &recorder,
    );
    assert!(!outcome.cancelled);
    let reports = recorder.reports.lock().expect("reports");
    assert!(!reports.is_empty());
    for report in reports.iter() {
        assert!(report.visited.is_multiple_of(7));
    }
    assert!(
        reports.last().expect("a report").visited <= outcome.visited,
        "no report claims more entries than were visited"
    );
}

#[test]
fn a_cancel_flag_stops_the_scan_early() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    for index in 0..30 {
        marker(&root, &format!("project{index:02}/.git/HEAD"));
    }

    let recorder = Recorder {
        cancel_after: Some(1),
        ..Recorder::default()
    };
    let config = ScanConfig::new([root.clone()]).with_progress_every(3);
    let outcome = scan_with(&config, &recorder);
    assert!(outcome.cancelled, "the observer cancelled the walk");
    assert!(
        outcome.projects.len() < 30,
        "the scan stopped before finishing: {} projects",
        outcome.projects.len()
    );

    // A pre-cancelled flag stops before anything is read.
    let flag = CancelFlag::new();
    flag.cancel();
    let outcome = scan_with(&config, &flag);
    assert!(outcome.cancelled);
    assert_eq!(outcome.visited, 0);
    assert!(outcome.projects.is_empty());

    let full = scan(&config);
    assert_eq!(
        full.projects.len(),
        30,
        "an uncancelled scan finds them all"
    );
}

#[test]
fn the_diff_separates_new_unchanged_and_missing_projects() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "alpha/.git/HEAD");
    marker(&root, "beta/.git/HEAD");
    let outcome = scan(&ScanConfig::new([root.clone()]));
    let alpha = canonical(&root.join("alpha"));
    let beta = canonical(&root.join("beta"));
    let stale = temp.path().join("gone");

    let diff = ProjectDiff::between(&outcome.projects, &[alpha.clone(), stale.clone()]);
    assert_eq!(
        diff.new
            .iter()
            .map(|project| project.path.clone())
            .collect::<Vec<_>>(),
        vec![beta.clone()]
    );
    assert_eq!(
        diff.unchanged
            .iter()
            .map(|project| project.path.clone())
            .collect::<Vec<_>>(),
        vec![alpha.clone()]
    );
    assert_eq!(diff.missing, vec![stale]);
    assert!(!diff.is_empty());

    let current: Vec<PathBuf> = outcome
        .projects
        .iter()
        .map(|project| project.path.clone())
        .collect();
    let diff = ProjectDiff::between(&outcome.projects, &current);
    assert!(diff.is_empty(), "a matching registry needs no upsert");
    assert_eq!(diff.unchanged.len(), 2);
}

/// One row of a fake registry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    last_used_at: String,
    source_root: PathBuf,
}

/// Records what [`apply_diff`] asked it to do.
#[derive(Default)]
struct FakeRegistry {
    rows: Vec<(PathBuf, Row)>,
    missing: Vec<PathBuf>,
    calls: Vec<String>,
}

impl FakeRegistry {
    fn row(&self, path: &Path) -> Option<&Row> {
        self.rows
            .iter()
            .find(|(row_path, _)| row_path == path)
            .map(|(_, row)| row)
    }
}

impl ProjectRegistry for FakeRegistry {
    fn insert_discovered(
        &mut self,
        path: &Path,
        _name: &str,
        source_root: &Path,
    ) -> Result<(), ProjectRegistryError> {
        self.calls.push(format!("insert {}", path.display()));
        // A real upsert must not move an existing row's timestamps.
        if let Some((_, row)) = self.rows.iter_mut().find(|(row_path, _)| row_path == path) {
            row.source_root = source_root.to_path_buf();
            return Ok(());
        }
        self.rows.push((
            path.to_path_buf(),
            Row {
                last_used_at: "scan:never".to_owned(),
                source_root: source_root.to_path_buf(),
            },
        ));
        Ok(())
    }

    fn mark_missing(&mut self, path: &Path) -> Result<(), ProjectRegistryError> {
        self.calls.push(format!("missing {}", path.display()));
        self.missing.push(path.to_path_buf());
        Ok(())
    }
}

#[test]
fn applying_a_diff_never_touches_an_existing_projects_last_used_at() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "alpha/.git/HEAD");
    marker(&root, "beta/.git/HEAD");
    let stale = temp.path().join("removed");
    let outcome = scan(&ScanConfig::new([root.clone()]));
    let alpha = canonical(&root.join("alpha"));

    let mut registry = FakeRegistry {
        rows: vec![(
            alpha.clone(),
            Row {
                last_used_at: "2026-01-02T03:04:05Z".to_owned(),
                source_root: root.clone(),
            },
        )],
        missing: Vec::new(),
        calls: Vec::new(),
    };
    let diff = ProjectDiff::between(&outcome.projects, &[alpha.clone(), stale.clone()]);
    let applied = apply_diff(&diff, &mut registry).expect("apply");
    assert_eq!(applied, 2, "one insert and one missing marker");

    let row = registry.row(&alpha).expect("alpha row");
    assert_eq!(
        row.last_used_at, "2026-01-02T03:04:05Z",
        "an unchanged project keeps its timestamp"
    );
    assert!(
        !registry.calls.iter().any(|call| call.contains("alpha")),
        "an unchanged project is never written: {:?}",
        registry.calls
    );
    assert_eq!(registry.rows.len(), 2, "beta was inserted");
    assert_eq!(registry.missing, vec![stale]);

    // A second scan with the same diff touches nothing that is already correct.
    let known: Vec<PathBuf> = registry.rows.iter().map(|(path, _)| path.clone()).collect();
    let diff = ProjectDiff::between(&outcome.projects, &known);
    assert!(diff.new.is_empty());
    let before = registry.row(&alpha).cloned();
    let applied = apply_diff(&diff, &mut registry).expect("apply again");
    assert_eq!(applied, 0);
    assert_eq!(registry.row(&alpha).cloned(), before);
}

#[test]
fn applying_a_diff_with_real_database_never_touches_existing_projects_last_used_at() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    marker(&root, "alpha/.git/HEAD");
    marker(&root, "beta/.git/HEAD");
    let stale = temp.path().join("removed");
    let outcome = scan(&ScanConfig::new([root.clone()]));
    let alpha = canonical(&root.join("alpha"));

    let mut db = at_db::Database::open_in_memory().expect("open db");
    at_db::projects::add_project(db.connection(), &alpha, None, Some(&root)).expect("add alpha");
    let alpha_summary = at_db::projects::find_project_by_path(db.connection(), &alpha)
        .expect("find")
        .expect("alpha exists");
    at_db::projects::touch_project(db.connection(), &alpha_summary.id).expect("touch");
    let before_touch = at_db::projects::find_project_by_id(db.connection(), &alpha_summary.id)
        .expect("find")
        .expect("alpha exists");
    let before_last_used_at = before_touch.last_used_at.expect("has timestamp");

    let diff = ProjectDiff::between(&outcome.projects, &[alpha.clone(), stale.clone()]);
    let applied = apply_diff(&diff, &mut db).expect("apply diff");
    assert_eq!(applied, 2, "inserted beta, marked stale missing");

    let after = at_db::projects::find_project_by_id(db.connection(), &alpha_summary.id)
        .expect("find")
        .expect("alpha exists");
    assert_eq!(
        after.last_used_at.as_deref(),
        Some(before_last_used_at.as_str()),
        "existing project's last_used_at was not touched by apply_diff"
    );
}
#[test]
fn the_scan_runs_as_a_tokio_job() {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().join("root");
    fixture(&root);
    let config = ScanConfig::new([root.clone()]);
    let expected = scan(&config);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let flag = Arc::new(CancelFlag::new());
    let outcome = runtime
        .block_on(async move { spawn_scan(config, flag).await })
        .expect("scan job");
    assert_eq!(outcome.projects, expected.projects);
    assert_eq!(outcome.warnings, expected.warnings);
}

#[test]
fn the_skipped_directory_list_covers_the_documented_heavy_names() {
    for name in ["node_modules", "target", "dist", "venv"] {
        assert!(
            SKIPPED_DIR_NAMES.contains(&name),
            "{name} must be a documented skip"
        );
    }
}
