//! at-model must stay free of IO, networking, databases and async runtimes.

use std::fs;
use std::path::Path;

const FORBIDDEN: &[&str] = &["std::fs", "std::net", "rusqlite", "tokio", "anyhow"];

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn source_uses_no_forbidden_apis() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty());
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for word in FORBIDDEN {
            assert!(!text.contains(word), "{} mentions `{word}`", file.display());
        }
    }
}

#[test]
fn manifest_depends_only_on_allowed_crates() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let deps: Vec<&str> = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap()
        .lines()
        .filter_map(|l| l.split('.').next().map(str::trim))
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        deps,
        [
            // Pure data and formatting crates. `serde_json` carries unknown native
            // MCP keys opaquely (TM-001); `url` types the HTTP transport and
            // `unicode-normalization` normalizes names (TA resolution). None of
            // them performs IO, networking or persistence.
            "serde",
            "serde_json",
            "thiserror",
            "unicode-normalization",
            "url",
            "uuid",
            "time",
        ]
    );
}
