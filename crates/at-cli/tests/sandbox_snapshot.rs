//! TU-038: real-binary smoke runs stay inside the fake HOME and preserve read-only state.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{Sandbox, agentuna, exit_code, stderr, stdout};

fn tree_hash(root: &Path) -> [u8; 32] {
    fn visit(root: &Path, current: &Path, digest: &mut Sha256) {
        let mut entries = fs::read_dir(current)
            .expect("read sandbox fixture")
            .map(|entry| entry.expect("read sandbox entry").path())
            .collect::<Vec<PathBuf>>();
        entries.sort();
        for path in entries {
            let relative = path
                .strip_prefix(root)
                .expect("entry remains under sandbox");
            // The log directory is the one place a read-only command may append to: logging
            // is diagnostics, not configuration, and is covered by the logging tests.
            // SQLite creates and removes `-wal`/`-shm` sidecars while reading a WAL database.
            let name = relative
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if relative.starts_with("support/logs")
                || name.ends_with(".db-wal")
                || name.ends_with(".db-shm")
            {
                continue;
            }
            digest.update(relative.to_string_lossy().as_bytes());
            let metadata = fs::symlink_metadata(&path).expect("read sandbox metadata");
            if metadata.file_type().is_symlink() {
                digest.update(b"symlink");
                digest.update(
                    fs::read_link(&path)
                        .expect("read sandbox symlink")
                        .to_string_lossy()
                        .as_bytes(),
                );
            } else if metadata.is_dir() {
                digest.update(b"directory");
                visit(root, &path, digest);
            } else if metadata.is_file() {
                digest.update(b"file");
                digest.update(fs::read(&path).expect("read sandbox file"));
            }
        }
    }

    let mut digest = Sha256::new();
    visit(root, root, &mut digest);
    digest.finalize().into()
}

fn assert_read_only_case(sandbox: &Sandbox, args: &[&str], expected_code: i32) {
    let before = tree_hash(&sandbox.path(""));
    let output = agentuna(sandbox, args);
    assert_eq!(
        exit_code(&output),
        expected_code,
        "args={args:?}; stdout={}; stderr={}",
        stdout(&output),
        stderr(&output)
    );
    if args.contains(&"--json") {
        let envelope: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("invalid JSON envelope for {args:?}: {error}"));
        assert!(envelope.is_object(), "JSON output is an object: {envelope}");
        for key in [
            "schema_version",
            "command",
            "ok",
            "data",
            "warnings",
            "error",
        ] {
            assert!(
                envelope.get(key).is_some(),
                "envelope lacks {key}: {envelope}"
            );
        }
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["ok"], expected_code == 0);
        if expected_code == 2 {
            assert_eq!(envelope["error"]["code"], "usage");
        }
    }
    assert_eq!(
        tree_hash(&sandbox.path("")),
        before,
        "read-only CLI invocation changed the fake HOME: {args:?}"
    );
}

#[test]
fn read_only_commands_keep_sandbox_tree_hash_unchanged() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    // Config's first invocation may create its AgenTuna-owned defaults; establish that
    // baseline before asserting that subsequent inspection commands are side-effect free.
    let config = agentuna(&sandbox, &["config", "--json"]);
    assert_eq!(exit_code(&config), 0, "{}", stderr(&config));

    let cases: &[(&[&str], i32)] = &[
        (&["projects", "list"], 0),
        (&["projects", "list", "--json"], 0),
        (&["scan"], 0),
        (&["scan", "--json"], 0),
        (&["doctor"], 0),
        (&["doctor", "--json"], 0),
        (&["completions", "bash"], 0),
        (&["version"], 0),
        (&["config"], 0),
        (&["config", "--json"], 0),
        (&["projects", "list", "--read-only", "--json"], 0),
    ];
    for (args, expected_code) in cases {
        assert_read_only_case(&sandbox, args, *expected_code);
    }

    let registry = at_cli::production_registry().expect("production command registry is valid");
    for noun in registry.names() {
        assert_read_only_case(&sandbox, &[noun, "--help"], 0);
    }
}

#[test]
fn malformed_and_unknown_invocations_preserve_fake_home_hash() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    assert_read_only_case(&sandbox, &["not-a-command"], 2);
    assert_read_only_case(&sandbox, &["--json", "not-a-command"], 2);
    assert_read_only_case(&sandbox, &["projects", "unknown"], 2);
    assert_read_only_case(&sandbox, &["doctor", "--unknown"], 2);
}
