//! TU-032: `agentuna doctor` findings, exit codes and safe repairs.
//!
//! Every test runs against a [`support::Sandbox`]: temp roots, fixture probes, no real
//! home, Keychain, disk or agent folder is touched.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use at_cli::doctor::{self, Capability, Options, Probes, Severity};
use at_cli::{ErrorCode, exit_code};
use at_db::{Database, Migrations};
use serde_json::Value;

use support::{Sandbox, agentuna, exit_code as process_exit, stderr, stdout};

/// Probes with no real IO: the test decides what each capability reports.
struct FixtureProbes {
    keychain: Capability<()>,
    watcher: Capability<()>,
    free: Capability<u64>,
}

impl Default for FixtureProbes {
    fn default() -> Self {
        Self {
            keychain: Capability::Unavailable {
                reason: "fixture: no credential store".to_owned(),
            },
            watcher: Capability::Unavailable {
                reason: "fixture: no watcher".to_owned(),
            },
            free: Capability::Ready(10 * 1024 * 1024 * 1024),
        }
    }
}

impl Probes for FixtureProbes {
    fn keychain(&self) -> Capability<()> {
        self.keychain.clone()
    }

    fn watcher(&self) -> Capability<()> {
        self.watcher.clone()
    }

    fn free_bytes(&self, _path: &Path) -> Capability<u64> {
        self.free.clone()
    }
}

fn options_with(probes: &FixtureProbes) -> Options<'_> {
    Options::new().with_probes(probes)
}

#[test]
fn every_check_reports_a_finding_with_code_message_and_fix() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();

    let agent_root = sandbox.path("agent");
    std::fs::create_dir_all(&agent_root).expect("create the fixture agent folder");
    let probes = FixtureProbes::default();
    let options = options_with(&probes).with_agent_paths(Capability::Ready(vec![agent_root]));

    let report = doctor::inspect(&paths, &options);

    for check in [
        "store_path",
        "database",
        "agent_paths",
        "keychain",
        "disk_space",
        "watcher",
        "version",
    ] {
        assert!(
            report.findings.iter().any(|finding| finding.check == check),
            "the `{check}` check wrote no finding; it wrote {:?}",
            report
                .findings
                .iter()
                .map(|finding| finding.check)
                .collect::<Vec<_>>()
        );
    }
    for finding in &report.findings {
        assert!(!finding.code.is_empty(), "{} has no code", finding.check);
        assert!(
            !finding.message.is_empty(),
            "{} has no message",
            finding.check
        );
        assert!(
            !finding.fix.trim().is_empty(),
            "{} has no fix hint",
            finding.check
        );
        assert!(
            [
                Severity::Ok,
                Severity::Warn,
                Severity::Error,
                Severity::NotApplicable
            ]
            .contains(&finding.severity),
            "{} has an unknown severity",
            finding.check
        );
    }
    assert_eq!(report.error_count(), 0, "a healthy sandbox has no errors");

    // The payload the envelope carries.
    let payload = report.payload();
    let findings = payload["findings"]
        .as_array()
        .expect("findings is an array");
    assert_eq!(findings.len(), report.findings.len());
    for finding in findings {
        for field in ["check", "code", "severity", "message", "fix"] {
            assert!(
                finding[field].is_string(),
                "`{field}` is a string: {finding}"
            );
        }
        assert!(
            ["ok", "warn", "error", "not_applicable"]
                .contains(&finding["severity"].as_str().unwrap_or("")),
            "`severity` is a documented status: {finding}"
        );
    }
    assert_eq!(payload["summary"]["errors"], 0);
}

#[test]
fn a_warnings_only_run_succeeds_with_ok_and_warnings() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();
    // A real warning: free space cannot be measured. The GUI-owned probes now report
    // `not_applicable` in a CLI run, so they no longer supply the warning.
    let probes = FixtureProbes {
        free: Capability::Unavailable {
            reason: "fixture: df cannot run".to_owned(),
        },
        ..FixtureProbes::default()
    };

    let report = doctor::inspect(&paths, &options_with(&probes));
    assert_eq!(report.error_count(), 0);
    assert!(
        report.warn_count() > 0,
        "the fixture has warnings to report"
    );
    assert!(report.ok_count() > 0, "the healthy checks must pass");

    let output = doctor::output(report).expect("a warnings-only run succeeds");
    assert!(
        !output.warnings.is_empty(),
        "warnings reach the envelope, not only `data`"
    );
    for warning in &output.warnings {
        assert!(!warning.code.is_empty());
        assert!(!warning.message.is_empty());
    }
    assert_eq!(output.data["summary"]["errors"], 0);
    assert!(
        output.human.starts_with("doctor: "),
        "the human rendering starts with a summary: {}",
        output.human
    );
}

