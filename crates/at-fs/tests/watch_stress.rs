//! Startup cost of the watch service on large, symlink-heavy agent layouts.
//!
//! Skill-manager tools install skills as symlinks into agent roots, so a real machine
//! presents a few hundred link entries across several roots, with the content living in
//! central libraries elsewhere in the home directory. These tests reproduce that shape
//! synthetically and require the service to report *started* without indexing every
//! linked tree first: seeding happens in the background after the platform watches are
//! installed.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use at_fs::{WatchConfig, WatchRoot, WatchRootKind, WatchService};
use tempfile::TempDir;

/// Entries (symlinked skill links) per skills root. Four roots multiply this to the
/// 60 / 240 / 1000 total-entry shapes measured against the reported regression.
const ENTRIES_PER_ROOT: [usize; 3] = [15, 60, 250];
const ROOT_COUNT: usize = 4;
const DANGLING_LINKS: usize = 13;
/// Library skills that carry a bulk directory of small files (the "big library skill" shape).
const BULK_SKILL_STEP: usize = 10;
const BULK_FILES: usize = 1_000;

/// A synthetic agent home: a central library, four skills roots linking into it, dangling
/// links, MCP configuration files and the AgenTuna store directory.
struct Fixture {
    library: PathBuf,
    _home: TempDir,
    home: PathBuf,
}

/// Creates one library skill with nested content and, for every tenth skill, a bulk
/// directory of one thousand small files.
fn write_library_skill(library: &Path, skill: usize) {
    let skill_dir = library.join(format!("skill-{skill:04}"));
    fs::create_dir_all(skill_dir.join("references")).expect("skill references");
    fs::create_dir_all(skill_dir.join("scripts")).expect("skill scripts");
    fs::write(skill_dir.join("SKILL.md"), "# skill\n").expect("skill doc");
    let files = 5 + (skill % 36);
    for file in 0..files {
        fs::write(
            skill_dir.join("references").join(format!("ref-{file}.md")),
            format!("reference {file}\n"),
        )
        .expect("reference file");
    }
    fs::write(skill_dir.join("scripts").join("run.sh"), "#!/bin/sh\n").expect("script");
    if skill.is_multiple_of(BULK_SKILL_STEP) {
        let bulk = skill_dir.join("assets").join("bulk");
        fs::create_dir_all(&bulk).expect("bulk dir");
        for file in 0..BULK_FILES {
            fs::write(bulk.join(format!("part-{file:04}.txt")), "x".repeat(64)).expect("bulk file");
        }
    }
}

#[cfg(unix)]
fn link(source: &Path, at: &Path) {
    std::os::unix::fs::symlink(source, at).expect("symlink");
}

impl Fixture {
    /// Builds the fixture for one entry count. Skills live in a library outside the watched
    /// roots, exactly like `~/.skills-manager/skills` does on a real machine.
    fn build(entries_per_root: usize) -> Self {
        let _home = TempDir::new().expect("temp home");
        let home = fs::canonicalize(_home.path()).expect("canonical temp home");
        let library = home.join("library").join("skills");
        let skills = entries_per_root * ROOT_COUNT;
        for skill in 0..skills {
            write_library_skill(&library, skill);
        }

        let mut roots = Vec::new();
        for root_index in 0..ROOT_COUNT {
            let root = home.join(format!("agent-{root_index}")).join("skills");
            fs::create_dir_all(&root).expect("skills root");
            for skill in 0..entries_per_root {
                let global = skill + root_index * entries_per_root;
                link(
                    &library.join(format!("skill-{global:04}")),
                    &root.join(format!("skill-{skill:04}")),
                );
            }
            if root_index == 0 {
                for dangling in 0..DANGLING_LINKS {
                    link(
                        &library.join(format!("superset-missing-{dangling}")),
                        &root.join(format!("superset-{dangling}")),
                    );
                }
            }
            roots.push(root);
        }

        // MCP configuration files, as the adapters report them.
        fs::write(home.join(".claude.json"), "{\"mcpServers\":{}}").expect("mcp file");
        let codex = home.join(".codex").join("config.toml");
        fs::create_dir_all(codex.parent().expect("codex dir")).expect("codex root");
        fs::write(&codex, "[mcp_servers]\n").expect("codex mcp");

        // AgenTuna store directory.
        let store = home.join(".config-studio");
        fs::create_dir_all(store.join("skills")).expect("store");
        fs::write(store.join("settings.json"), "{}").expect("store settings");

        Self {
            library,
            _home,
            home,
        }
    }

