//! Logging acceptance tests: daily file, redaction, retention, reload, stderr fallback.
//!
//! Every test runs in a temp dir with an injected clock and an injected redactor; nothing
//! touches the real HOME or the process environment. Subscriber tests use
//! [`subscriber_for_test`] plus `tracing::dispatcher::with_default`, so parallel tests
//! never race on `set_global_default`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::LogLevel;
use crate::logging::{
    LoggingOptions, RETENTION_DAYS, RedactorSlot, relative_path, subscriber_for_test, sweep_expired,
};
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn fixed_now() -> Arc<dyn Fn() -> OffsetDateTime + Send + Sync> {
    Arc::new(|| datetime!(2026-11-05 12:00 UTC))
}

fn options(dir: &Path) -> LoggingOptions {
    LoggingOptions {
        log_dir: dir.to_path_buf(),
        level: LogLevel::Info,
        json: false,
        rust_log: None,
        secret_values: Vec::new(),
        secret_names: Vec::new(),
        redactor: None,
        session_id: Some("test-session".to_owned()),
        app_version: Some("0.0.0-test".to_owned()),
        schema_version: Some(7),
        os_version: Some("macos 27.3".to_owned()),
        now: Some(fixed_now()),
        stderr: None,
        file_logging: true,
    }
}

fn read_logs(dir: &Path) -> String {
    let mut out = String::new();
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .expect("read log dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agentuna-") && n.ends_with(".log"))
        })
        .collect();
    names.sort();
    for name in names {
        out.push_str(&fs::read_to_string(&name).expect("read log file"));
    }
    out
}

/// Reads until `ready` accepts the value, so no assertion depends on a fixed sleep.
///
/// The file appender writes synchronously on the calling thread, so the first read normally
/// carries everything; the bound only covers a file system that is still flushing on a loaded
/// machine.
fn settle<T>(read: impl Fn() -> T, ready: impl Fn(&T) -> bool) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let value = read();
        if ready(&value) || std::time::Instant::now() >= deadline {
            return value;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn first_run_creates_daily_file_with_startup_line() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");

    let expected = log_dir.join("agentuna-2026-11-05.log");
    assert!(expected.exists(), "daily log file was not created");
    let text = fs::read_to_string(&expected).expect("read log");
    assert!(
        text.contains("AgenTuna started"),
        "startup line missing: {text}"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("AgenTuna started"))
            .count(),
        1,
        "exactly one startup line per run: {text}"
    );
    assert!(
        text.contains("app_version=\"0.0.0-test\""),
        "app version missing: {text}"
    );
    assert!(
        text.contains("os_version=\"macos 27.3\""),
        "os version missing: {text}"
    );
    assert!(
        text.contains("schema_version=7"),
        "schema version missing: {text}"
    );
    assert!(
        text.contains("session_id=\"test-session\""),
        "session id missing: {text}"
    );
    drop(dispatch);
}

#[test]
fn redaction_masks_known_secret_and_preserves_var_reference() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let secret = concat!("sk-", "live-0123456789abcdef");
    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    opts.secret_values = vec![secret.to_owned()];
    opts.secret_names = vec!["GITHUB_TOKEN".to_owned()];
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");

    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(
            target: "at_store::logging::tests::redaction",
            token = %secret,
            assignment = "GITHUB_TOKEN=ghp_0123456789abcdef",
            reference = "use ${GITHUB_TOKEN} here",
            "redaction probe"
        );
    });
    let text = settle(
        || read_logs(&log_dir),
        |text| text.contains("redaction probe"),
    );
    assert!(
        !text.contains(secret),
        "secret value leaked into log:\n{text}"
    );
    assert!(
        !text.contains("ghp_0123456789abcdef"),
        "ghp_ token leaked into log:\n{text}"
    );
    assert!(text.contains("***"), "expected redaction mask:\n{text}");
    assert!(
        text.contains("${GITHUB_TOKEN}"),
        "${{GITHUB_TOKEN}} reference was altered:\n{text}"
    );
    assert!(
        text.contains("GITHUB_TOKEN=***"),
        "named assignment not masked as expected:\n{text}"
    );
}

#[test]
fn set_level_takes_effect_through_reload_handle() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    // Empty string pins "no RUST_LOG" without marking the env filter as set, so the
    // settings level owns the filter and `set_level` can reload it.
    opts.rust_log = Some(String::new());
    let (dispatch, guard) = subscriber_for_test(&opts).expect("build subscriber");
    let logging = guard.logging().clone();

    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(
            target: "at_store::logging::tests::reload",
            "before-reload info"
        );
        logging.set_level(LogLevel::Error).expect("reload to error");
        tracing::info!(
            target: "at_store::logging::tests::reload",
            "after-reload info"
        );
        tracing::error!(
            target: "at_store::logging::tests::reload",
            "after-reload error"
        );
    });
    let text = settle(
        || read_logs(&log_dir),
        |text| text.contains("after-reload error"),
    );
    assert!(
        text.contains("before-reload info"),
        "info line missing:\n{text}"
    );
    assert!(
        !text.contains("after-reload info"),
        "info leaked after error reload:\n{text}"
    );
    assert!(
        text.contains("after-reload error"),
        "error line missing:\n{text}"
    );
}