#[test]
fn an_error_finding_exits_eight_and_keeps_its_findings_in_data() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();
    std::fs::write(paths.db_path(), b"not a database").expect("corrupt the fixture index");

    let probes = FixtureProbes::default();
    let report = doctor::inspect(&paths, &options_with(&probes));
    assert!(
        report.error_count() > 0,
        "a corrupt index is an error finding"
    );

    let error = doctor::output(report).expect_err("an error finding fails the run");
    assert_eq!(error.exit_code(), exit_code::DOCTOR_FAILURE);
    assert_eq!(error.code(), ErrorCode::DoctorFailure);
    assert!(!error.hint().is_empty());

    let envelope = at_cli::output::envelope_err("doctor", &error);
    assert_eq!(envelope["ok"], Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "doctor_failed");
    let findings = envelope["data"]["findings"]
        .as_array()
        .expect("data.findings survives the failure");
    assert!(!findings.is_empty());
    assert!(
        findings
            .iter()
            .any(|finding| finding["severity"] == "error"),
        "the failing finding is in the payload: {findings:?}"
    );
    assert!(
        error
            .report()
            .is_some_and(|report| report.contains("doctor: ")),
        "a failed run still renders its report for humans"
    );
}

#[test]
fn a_permission_denied_agent_folder_is_an_error_naming_the_path() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();

    let denied = sandbox.path("denied-agent");
    std::fs::create_dir_all(&denied).expect("create the fixture agent folder");
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000))
        .expect("remove read permission");

    // A privileged process can still read the folder; then the environment cannot
    // produce the state this test is about.
    if std::fs::read_dir(&denied).is_ok() {
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700))
            .expect("restore read permission");
        eprintln!("skipping: this process can read a 0o000 directory");
        return;
    }

    let probes = FixtureProbes::default();
    let report = doctor::inspect(
        &paths,
        &options_with(&probes).with_agent_paths(Capability::Ready(vec![denied.clone()])),
    );

    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700))
        .expect("restore read permission");

    let finding = report
        .findings
        .iter()
        .find(|finding| finding.code == "agent_path_permission_denied")
        .unwrap_or_else(|| panic!("no permission-denied finding: {:?}", report.findings));
    assert_eq!(finding.severity, Severity::Error);
    assert!(
        finding.message.contains(&denied.display().to_string()),
        "the finding names the path: {}",
        finding.message
    );
    assert!(!finding.fix.trim().is_empty());
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.check == "database"),
        "the rest of the run still happened"
    );
}

#[test]
fn fix_creates_the_store_directories_and_lists_each_action() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    assert!(!paths.support_dir().exists());

    let probes = FixtureProbes::default();
    let report = doctor::inspect(&paths, &options_with(&probes).fixing());

    assert!(
        paths.support_dir().is_dir(),
        "the support directory is created"
    );
    assert!(paths.user_store().is_dir(), "the user store is created");
    assert!(
        paths.db_path().is_file(),
        "the index is created and migrated"
    );
    assert_eq!(report.error_count(), 0, "{:?}", report.findings);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "store_path_ok"),
        "the checks reflect the repair"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "database_ok"),
        "the fresh index is healthy"
    );

    let created: Vec<&doctor::Fix> = report
        .fixes
        .iter()
        .filter(|fix| fix.action == "create_directory")
        .collect();
    assert!(!created.is_empty(), "each created directory is listed");
    assert!(created.iter().all(|fix| fix.applied));
    assert!(report.fixes.iter().any(|fix| fix.action == "create_index"));
}

#[test]
fn fix_rebuilds_a_corrupt_index_and_keeps_the_old_file() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let paths = sandbox.paths();
    let garbage = b"not a sqlite database";
    std::fs::write(paths.db_path(), garbage).expect("corrupt the fixture index");

    let probes = FixtureProbes::default();
    let report = doctor::inspect(&paths, &options_with(&probes).fixing());
    assert_eq!(report.error_count(), 0, "{:?}", report.findings);

    // The unreadable file is preserved, never deleted.
    let backup = moved_aside(paths.support_dir(), "agentuna.db")
        .expect("the unreadable index is kept under a backup name");
    assert_eq!(
        std::fs::read(&backup).expect("read the kept index"),
        garbage
    );

    // The rebuilt index is usable and at the newest schema.
    let db = Database::open_file(paths.db_path()).expect("the rebuilt index opens");
    let latest = Migrations::shipped().expect("shipped migrations").latest();
    assert_eq!(db.schema_version().expect("schema version"), latest);
    drop(db);

    assert!(
        report
            .fixes
            .iter()
            .any(|fix| fix.action == "move_aside" && fix.applied),
        "the repair is listed: {:?}",
        report.fixes
    );
    assert!(
        report
            .fixes
            .iter()
            .any(|fix| fix.action == "create_index" && fix.applied)
    );
}

