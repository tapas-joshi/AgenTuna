//! TU-025: the Playwright visual goldens are regenerated only on purpose.
//!
//! `UPDATE_SNAPSHOTS=1 cargo test -p at-app --test snapshots` re-runs the visual suite with
//! `--update-snapshots` through the project-local pnpm, which rewrites `ui/e2e/snapshots/**`.
//! Without the variable the comparison runs in the `visual` CI job instead, where the browser
//! and the built UI exist; there a difference over 0.1% of pixels fails the test and uploads
//! the diff artifacts.
//!
//! The inventory check is offline, needs no browser and always runs: a golden set that lost a
//! scale is a failure even when this test is invoked without the update variable.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Scales the Playwright config renders. Each scale needs its own goldens.
const SCALES: [&str; 2] = ["1x", "2x"];

/// The story requires at least twelve snapshots per scale.
const MINIMUM_SNAPSHOTS_PER_SCALE: usize = 12;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves")
}

fn golden_count(root: &Path, scale: &str) -> usize {
    let directory = root.join("ui/e2e/snapshots").join(scale);
    let entries = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension() == Some(OsStr::new("png")))
        .count()
}

#[test]
fn every_scale_keeps_its_golden_inventory() {
    let root = repo_root();
    for scale in SCALES {
        let count = golden_count(&root, scale);
        assert!(
            count >= MINIMUM_SNAPSHOTS_PER_SCALE,
            "ui/e2e/snapshots/{scale} holds {count} goldens; the story needs at least \
             {MINIMUM_SNAPSHOTS_PER_SCALE}"
        );
    }
}

#[test]
fn update_snapshots_regenerates_the_visual_goldens() {
    if std::env::var_os("UPDATE_SNAPSHOTS").as_deref() != Some(OsStr::new("1")) {
        println!(
            "skipped: set UPDATE_SNAPSHOTS=1 to regenerate ui/e2e/snapshots/**; the comparison \
             itself runs in the `visual` CI job"
        );
        return;
    }

    let root = repo_root();
    let playwright = root.join("ui/node_modules/@playwright/test");
    assert!(
        playwright.is_dir(),
        "install the UI dependencies first: pnpm --dir ui install --frozen-lockfile"
    );

    let status = Command::new("pnpm")
        .args(["--dir", "ui", "run", "visual:update"])
        .current_dir(&root)
        .status()
        .expect("the project-local pnpm runs the Playwright update");
    assert!(
        status.success(),
        "`pnpm --dir ui run visual:update` exited with {status}"
    );

    for scale in SCALES {
        let count = golden_count(&root, scale);
        assert!(
            count >= MINIMUM_SNAPSHOTS_PER_SCALE,
            "regenerating left {count} goldens in ui/e2e/snapshots/{scale}"
        );
    }
}
