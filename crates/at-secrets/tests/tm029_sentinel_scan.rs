//! Adversarial scans prove the secret sentinel detector itself can fail.

use at_secrets::{Redactor, SecretString};
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind, set_hook, take_hook};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

const SENTINEL: &str = "sk-live-tm029-sentinel-6fd3b90e";

fn scan(root: &Path, sentinel: &str) -> Vec<(PathBuf, usize)> {
    let mut pending = vec![root.to_path_buf()];
    let mut hits = Vec::new();
    while let Some(path) = pending.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = fs::read(&path)
                && let Some(offset) = find_bytes(&bytes, sentinel.as_bytes())
            {
                hits.push((path, offset));
            }
        }
    }
    hits
}

fn file_count(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { file_count(&path) } else { 1 }
        })
        .sum()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn sentinel_scan_covers_home_store_database_wal_backups_and_proves_detection() {
    let root = tempdir().expect("temporary root");
    let buckets = [
        "home",
        "store",
        "db",
        "backups",
        "logs",
        "diagnostics",
        "journal",
        "diffs",
        "toasts",
        "crash",
        "argv",
    ];
    for name in buckets {
        fs::create_dir_all(root.path().join(name)).expect("fixture directory");
    }

    // More than 200 independent artifacts cover the temp home, store, database,
    // backups, generated configs, operation journals, diagnostics, diffs and logs.
    for index in 0..205 {
        let directory = buckets[index % buckets.len()];
        fs::write(
            root.path()
                .join(directory)
                .join(format!("artifact-{index:03}")),
            format!("safe artifact {index}"),
        )
        .expect("fixture file");
    }
    let database = Connection::open(root.path().join("db/agentuna.db")).expect("SQLite database");
    database
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    database
        .execute_batch(
            "CREATE TABLE fixture (value TEXT); INSERT INTO fixture VALUES ('safe value');",
        )
        .expect("write SQLite main and WAL files");
    assert!(root.path().join("db/agentuna.db").is_file());
    assert!(root.path().join("db/agentuna.db-wal").is_file());
    fs::write(
        root.path().join("crash/leak-self-test.txt"),
        "fixture-secret-leak",
    )
    .expect("self-test leak");
    fs::write(
        root.path().join("store/generated-agent.json"),
        br#"{"env":{"TOKEN":"${REMOTE_TOKEN}"}}"#,
    )
    .expect("generated config");
    fs::write(
        root.path().join("backups/remote.json"),
        br#"{"token":"${REMOTE_TOKEN}"}"#,
    )
    .expect("backup");

    let artifacts = scan(root.path(), SENTINEL);
    assert!(
        file_count(root.path()) >= 200,
        "scanner must cover at least 200 files"
    );
    assert!(
        artifacts.is_empty(),
        "unexpected sentinel hits: {artifacts:?}"
    );
    let leaked = scan(root.path(), "fixture-secret-leak");
    assert_eq!(
        leaked.len(),
        1,
        "the self-test must detect a deliberate leak"
    );
    assert!(leaked[0].0.ends_with("leak-self-test.txt"));
    assert_eq!(leaked[0].1, 0, "leak report includes byte offset");
}

#[test]
fn secret_error_and_panic_payloads_are_redacted_before_serialization() {
    let mut redactor = Redactor::new();
    redactor.insert(SecretString::new(SENTINEL));
    let value = SecretString::new(SENTINEL);
    let error = format!("connection rejected token {}", value.expose_secret());
    let payload = json!({"code":"connection_failed", "message":redactor.redact(&error)});
    let encoded = serde_json::to_string(&payload).expect("error JSON");
    assert!(!encoded.contains(SENTINEL));

    let panic_log = Arc::new(Mutex::new(String::new()));
    let panic_log_sink = Arc::clone(&panic_log);
    let panic_redactor = redactor;
    let prior_hook = take_hook();
    set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic payload");
        panic_log_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_str(&panic_redactor.redact(message));
    }));
    let panic_result = catch_unwind(AssertUnwindSafe(|| {
        panic!("request failed: {}", value.expose_secret());
    }));
    set_hook(prior_hook);
    assert!(panic_result.is_err());
    let panic_log = panic_log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!panic_log.contains(SENTINEL));
    assert!(panic_log.contains("***"));
}

/// Kills the fixture child even when an assertion panics.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `ps` and returns its stdout.
fn ps_output(args: &[&str]) -> String {
    let output = std::process::Command::new("/bin/ps")
        .args(args)
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_spawned_child_receives_the_secret_only_through_its_environment() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let dir = tempdir().expect("temporary root");
    let observed = dir.path().join("child-env.txt");
    // The script mentions only the variable name; the value comes from the environment and
    // is written by the child itself, so this proves delivery without trusting `ps -E`.
    let script = format!(
        "printf '%s' \"$AGENTUNA_TM029_SECRET\" > '{}'; sleep 30",
        observed.display()
    );
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .env("AGENTUNA_TM029_SECRET", SENTINEL)
        .spawn()
        .expect("fixture child spawns");
    let mut guard = ChildGuard(child);
    let pid = guard.0.id().to_string();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if observed.metadata().is_ok_and(|meta| meta.len() > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(&observed).expect("the child wrote its environment value"),
        SENTINEL,
        "the secret reaches the child through its environment"
    );

    // The argv line must never carry the value, even though the script names the variable.
    let argv = ps_output(&["-o", "args=", "-ww", "-p", &pid]);
    assert!(
        !argv.contains(SENTINEL),
        "the secret must never appear in a child's argv: {argv}"
    );
    assert!(
        argv.contains("AGENTUNA_TM029_SECRET"),
        "ps reports the fixture process and its script: {argv}"
    );

    #[cfg(target_os = "linux")]
    {
        let environment = fs::read(format!("/proc/{pid}/environ"))
            .expect("same-user process environment is readable");
        let expected = format!("AGENTUNA_TM029_SECRET={SENTINEL}");
        assert!(
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected.as_bytes()),
            "the exact secret assignment is exposed in the child's environment"
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Where the platform exposes environments through `ps -E`, the value must be there
        // — never in argv. Current macOS omits the environment section, so the child's own
        // report above is the authoritative positive check.
        let with_environment = ps_output(&["-E", "-ww", "-p", &pid]);
        assert!(
            !with_environment.contains(SENTINEL)
                || with_environment.contains("AGENTUNA_TM029_SECRET="),
            "an exposed environment lists the assignment, not a bare argument: {with_environment}"
        );
        assert!(
            with_environment.contains(&pid),
            "ps -E still reports the fixture process: {with_environment}"
        );
    }

    let _ = guard.0.kill();
    let _ = guard.0.wait();
}