    /// The skills roots, as the adapters report them.
    fn skills_roots(&self) -> Vec<PathBuf> {
        (0..ROOT_COUNT)
            .map(|root_index| self.home.join(format!("agent-{root_index}")).join("skills"))
            .collect()
    }

    /// The watch roots the application would build for this layout (TA-081 shape).
    fn watch_roots(&self) -> Vec<WatchRoot> {
        let mut roots = vec![WatchRoot::dir(
            self.home.join(".config-studio"),
            WatchRootKind::UserStore,
        )];
        for root in self.skills_roots() {
            roots.push(WatchRoot::dir(root, WatchRootKind::SkillsDir));
        }
        roots.push(WatchRoot::file(
            self.home.join(".claude.json"),
            WatchRootKind::McpFile,
        ));
        roots.push(WatchRoot::file(
            self.home.join(".codex").join("config.toml"),
            WatchRootKind::McpFile,
        ));
        roots
    }
}

#[test]
fn start_completes_within_bound_for_large_symlinked_setups() {
    for entries_per_root in ENTRIES_PER_ROOT {
        let fixture = Fixture::build(entries_per_root);
        let total = entries_per_root * ROOT_COUNT;
        // Let the fixture settle so platform event replay does not distort the measurement.
        std::thread::sleep(Duration::from_millis(1_500));
        let service = WatchService::new(
            WatchConfig::new(fixture.watch_roots()).with_allowed_link_roots([fixture.home.clone()]),
        );
        let began = Instant::now();
        let outcome = service.start();
        let elapsed = began.elapsed();
        println!("entries={total} per_root={entries_per_root} start={elapsed:?}");
        outcome.expect("the watcher must start");
        assert!(
            elapsed < Duration::from_secs(4),
            "start with {total} entries took {elapsed:?}; the start budget must never be \
             spent indexing content",
        );
        let status = wait_for_status(&service, Duration::from_secs(30), |status| {
            !matches!(status, at_fs::WatchStatus::Starting)
        })
        .expect("the watcher reports a terminal condition");
        assert!(
            matches!(
                status,
                at_fs::WatchStatus::Watching | at_fs::WatchStatus::Degraded(_)
            ),
            "a large symlinked setup must never end up unavailable: {status:?}",
        );
        service.stop().expect("stop");
    }
}

