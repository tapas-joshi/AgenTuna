//! Guard: no at-model struct stores a secret value in a `String` field.
//!
//! Secrets live in the OS credential store. The model may hold a *reference*
//! (an environment name and a Keychain account), never the value, so no field may
//! be spelled like a credential. This scans the crate's own source, which is also
//! what keeps a reviewer from adding one later.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_FIELD_NAMES: &[&str] = &[
    "secret",
    "secret_value",
    "token",
    "password",
    "api_key",
    "apikey",
    "client_secret",
    "access_token",
    "refresh_token",
    "private_key",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[test]
fn no_string_field_is_named_like_a_secret() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty());

    let mut inspected = 0;
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim().trim_end_matches(',');
            let Some((name, ty)) = line.split_once(':') else {
                continue;
            };
            let name = name.strip_prefix("pub ").unwrap_or(name).trim();
            if !is_identifier(name) {
                continue;
            }
            let ty = ty.trim();
            if ty != "String" && ty != "Option<String>" {
                continue;
            }
            inspected += 1;
            assert!(
                !FORBIDDEN_FIELD_NAMES.contains(&name),
                "{}:{}: field `{name}: {ty}` may hold a secret value; store a SecretRef instead",
                file.display(),
                index + 1
            );
        }
    }
    assert!(inspected > 0, "the scan must actually see String fields");
}
