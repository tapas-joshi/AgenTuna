//! Guard: only `at-store` and `at-app` may turn the environment into AgenTuna paths.

use std::fs;
use std::path::{Path, PathBuf};

/// Crates allowed to resolve `$HOME` and build the `.agentuna` store path.
const ALLOWED_CRATES: &[&str] = &["at-store", "at-app"];

/// Test-only fixture module (cargo feature `testkit`, never compiled into production
/// builds) that must set HOME for sandboxed homes. Exactly this file, relative to the
/// workspace root, is exempt.
const EXEMPT_FILES: &[&str] = &["crates/at-fs/src/testkit.rs"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate>/..")
        .to_path_buf()
}

fn rust_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Whether a source line resolves a home directory or builds the store path itself.
fn is_violation(line: &str) -> bool {
    line.contains("dirs::home_dir")
        || line.contains("\"HOME\"")
        || line.contains("\".agentuna\"")
        || line.contains("'.agentuna'")
        || line.contains("\"/.agentuna\"")
        || line.contains("'/.agentuna'")
}

fn violations() -> Vec<String> {
    let crates = workspace_root().join("crates");
    let mut found = Vec::new();
    let mut scanned = 0_usize;
    for entry in fs::read_dir(&crates).expect("crates directory") {
        let directory = entry.expect("crate entry").path();
        let Some(name) = directory.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if ALLOWED_CRATES.contains(&name) {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&directory.join("src"), &mut files);
        for file in files {
            let relative = file.strip_prefix(workspace_root()).unwrap_or(&file);
            if EXEMPT_FILES
                .iter()
                .any(|exempt| relative == Path::new(exempt))
            {
                continue;
            }
            scanned += 1;
            let text = fs::read_to_string(&file).expect("read source");
            for (index, line) in text.lines().enumerate() {
                if is_violation(line) {
                    found.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
                }
            }
        }
    }
    assert!(scanned > 20, "grep test scanned only {scanned} files");
    found
}

#[test]
fn detector_flags_only_real_violations() {
    assert!(is_violation("let home = dirs::home_dir();"));
    assert!(is_violation("let home = std::env::var(\"HOME\").ok();"));
    assert!(is_violation("let store = home.join(\".agentuna\");"));
    assert!(is_violation("let store = Path::new('/.agentuna');"));
    // Temp-file and backup names are not the store directory.
    assert!(!is_violation(
        "let temp = parent.join(format!(\".{name}.agentuna-tmp-{}\"));"
    ));
    assert!(!is_violation(
        "pub const COPY_MANIFEST_FILE: &str = \".agentuna-copy-manifest.json\";"
    ));
    assert!(!is_violation("let home = paths.home_dir();"));
}

#[test]
fn only_store_and_app_read_home_or_build_the_store_path() {
    let found = violations();
    assert!(
        found.is_empty(),
        "crates other than {ALLOWED_CRATES:?} must receive paths from AppPaths:\n{}",
        found.join("\n")
    );
}