#[test]
fn a_dry_run_lists_the_repairs_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let paths = sandbox.paths();
    let probes = FixtureProbes::default();

    let report = doctor::inspect(&paths, &options_with(&probes).fixing().without_writing());

    assert!(!paths.support_dir().exists(), "a dry run creates nothing");
    assert!(!paths.user_store().exists(), "a dry run creates nothing");
    assert!(!report.fixes.is_empty(), "a dry run lists what it would do");
    assert!(
        report.fixes.iter().all(|fix| !fix.applied),
        "a dry run performs nothing: {:?}",
        report.fixes
    );
    assert!(
        report
            .fixes
            .iter()
            .any(|fix| fix.action == "create_directory")
    );
}

#[test]
fn an_unresolvable_project_is_an_error_finding_not_a_crash() {
    let sandbox = Sandbox::new();
    let missing = sandbox.path("missing-project");
    let env = sandbox.env().with_project_root(&missing);

    let probes = FixtureProbes::default();
    let report = doctor::diagnose(&env, &options_with(&probes));
    assert_eq!(report.error_count(), 1);
    let finding = &report.findings[0];
    assert_eq!(finding.code, "paths_unresolved");
    assert!(
        finding.message.contains(&missing.display().to_string()),
        "the finding names the path: {}",
        finding.message
    );
}

#[test]
fn doctor_reports_warnings_and_exits_zero_in_json_mode() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["--json", "doctor"]);
    assert_eq!(
        process_exit(&output),
        exit_code::SUCCESS,
        "a warnings-only run exits 0: {}",
        stderr(&output)
    );

    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    assert_eq!(envelope["ok"], Value::Bool(true));
    assert_eq!(envelope["command"], "doctor");
    assert!(
        envelope["warnings"]
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty()),
        "warnings are populated: {envelope}"
    );
    let findings = envelope["data"]["findings"]
        .as_array()
        .expect("data.findings");
    assert!(!findings.is_empty());
    assert_eq!(envelope["data"]["summary"]["errors"], 0);
    assert!(
        findings.iter().any(|finding| finding["check"] == "version"),
        "the binary version is one of the checks"
    );
}

#[test]
fn doctor_exits_eight_when_the_index_is_corrupt() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    std::fs::write(sandbox.paths().db_path(), b"not a database").expect("corrupt the index");

    let output = agentuna(&sandbox, &["--json", "doctor"]);
    assert_eq!(process_exit(&output), exit_code::DOCTOR_FAILURE);
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    assert_eq!(envelope["ok"], Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "doctor_failed");
    assert!(
        envelope["error"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty())
    );
    assert!(
        envelope["data"]["findings"]
            .as_array()
            .is_some_and(|findings| findings.iter().any(|f| f["severity"] == "error")),
        "the failing findings stay in `data.findings`: {envelope}"
    );

    // Human mode still shows the report, with the summary on stderr.
    let human = agentuna(&sandbox, &["doctor"]);
    assert_eq!(process_exit(&human), exit_code::DOCTOR_FAILURE);
    assert!(
        stdout(&human).contains("doctor: "),
        "the report goes to stdout: {}",
        stdout(&human)
    );
    let diagnostics = stderr(&human);
    assert!(diagnostics.contains("error:"), "stderr: {diagnostics}");
    assert!(diagnostics.contains("hint:"), "stderr: {diagnostics}");
}

#[test]
fn doctor_fix_repairs_the_store_through_the_binary() {
    let sandbox = Sandbox::new();
    let applied = agentuna(&sandbox, &["--json", "doctor", "--fix"]);
    assert_eq!(
        process_exit(&applied),
        exit_code::SUCCESS,
        "{}",
        stderr(&applied)
    );
    let envelope: Value = serde_json::from_str(&stdout(&applied)).expect("one JSON envelope");
    assert!(
        envelope["data"]["fixes"]
            .as_array()
            .is_some_and(|fixes| fixes.iter().all(|fix| fix["applied"] == true)),
        "`--fix` applies the repairs: {envelope}"
    );
    assert!(sandbox.paths().support_dir().is_dir());
    assert!(sandbox.paths().db_path().is_file());

    let dry = Sandbox::new();
    let planned = agentuna(&dry, &["--json", "doctor", "--fix", "--dry-run"]);
    assert_eq!(process_exit(&planned), exit_code::SUCCESS);
    let envelope: Value = serde_json::from_str(&stdout(&planned)).expect("one JSON envelope");
    assert!(
        envelope["data"]["fixes"]
            .as_array()
            .is_some_and(|fixes| fixes.iter().all(|fix| fix["applied"] == false)),
        "`--dry-run` applies nothing: {envelope}"
    );
    assert!(
        !dry.paths().support_dir().exists(),
        "`--fix --dry-run` writes nothing"
    );
}

