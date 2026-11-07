//! TA-005 AC5: [`at_model::SyncLabel`] has exactly one derivation.
//!
//! Decision 12 says `at_engine::state::classify` computes the label and TC-015 stores and
//! reads the result, never recomputing it. The executable half of that claim is the doc test
//! on `classify` (`cargo test -p at-engine --doc`); this test is the guard that fails the
//! moment another crate grows a label derivation of its own.

use std::fs;
use std::path::{Path, PathBuf};

/// The `crates/` directory that holds every workspace crate.
fn crates_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a workspace parent")
        .to_path_buf()
}

/// Every `.rs` file below `root` (sorted for a stable failure message).
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort();
    found
}

/// Whether `path` sits inside a crate's `src` tree (implementation, not tests).
fn is_implementation(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "src")
        && !path
            .components()
            .any(|component| component.as_os_str() == "tests")
}

#[test]
fn sync_label_is_derived_only_in_at_engine_state() {
    let crates = crates_root();
    let owner = crates.join("at-engine").join("src").join("state.rs");
    let derivations = [
        "fn derive_label(",
        "fn derive_sync_label(",
        "fn sync_label(",
    ];
    let mut offenders = Vec::new();
    for path in rust_sources(&crates) {
        if !is_implementation(&path) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        for definition in derivations {
            if source.contains(definition) && path != owner {
                offenders.push(format!("{} defines {definition}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "SyncLabel is derived only in at-engine/src/state.rs: {offenders:#?}"
    );

    let state = fs::read_to_string(&owner).expect("the state module is readable");
    assert!(
        state.contains("fn derive_label("),
        "state.rs keeps the single derivation"
    );
    assert!(
        state.contains("/// let derived = classify(DeploymentFacts::managed(Some(\"abc\"), Some(\"abc\"), Some(\"abc\")));"),
        "classify keeps its executable doc example, run by `cargo test -p at-engine --doc`"
    );

    // TC-015's consumer reads the stored value instead of re-deriving it.
    let consumer = crates.join("at-engine").join("src").join("project_sync.rs");
    let source = fs::read_to_string(&consumer).expect("project_sync is readable");
    assert!(
        source.contains("label"),
        "project_sync consumes the stored sync label"
    );
    assert!(
        !derivations
            .iter()
            .any(|definition| source.contains(definition)),
        "project_sync must not derive the label itself"
    );
}