#[test]
fn rust_log_env_var_makes_set_level_a_no_op() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    opts.rust_log = Some("debug".to_owned());
    let (dispatch, guard) = subscriber_for_test(&opts).expect("build subscriber");
    let logging = guard.logging().clone();

    tracing::dispatcher::with_default(&dispatch, || {
        logging.set_level(LogLevel::Error).expect("no-op reload");
        tracing::debug!(
            target: "at_store::logging::tests::rust_log_noop",
            "debug survives settings change"
        );
    });
    let text = settle(
        || read_logs(&log_dir),
        |text| text.contains("debug survives settings change"),
    );
    assert!(
        text.contains("debug survives settings change"),
        "RUST_LOG=debug was overridden by set_level:\n{text}"
    );
}

#[test]
fn expired_logs_are_swept_on_startup() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    fs::create_dir_all(&log_dir).unwrap();

    let now = fixed_now();
    let today = now().date();
    let old = today - time::Duration::days(RETENTION_DAYS + 1);
    let recent = today - time::Duration::days(RETENTION_DAYS - 1);
    let old_name = format!(
        "agentuna-{:04}-{:02}-{:02}.log",
        old.year(),
        u8::from(old.month()),
        old.day()
    );
    let recent_name = format!(
        "agentuna-{:04}-{:02}-{:02}.log",
        recent.year(),
        u8::from(recent.month()),
        recent.day()
    );
    fs::write(log_dir.join(&old_name), b"old\n").unwrap();
    fs::write(log_dir.join(&recent_name), b"recent\n").unwrap();
    fs::write(log_dir.join("agentuna-not-a-date.log"), b"keep?\n").unwrap();

    sweep_expired(&log_dir, &now).expect("sweep");

    assert!(!log_dir.join(&old_name).exists(), "expired file kept");
    assert!(log_dir.join(&recent_name).exists(), "recent file deleted");
    // Unparseable name falls back to mtime; just-created file is recent, so it stays.
    assert!(log_dir.join("agentuna-not-a-date.log").exists());
}

#[test]
fn sweep_runs_during_init() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    fs::create_dir_all(&log_dir).unwrap();
    let now = fixed_now();
    let old = now().date() - time::Duration::days(RETENTION_DAYS + 5);
    let old_name = format!(
        "agentuna-{:04}-{:02}-{:02}.log",
        old.year(),
        u8::from(old.month()),
        old.day()
    );
    fs::write(log_dir.join(&old_name), b"ancient\n").unwrap();

    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    let (_dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");
    assert!(
        !log_dir.join(&old_name).exists(),
        "init did not sweep the expired file"
    );
}

#[test]
fn read_only_log_dir_falls_back_to_stderr_and_still_logs() {
    let dir = tempdir();
    // A file where the log dir should be: create_dir_all fails, open fails.
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"not a dir").unwrap();
    let log_dir = blocker.join("logs");

    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let captured_clone = Arc::clone(&captured);
    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    opts.stderr = Some(Arc::new(move || {
        let buf = Arc::clone(&captured_clone);
        Box::new(SharedBuf(buf)) as Box<dyn Write + Send>
    }));
    let (dispatch, _guard) =
        subscriber_for_test(&opts).expect("init must succeed without a log dir");
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(
            target: "at_store::logging::tests::stderr_fallback",
            "stderr fallback probe"
        );
    });
    let text = settle(
        || String::from_utf8_lossy(&captured.lock().unwrap()).into_owned(),
        |text| text.contains("stderr fallback probe") || text.contains("AgenTuna started"),
    );
    assert!(
        text.contains("stderr fallback probe") || text.contains("AgenTuna started"),
        "stderr fallback produced no output:\n{text}"
    );
}

#[test]
fn json_mode_emits_parseable_objects() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    opts.json = true;
    opts.rust_log = Some("info".to_owned());
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(
            target: "at_store::logging::tests::json",
            answer = 42,
            "json probe"
        );
    });
    let text = settle(|| read_logs(&log_dir), |text| text.contains("json probe"));
    let line = text
        .lines()
        .find(|l| l.contains("json probe"))
        .expect("json event line missing");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("line is not JSON");
    assert_eq!(parsed["answer"], 42);
    assert_eq!(parsed["message"], "json probe");
}