#[test]
fn doctor_never_reads_the_real_home() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["--json", "doctor", "--fix"]);
    let text = format!("{}{}", stdout(&output), stderr(&output));

    if let Ok(home) = std::env::var("HOME") {
        assert!(
            !home.is_empty() && !text.contains(&home),
            "doctor must stay inside the sandbox; the real home {home} appears in: {text}"
        );
    }
    assert!(
        !text.contains(".agentuna"),
        "no real store path is reported: {text}"
    );
}

/// Finds a file named `<name>.agentuna-backup-*` inside `dir`.
fn moved_aside(dir: &Path, name: &str) -> Option<PathBuf> {
    let prefix = format!("{name}.agentuna-backup-");
    std::fs::read_dir(dir)
        .expect("read the support directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&prefix))
        })
}

#[test]
fn the_human_report_lists_each_warning_once() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["doctor"]);
    assert_eq!(process_exit(&output), exit_code::SUCCESS);

    let report = stdout(&output);
    assert!(
        report.contains("warn  store_path"),
        "the report lists the warning: {report}"
    );
    assert!(
        !stderr(&output).contains("warning:"),
        "the report already lists the warnings, so stderr must not repeat them: {}",
        stderr(&output)
    );
}

#[test]
fn fix_refuses_to_write_in_read_only_mode() {
    let sandbox = Sandbox::new();
    let output = agentuna(&sandbox, &["--json", "--read-only", "doctor", "--fix"]);
    assert_eq!(
        process_exit(&output),
        exit_code::LOCKED,
        "`--read-only` forbids the repair: {}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    assert_eq!(envelope["ok"], Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "locked");
    assert!(
        !sandbox.paths().support_dir().exists(),
        "a refused repair writes nothing"
    );
}

#[test]
fn cli_reports_gui_owned_checks_as_not_applicable() {
    let sandbox = Sandbox::new();
    sandbox.healthy();

    let output = agentuna(&sandbox, &["--json", "doctor"]);
    assert_eq!(
        process_exit(&output),
        exit_code::SUCCESS,
        "not-applicable checks must not degrade the status: {}",
        stderr(&output)
    );
    let envelope: Value = serde_json::from_str(&stdout(&output)).expect("one JSON envelope");
    assert_eq!(envelope["data"]["summary"]["errors"], 0);
    assert_eq!(envelope["data"]["summary"]["not_applicable"], 3);
    let findings = envelope["data"]["findings"]
        .as_array()
        .expect("data.findings");
    for check in ["agent_paths", "keychain", "watcher"] {
        let finding = findings
            .iter()
            .find(|finding| finding["check"] == check)
            .unwrap_or_else(|| panic!("`{check}` wrote no finding: {envelope}"));
        assert_eq!(
            finding["severity"], "not_applicable",
            "`{check}` is owned by the desktop app: {finding}"
        );
        assert!(
            !finding["code"].as_str().unwrap_or_default().is_empty(),
            "a not-applicable finding still carries a stable code: {finding}"
        );
    }
    let warning_codes = envelope["warnings"]
        .as_array()
        .expect("envelope warnings")
        .iter()
        .filter_map(|warning| warning["code"].as_str())
        .collect::<Vec<_>>();
    for code in [
        "agent_paths_unavailable",
        "keychain_unavailable",
        "watcher_unavailable",
    ] {
        assert!(
            !warning_codes.contains(&code),
            "`{code}` is not a CLI warning: {warning_codes:?}"
        );
    }
}

#[test]
fn a_desktop_run_keeps_warnings_for_gui_owned_checks() {
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let probes = FixtureProbes::default();

    let report = doctor::inspect(&sandbox.paths(), &options_with(&probes).with_desktop(true));
    assert_eq!(report.not_applicable_count(), 0);
    for check in ["agent_paths", "keychain", "watcher"] {
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.check == check)
            .unwrap_or_else(|| panic!("`{check}` wrote no finding"));
        assert_eq!(
            finding.severity,
            Severity::Warn,
            "the app could run `{check}`, so an unavailable probe is a warning"
        );
    }
}
