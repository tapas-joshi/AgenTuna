//! TU-034: the CLI is a thin front end; writes and the index belong to other crates.
//!
//! These are architecture tests: they read this crate's manifest and sources and fail
//! when the CLI grows a private IO path instead of calling `at-engine`, `at-fs`,
//! `at-store` or `at-db`.

use std::fs;
use std::path::{Path, PathBuf};

/// Crates `at-cli` may depend on.
///
/// The first-party entries are the engine plus the crates that own every write; the
/// rest are the parser, the JSON encoder, the error derive and two value-only crates
/// (`time` is required by the `at-fs` repair API). Anything else needs a TU-034
/// review, and an IO crate never qualifies.
const ALLOWED: &[&str] = &[
    "at-model",
    "at-store",
    "at-engine",
    "at-db",
    "at-fs",
    "at-adapters",
    "at-migrate",
    "clap",
    "clap_complete",
    "serde",
    "serde_json",
    "thiserror",
    "time",
];

/// Crates that would give the CLI its own database, watcher, Keychain or HTTP path.
const FORBIDDEN: &[&str] = &[
    "rusqlite",
    "libsqlite3-sys",
    "sqlx",
    "diesel",
    "notify",
    "notify-debouncer-full",
    "keyring",
    "keyring-core",
    "reqwest",
    "ureq",
    "hyper",
    "tokio",
    "walkdir",
    "fs-err",
    "atomic-write-file",
];

/// Tokens that mean the CLI is writing files itself instead of calling `at-fs`.
///
/// Reads are allowed: `doctor` inspects metadata and directory listings to report
/// readability, which needs no write pipeline.
const WRITE_TOKENS: &[&str] = &[
    "fs::write",
    "fs::File::create",
    "File::create",
    "OpenOptions",
    "fs::remove_file",
    "fs::remove_dir",
    "fs::remove_dir_all",
    "fs::rename",
    "fs::copy",
    "fs::create_dir",
    "fs::create_dir_all",
    "fs::set_permissions",
    "fs::hard_link",
    "fs::symlink",
    "std::fs::write",
];

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

/// Dependency names declared in one manifest section.
fn dependencies(section: &str) -> Vec<String> {
    let text = fs::read_to_string(manifest_path()).expect("read the at-cli manifest");
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut names = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == header;
            continue;
        }
        if !in_section || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            // `at-model.workspace = true` declares the dependency `at-model`.
            let name = key.trim().split('.').next().unwrap_or_default().trim();
            names.push(name.to_owned());
        }
    }
    names
}

/// Every `.rs` file below `dir`.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Production sources only.
///
/// Unit tests live beside the code (`src/**/tests.rs`, `#[cfg(test)]` modules) and are
/// allowed to write into temp directories: this guard is about the shipped paths.
fn source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files.retain(|path| {
        let test_file = path.file_stem().is_some_and(|stem| stem == "tests");
        let test_dir = path
            .components()
            .any(|component| component.as_os_str() == "tests");
        !test_file && !test_dir
    });
    files.sort();
    assert!(!files.is_empty(), "the at-cli sources were not found");
    files
}

#[test]
fn at_cli_depends_only_on_the_engine_and_the_crates_that_own_writes() {
    let declared = dependencies("dependencies");
    assert!(!declared.is_empty(), "no dependencies were parsed");
    for dependency in &declared {
        assert!(
            !FORBIDDEN.contains(&dependency.as_str()),
            "`at-cli` may not depend on `{dependency}`: the CLI must not open SQLite, \
             watch the filesystem, reach the Keychain or use the network directly"
        );
        assert!(
            ALLOWED.contains(&dependency.as_str()),
            "`at-cli` added `{dependency}`; TU-034 allows only {ALLOWED:?}"
        );
    }
    assert!(
        declared.iter().any(|dependency| dependency == "at-engine"),
        "the CLI must share the engine, not reimplement it"
    );

    for dependency in dependencies("dev-dependencies") {
        assert!(
            !FORBIDDEN.contains(&dependency.as_str()),
            "`{dependency}` is banned even as a dev-dependency"
        );
        assert!(
            dependency == "tempfile" || ALLOWED.contains(&dependency.as_str()),
            "unexpected dev-dependency `{dependency}`: tests may only add test-only helpers"
        );
    }
}

#[test]
fn at_cli_sources_never_write_files_or_open_sqlite() {
    for file in source_files() {
        let text = fs::read_to_string(&file).expect("read an at-cli source");
        let mut in_unit_tests = false;
        for (index, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("#[cfg(test)]") {
                in_unit_tests = true;
            }
            if in_unit_tests || code.starts_with("//") {
                continue;
            }
            for token in WRITE_TOKENS {
                assert!(
                    !code.contains(token),
                    "{}:{} writes through `{token}`; every write goes through at-fs",
                    file.display(),
                    index + 1
                );
            }
            assert!(
                !code.contains("rusqlite"),
                "{}:{} names rusqlite; the index is opened through at-db",
                file.display(),
                index + 1
            );
        }
    }
}

#[test]
fn the_cli_never_builds_a_change_set_itself() {
    // TF-052 owns ChangeSet/FileOp and the engine plans them. A CLI-built change set
    // would let `--dry-run` and the app's planner disagree for the same intent, which
    // is exactly what this story forbids.
    for file in source_files() {
        let text = fs::read_to_string(&file).expect("read an at-cli source");
        for token in ["ChangeSet {", "FileOp {"] {
            assert!(
                !text.contains(token),
                "{} builds `{token}`; plans come from at-engine",
                file.display()
            );
        }
    }
}