#[test]
fn daily_rollover_uses_injected_clock() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let clock = Arc::new(Mutex::new(datetime!(2026-11-05 23:59 UTC)));
    let clock_for_opts: Arc<dyn Fn() -> OffsetDateTime + Send + Sync> = {
        let clock = Arc::clone(&clock);
        Arc::new(move || *clock.lock().unwrap())
    };
    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    opts.now = Some(clock_for_opts);
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");

    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(target: "at_store::logging::tests::rollover", "day one");
        *clock.lock().unwrap() = datetime!(2026-11-06 00:01 UTC);
        tracing::info!(target: "at_store::logging::tests::rollover", "day two");
    });
    let _ = settle(|| read_logs(&log_dir), |text| text.contains("day two"));
    let day1_file = log_dir.join("agentuna-2026-11-05.log");
    let day2_file = log_dir.join("agentuna-2026-11-06.log");
    assert!(day1_file.exists(), "day-1 file missing");
    assert!(day2_file.exists(), "day-2 file missing after rollover");
    assert!(fs::read_to_string(&day1_file).unwrap().contains("day one"));
    assert!(fs::read_to_string(&day2_file).unwrap().contains("day two"));
}

#[test]
fn rust_log_overrides_settings_level() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    opts.level = LogLevel::Error;
    opts.rust_log = Some("debug".to_owned());
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::debug!(
            target: "at_store::logging::tests::rust_log",
            "debug via rust_log"
        );
    });
    let text = settle(
        || read_logs(&log_dir),
        |text| text.contains("debug via rust_log"),
    );
    assert!(
        text.contains("debug via rust_log"),
        "RUST_LOG=debug ignored:\n{text}"
    );
}

#[test]
fn panic_inside_instrumented_scope_writes_error_with_location() {
    let dir = tempdir();
    let log_dir = dir.path().join("logs");
    let mut opts = options(&log_dir);
    opts.rust_log = Some("info".to_owned());
    let (dispatch, _guard) = subscriber_for_test(&opts).expect("build subscriber");

    // tracing_panic's hook emits the ERROR event; installing it here mirrors what
    // `Logging::init` does in production (the hook itself is process-global).
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(tracing_panic::panic_hook));

    let job_id = "job_testpanic0000000000000000000000";
    let result = tracing::dispatcher::with_default(&dispatch, || {
        let span = crate::logging::job_span(job_id, "panic-job");
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _entered = span.enter();
            panic!("deliberate test panic");
        }))
    });
    assert!(result.is_err(), "panic was not caught");
    std::panic::set_hook(previous);
    // The process keeps running after the caught panic, and its next event is logged.
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(job_id = %job_id, "process continued after the panic");
    });
    let text = settle(
        || read_logs(&log_dir),
        |text| {
            (text.contains("A panic occurred") || text.contains("deliberate test panic"))
                && text.contains("process continued after the panic")
        },
    );
    assert!(
        text.contains("A panic occurred") || text.contains("deliberate test panic"),
        "panic ERROR entry missing:\n{text}"
    );
    assert!(
        text.contains("panic.payload") || text.contains("deliberate test panic"),
        "panic payload field missing:\n{text}"
    );
    assert!(
        text.contains("panic.location") || text.contains("logging/tests.rs"),
        "panic location missing:\n{text}"
    );
    assert!(
        text.contains(job_id),
        "panic entry must carry the enclosing job span's job id:\n{text}"
    );
    let panic_entries = text
        .lines()
        .filter(|line| {
            line.contains("ERROR")
                && (line.contains("deliberate test panic") || line.contains("A panic occurred"))
        })
        .count();
    assert_eq!(
        panic_entries, 1,
        "one ERROR entry per panic, not a cascade:\n{text}"
    );
    assert!(
        text.contains("process continued after the panic"),
        "the process must keep logging after the panic:\n{text}"
    );
}

#[test]
fn redactor_slot_accepts_late_secret_registration() {
    let slot = RedactorSlot::new();
    assert!(slot.add_name("LATE_TOKEN"));
    assert!(slot.add_value(concat!("late-", "value-0123456789")));
    let out = slot.redact("LATE_TOKEN=abc and late-value-0123456789 and ${LATE_TOKEN}");
    assert!(out.contains("LATE_TOKEN=***"), "{out}");
    assert!(out.contains("***"), "{out}");
    assert!(out.contains("${LATE_TOKEN}"), "{out}");
}

#[test]
fn relative_path_renders_home_relative() {
    let home = Path::new("/home/user");
    assert_eq!(
        relative_path(Path::new("/home/user/x/y"), Some(home)).as_ref(),
        "~/x/y"
    );
    assert_eq!(
        relative_path(Path::new("/elsewhere/x"), Some(home)).as_ref(),
        "/elsewhere/x"
    );
    assert_eq!(
        relative_path(Path::new("/home/user"), Some(home)).as_ref(),
        "~"
    );
}

#[derive(Debug)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