/// Waits until `predicate` accepts the service condition.
fn wait_for_status(
    service: &WatchService,
    timeout: Duration,
    predicate: impl Fn(&at_fs::WatchStatus) -> bool,
) -> Option<at_fs::WatchStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = service.status();
        if predicate(&status) {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits for the next batch, skipping other event kinds.
fn next_batch(service: &WatchService, timeout: Duration) -> Option<at_fs::WatchBatch> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match service.next_event(remaining) {
            Some(at_fs::WatchEvent::Batch(batch)) => return Some(batch),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// Drains events for `window` so fixture-replay noise does not leak into the next assertion.
fn drain(service: &WatchService, window: Duration) {
    let deadline = Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if service.next_event(remaining).is_none() {
            break;
        }
    }
}

#[test]
fn a_change_inside_a_linked_skill_is_attributed_to_the_link_path() {
    let fixture = Fixture::build(15);
    // Let the fixture settle so platform event replay does not pollute the assertion.
    std::thread::sleep(Duration::from_millis(2_500));
    let service = WatchService::new(
        WatchConfig::new(fixture.watch_roots()).with_allowed_link_roots([fixture.home.clone()]),
    );
    service.start().expect("the watcher must start");
    wait_for_status(&service, Duration::from_secs(30), |status| {
        !matches!(status, at_fs::WatchStatus::Starting)
    })
    .expect("the watcher settles");
    drain(&service, Duration::from_secs(1));

    let target = fixture
        .library
        .join("skill-0003")
        .join("references")
        .join("ref-0.md");
    fs::write(&target, "edited through the library\n").expect("edit linked skill content");

    let link_path = fixture.skills_roots()[0]
        .join("skill-0003")
        .join("references")
        .join("ref-0.md");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("a batch");
    assert!(
        batch.paths.iter().any(|path| path == &link_path),
        "a change inside the linked skill is attributed to the link path: {:?}",
        batch.paths
    );
    // The same library skill is not linked from the other roots, so only the owning link is
    // attributed.
    let other = fixture.skills_roots()[1].join("skill-0003");
    assert!(
        !batch.paths.iter().any(|path| path.starts_with(&other)),
        "unrelated links are not attributed: {:?}",
        batch.paths
    );
    service.stop().expect("stop");
}

#[test]
fn a_link_created_after_start_watches_its_target() {
    let fixture = Fixture::build(15);
    std::thread::sleep(Duration::from_millis(2_500));
    let service = WatchService::new(
        WatchConfig::new(fixture.watch_roots()).with_allowed_link_roots([fixture.home.clone()]),
    );
    service.start().expect("the watcher must start");
    wait_for_status(&service, Duration::from_secs(30), |status| {
        !matches!(status, at_fs::WatchStatus::Starting)
    })
    .expect("the watcher settles");
    drain(&service, Duration::from_secs(1));

    // A skill manager links a new skill while the app is running.
    let global = 15 * ROOT_COUNT;
    let new_skill = fixture.library.join(format!("skill-{global:04}"));
    write_library_skill(&fixture.library, global);
    let new_link = fixture.skills_roots()[0].join("skill-new");
    link(&new_skill, &new_link);
    std::thread::sleep(Duration::from_millis(500));

    let target_file = new_skill.join("SKILL.md");
    fs::write(&target_file, "# rewritten\n").expect("edit the new linked skill");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut attributed = false;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Some(batch) = next_batch(&service, remaining.min(Duration::from_secs(5))) else {
            break;
        };
        if batch
            .paths
            .iter()
            .any(|path| path == &new_link.join("SKILL.md"))
        {
            attributed = true;
            break;
        }
    }
    assert!(
        attributed,
        "a change inside a skill linked after start reaches the batch under the link path"
    );
    service.stop().expect("stop");
}

#[test]
fn an_oversized_root_degrades_to_rescan_without_becoming_unavailable() {
    let fixture = Fixture::build(5);
    std::thread::sleep(Duration::from_millis(2_500));
    let service = WatchService::new(
        WatchConfig::new(fixture.watch_roots())
            .with_allowed_link_roots([fixture.home.clone()])
            .with_max_entries_per_root(10),
    );
    service.start().expect("an oversized root still starts");

    let status = wait_for_status(&service, Duration::from_secs(30), |status| {
        matches!(status, at_fs::WatchStatus::Degraded(_))
    })
    .expect("the root is reported as degraded");
    let at_fs::WatchStatus::Degraded(roots) = status else {
        unreachable!("matched Degraded above")
    };
    assert!(
        roots
            .iter()
            .any(|root| root.reason == at_fs::RootDegradation::TooLarge),
        "the bound is reported as a typed too-large state: {roots:?}"
    );
    assert!(
        service.stats().rescan_required >= 1,
        "a too-large root asks the consumer for a periodic rescan: {:?}",
        service.stats()
    );

    // The service stays up and keeps reporting changes on the root itself. A change behind a
    // link is *not* guaranteed here — the too-large root has no target watches — but the
    // watched directory itself still delivers.
    drain(&service, Duration::from_secs(1));
    let edited = fixture.skills_roots()[0].join("manual-note.md");
    fs::write(&edited, "# edited directly under the root\n").expect("edit");
    let batch = next_batch(&service, Duration::from_secs(30)).expect("a batch after degrade");
    assert!(
        batch.paths.iter().any(|path| path == &edited),
        "watching continues for the degraded root: {:?}",
        batch.paths
    );
    service.stop().expect("stop");
}
