//! `agentuna doctor`: check the local environment and report findings.
//!
//! Every check emits a [`Finding`] carrying a severity, a stable code, a message and a
//! suggested fix, so a machine can act without parsing prose. A check that needs a
//! capability this build does not have reports `unavailable` as a warning instead of
//! claiming success: a false `ok` is worse than an honest gap.
//!
//! The command never trusts the process environment in tests: callers supply
//! [`PathEnvironment`] and [`Probes`], so a test exercises temp directories and fake
//! capabilities and never the real home, Keychain or agent folders.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use at_db::{Database, DbError, Migrations};
use at_store::{AppPaths, PathEnvironment};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::error::{CliError, ErrorCode};
use crate::globals::GlobalArgs;
use crate::output::{CommandOutput, Warning};

/// Free space below this is an error: backups and the index cannot grow.
pub const MIN_FREE_BYTES: u64 = 256 * 1024 * 1024;

/// Free space below this is a warning.
pub const WARN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The check passed.
    Ok,
    /// The check found something the user should fix, but commands still work.
    Warn,
    /// The check found something that blocks normal operation.
    Error,
}

impl Severity {
    /// The lowercase name written to JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Which check produced it: `store_path`, `database`, `agent_paths`, `keychain`,
    /// `disk_space`, `watcher` or `version`.
    pub check: &'static str,
    /// Stable machine code, for example `database_corrupt`.
    pub code: &'static str,
    /// How serious the finding is.
    pub severity: Severity,
    /// What was found, naming every path involved.
    pub message: String,
    /// What the reader can do about it. Never empty.
    pub fix: String,
}

impl Finding {
    fn new(
        check: &'static str,
        code: &'static str,
        severity: Severity,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            check,
            code,
            severity,
            message: message.into(),
            fix: fix.into(),
        }
    }

    fn ok(check: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self::new(check, code, Severity::Ok, message, "no action needed")
    }

    fn warn(
        check: &'static str,
        code: &'static str,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(check, code, Severity::Warn, message, fix)
    }

    fn error(
        check: &'static str,
        code: &'static str,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(check, code, Severity::Error, message, fix)
    }

    fn to_json(&self) -> Value {
        json!({
            "check": self.check,
            "code": self.code,
            "severity": self.severity.as_str(),
            "message": self.message,
            "fix": self.fix,
        })
    }
}

/// A repair `--fix` performed, or would perform under `--dry-run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// What was done: `create_directory`, `create_index` or `move_aside`.
    pub action: &'static str,
    /// The path acted on.
    pub path: PathBuf,
    /// `false` under `--dry-run`, where the fix is listed but not performed.
    pub applied: bool,
    /// One sentence describing the change.
    pub detail: String,
}

impl Fix {
    fn to_json(&self) -> Value {
        json!({
            "action": self.action,
            "path": self.path.display().to_string(),
            "applied": self.applied,
            "detail": self.detail,
        })
    }
}

/// A capability a check needs, which a given build may or may not provide.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability<T> {
    /// The capability exists and reports here.
    Ready(T),
    /// This build cannot perform the check; the reason is shown to the user.
    Unavailable {
        /// Why the check cannot run, phrased for a user.
        reason: String,
    },
}

/// The external probes the checks need.
///
/// The shipped binary uses [`SystemProbes`]; tests substitute their own so nothing
/// reads the real Keychain, the real disk or a real watcher.
pub trait Probes: Send + Sync {
    /// Whether the OS credential store can be reached.
    fn keychain(&self) -> Capability<()>;

    /// Whether the file watcher can start.
    fn watcher(&self) -> Capability<()>;

    /// Free bytes available on the filesystem holding `path`.
    fn free_bytes(&self, path: &Path) -> Capability<u64>;
}

/// The probes the shipped binary uses.
///
/// Free space comes from `df -kP`, the only process `at-cli` spawns: reading
/// `statvfs` would need an FFI or syscall crate, which the TU-034 dependency rule
/// forbids.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbes;

impl Probes for SystemProbes {
    fn keychain(&self) -> Capability<()> {
        Capability::Unavailable {
            reason: "this build's CLI does not link the OS credential store".to_owned(),
        }
    }

    fn watcher(&self) -> Capability<()> {
        Capability::Unavailable {
            reason: "file watching is not wired into this build yet".to_owned(),
        }
    }

    fn free_bytes(&self, path: &Path) -> Capability<u64> {
        match df_free_bytes(path) {
            Ok(bytes) => Capability::Ready(bytes),
            Err(reason) => Capability::Unavailable { reason },
        }
    }
}

static SYSTEM_PROBES: SystemProbes = SystemProbes;

/// Free bytes on the filesystem holding `path`, read from `df -kP`.
fn df_free_bytes(path: &Path) -> Result<u64, String> {
    // The support directory does not exist on a fresh install; `df` needs an existing
    // path, and the nearest ancestor is on the same filesystem.
    let measured = nearest_existing(path);
    let output = Command::new("df")
        .arg("-kP")
        .arg(measured)
        .output()
        .map_err(|error| format!("`df` could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!("`df` failed with {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // POSIX format: Filesystem 1024-blocks Used Available Capacity Mounted-on
    let row = text
        .lines()
        .nth(1)
        .ok_or_else(|| "`df` printed no data row".to_owned())?;
    let available_kib = row
        .split_whitespace()
        .nth(3)
        .ok_or_else(|| "`df` printed fewer than four columns".to_owned())?
        .parse::<u64>()
        .map_err(|_| "`df` printed a non-numeric available column".to_owned())?;
    Ok(available_kib.saturating_mul(1024))
}

/// What `doctor` should do and which capabilities it may use.
#[non_exhaustive]
pub struct Options<'a> {
    /// Apply the safe repairs after checking.
    pub fix: bool,
    /// List the repairs without applying them.
    pub dry_run: bool,
    /// Agent folders to check, or a reason none can be listed.
    pub agent_paths: Capability<Vec<PathBuf>>,
    /// Probes for capabilities at-cli does not link.
    pub probes: &'a dyn Probes,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            fix: false,
            dry_run: false,
            agent_paths: Capability::Unavailable {
                reason: "no agent path source is wired into this build yet".to_owned(),
            },
            probes: &SYSTEM_PROBES,
        }
    }
}

impl<'a> Options<'a> {
    /// Production defaults: no repairs, no dry run, no agent folders, system probes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies the safe repairs after checking.
    #[must_use]
    pub fn fixing(mut self) -> Self {
        self.fix = true;
        self
    }

    /// Lists the repairs without applying them.
    #[must_use]
    pub fn without_writing(mut self) -> Self {
        self.dry_run = true;
        self
    }

    /// Replaces the agent folders this build can list.
    #[must_use]
    pub fn with_agent_paths(mut self, agent_paths: Capability<Vec<PathBuf>>) -> Self {
        self.agent_paths = agent_paths;
        self
    }

    /// Replaces the probes, used by tests to avoid the real disk and Keychain.
    #[must_use]
    pub fn with_probes<'b>(self, probes: &'b dyn Probes) -> Options<'b> {
        Options {
            fix: self.fix,
            dry_run: self.dry_run,
            agent_paths: self.agent_paths,
            probes,
        }
    }
}

/// Every finding and repair of one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// One entry per check, in check order.
    pub findings: Vec<Finding>,
    /// Repairs performed, or planned under `--dry-run`.
    pub fixes: Vec<Fix>,
}

impl Report {
    /// Findings with severity [`Severity::Error`].
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
    }

    /// How many error findings the report holds.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.errors().count()
    }

    /// How many warning findings the report holds.
    #[must_use]
    pub fn warn_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warn)
            .count()
    }

    /// How many findings passed.
    #[must_use]
    pub fn ok_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Ok)
            .count()
    }

    /// The payload written to the envelope's `data`.
    #[must_use]
    pub fn payload(&self) -> Value {
        json!({
            "findings": self.findings.iter().map(Finding::to_json).collect::<Vec<_>>(),
            "fixes": self.fixes.iter().map(Fix::to_json).collect::<Vec<_>>(),
            "summary": {
                "ok": self.ok_count(),
                "warnings": self.warn_count(),
                "errors": self.error_count(),
            },
        })
    }

    /// Warnings carried in the envelope, one per warning finding.
    #[must_use]
    pub fn warnings(&self) -> Vec<Warning> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warn)
            .map(|finding| Warning::new(finding.code, finding.message.clone()))
            .collect()
    }

    /// The human rendering: a summary line, one line per finding, then the repairs.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let mut text = format!(
            "doctor: {} ok, {} warning(s), {} error(s)\n",
            self.ok_count(),
            self.warn_count(),
            self.error_count()
        );
        for finding in &self.findings {
            let _ = writeln!(
                text,
                "{:<5} {:<11} {:<26} {}",
                finding.severity.as_str(),
                finding.check,
                finding.code,
                finding.message
            );
            if finding.severity != Severity::Ok {
                let _ = writeln!(text, "{:<5} {:<11} {:<26} fix: {}", "", "", "", finding.fix);
            }
        }
        if !self.fixes.is_empty() {
            text.push_str("\nrepairs:\n");
            for fix in &self.fixes {
                let state = if fix.applied { "applied" } else { "planned" };
                let _ = writeln!(
                    text,
                    "  {state:<8} {:<16} {}\n           {}",
                    fix.action,
                    fix.path.display(),
                    fix.detail
                );
            }
        }
        text
    }
}

/// Runs every check against already-resolved paths.
#[must_use]
pub fn inspect(paths: &AppPaths, options: &Options<'_>) -> Report {
    let mut findings = Vec::new();
    let mut repair = Repair {
        enabled: options.fix,
        dry_run: options.dry_run,
        fixes: Vec::new(),
    };

    check_store_paths(paths, &mut repair, &mut findings);
    check_database(paths, &mut repair, &mut findings);
    check_agent_paths(&options.agent_paths, &mut findings);
    check_keychain(options.probes, &mut findings);
    check_disk_space(paths, options.probes, &mut findings);
    check_watcher(options.probes, &mut findings);
    check_version(&mut findings);

    Report {
        findings,
        fixes: repair.fixes,
    }
}

/// Resolves the paths from `env`, then runs every check.
///
/// A path that cannot be resolved becomes an error finding rather than a crash: the
/// run still reports everything it could inspect.
#[must_use]
pub fn diagnose(env: &PathEnvironment, options: &Options<'_>) -> Report {
    match AppPaths::resolve(env) {
        Ok(paths) => inspect(&paths, options),
        Err(error) => Report {
            findings: vec![Finding::error(
                "store_path",
                "paths_unresolved",
                format!("the AgenTuna roots cannot be resolved: {error}"),
                "pass `--project` a directory that exists, or fix the home directory and its overrides",
            )],
            fixes: Vec::new(),
        },
    }
}

/// Runs `agentuna doctor` from the parsed global flags.
///
/// # Errors
/// Returns a [`ErrorCode::DoctorFailure`] error carrying the report when any check
/// found an error, so the process exits [`crate::exit_code::DOCTOR_FAILURE`].
pub fn dispatch(globals: &GlobalArgs, fix: bool) -> Result<CommandOutput, CliError> {
    if fix && globals.read_only {
        return Err(CliError::new(
            ErrorCode::Locked,
            "`--fix` cannot run because `--read-only` forbids writes",
            "drop `--read-only` to let doctor repair the store, or run `agentuna doctor` without `--fix` to inspect it",
        ));
    }
    let mut env = PathEnvironment::from_process();
    if let Some(project) = &globals.project {
        env = env.with_project_root(project);
    }
    let options = Options {
        fix,
        dry_run: globals.dry_run,
        ..Options::default()
    };
    output(diagnose(&env, &options))
}

/// Maps a finished report to command output and the documented exit code.
///
/// # Errors
/// Any error finding turns the run into a failure carrying the whole report, so the
/// caller exits with [`crate::exit_code::DOCTOR_FAILURE`] and JSON consumers still
/// receive `data.findings`.
pub fn output(report: Report) -> Result<CommandOutput, CliError> {
    let errors = report.error_count();
    let payload = report.payload();
    let human = report.render();
    let warnings = report.warnings();

    if errors > 0 {
        return Err(CliError::new(
            ErrorCode::DoctorFailure,
            format!("doctor reported {errors} failing check(s)"),
            "apply the suggested fix for each error finding; in JSON mode they are under `data.findings`",
        )
        .with_data(payload)
        .with_report(human));
    }
    Ok(CommandOutput::new(payload, human)
        .with_warnings(warnings)
        .reporting_warnings())
}

/// Repairs in flight, or planned under `--dry-run`.
struct Repair {
    enabled: bool,
    dry_run: bool,
    fixes: Vec<Fix>,
}

impl Repair {
    fn record(&mut self, action: &'static str, path: &Path, detail: String) {
        self.fixes.push(Fix {
            action,
            path: path.to_path_buf(),
            applied: self.enabled && !self.dry_run,
            detail,
        });
    }
}

/// Store roots with the label used in messages and whether `--fix` may create them.
fn store_roots(paths: &AppPaths) -> Vec<(&'static str, &Path, bool)> {
    let mut roots = vec![
        ("support directory", paths.support_dir(), true),
        ("user store", paths.user_store(), true),
    ];
    if let Some(project) = paths.project_store() {
        roots.push(("project store", project, true));
    }
    roots.push(("global store", paths.global_store(), false));
    roots
}

fn check_store_paths(paths: &AppPaths, repair: &mut Repair, out: &mut Vec<Finding>) {
    let roots = store_roots(paths);
    if repair.enabled {
        let missing: Vec<(&'static str, PathBuf)> = roots
            .iter()
            .filter(|(_, path, creatable)| *creatable && std::fs::metadata(path).is_err())
            .map(|(label, path, _)| (*label, path.to_path_buf()))
            .collect();
        if !missing.is_empty() {
            if repair.dry_run {
                for (label, path) in &missing {
                    repair.record(
                        "create_directory",
                        path,
                        format!("create the {label} and the directories AgenTuna owns"),
                    );
                }
            } else {
                match paths.create_dirs() {
                    Ok(()) => {
                        for (label, path) in &missing {
                            repair.record(
                                "create_directory",
                                path,
                                format!("created the {label} and the directories AgenTuna owns"),
                            );
                        }
                    }
                    Err(error) => out.push(Finding::error(
                        "store_path",
                        "store_path_create_failed",
                        format!("the AgenTuna directories could not be created: {error}"),
                        "fix the permissions of the parent directory and rerun `agentuna doctor --fix`",
                    )),
                }
            }
        }
    }
    for (label, path, creatable) in &roots {
        out.push(inspect_store_root(label, path, *creatable));
    }
}

fn inspect_store_root(label: &'static str, path: &Path, creatable: bool) -> Finding {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Finding::warn(
            "store_path",
            "store_path_missing",
            format!("the {label} {} does not exist", path.display()),
            if creatable {
                "run `agentuna doctor --fix` to create it".to_owned()
            } else {
                format!(
                    "create it as an administrator, for example `sudo mkdir -p {}`",
                    path.display()
                )
            },
        ),
        Err(error) => Finding::error(
            "store_path",
            "store_path_unreadable",
            format!(
                "the {label} {} cannot be inspected: {error}",
                path.display()
            ),
            "fix the permissions of the parent directory",
        ),
        Ok(metadata) if !metadata.is_dir() => Finding::error(
            "store_path",
            "store_path_not_directory",
            format!("the {label} {} is not a directory", path.display()),
            "move the file out of the way so AgenTuna can create the directory",
        ),
        Ok(metadata) => {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o200 == 0 {
                Finding::error(
                    "store_path",
                    "store_path_read_only",
                    format!(
                        "the {label} {} is not writable by its owner (mode 0o{mode:03o})",
                        path.display()
                    ),
                    format!("restore write permission: `chmod u+w {}`", path.display()),
                )
            } else {
                Finding::ok(
                    "store_path",
                    "store_path_ok",
                    format!(
                        "the {label} {} is writable (mode 0o{mode:03o})",
                        path.display()
                    ),
                )
            }
        }
    }
}

fn check_database(paths: &AppPaths, repair: &mut Repair, out: &mut Vec<Finding>) {
    let db_path = paths.db_path();

    if std::fs::metadata(db_path).is_err_and(|error| error.kind() == ErrorKind::NotFound) {
        let message = format!("the index {} does not exist yet", db_path.display());
        if !repair.enabled {
            out.push(Finding::warn(
                "database",
                "database_absent",
                message,
                "run `agentuna doctor --fix` to create and migrate the index",
            ));
            return;
        }
        if repair.dry_run {
            repair.record(
                "create_index",
                db_path,
                "create and migrate the SQLite index".to_owned(),
            );
            out.push(Finding::warn(
                "database",
                "database_absent",
                message,
                "`agentuna doctor --fix` would create and migrate the index",
            ));
            return;
        }
        match Database::open_file_inspecting(db_path) {
            Ok(db) => {
                drop(db);
                repair.record(
                    "create_index",
                    db_path,
                    "created and migrated the SQLite index".to_owned(),
                );
            }
            Err(error) => {
                out.push(Finding::error(
                    "database",
                    "database_create_failed",
                    format!("the index {} could not be created: {error}", db_path.display()),
                    "check that the support directory is writable and rerun `agentuna doctor --fix`",
                ));
                return;
            }
        }
    }

    match Database::open_file_inspecting(db_path) {
        Ok(db) => {
            if let Err(detail) = database_integrity(&db) {
                let failure = Finding::error(
                    "database",
                    "database_corrupt",
                    format!(
                        "PRAGMA integrity_check failed for {}: {detail}",
                        db_path.display()
                    ),
                    "run `agentuna doctor --fix` to rebuild the index; the unreadable file is kept",
                );
                drop(db);
                rebuild_index(db_path, failure, repair, out);
                return;
            }
            let version = match db.schema_version() {
                Ok(version) => version,
                Err(error) => {
                    let failure = Finding::error(
                        "database",
                        "database_unreadable",
                        format!("the index schema version cannot be read: {error}"),
                        "run `agentuna doctor --fix` to rebuild the index; the unreadable file is kept",
                    );
                    drop(db);
                    rebuild_index(db_path, failure, repair, out);
                    return;
                }
            };
            let latest = match Migrations::shipped() {
                Ok(migrations) => migrations.latest(),
                Err(error) => {
                    out.push(Finding::error(
                        "database",
                        "database_migrations_invalid",
                        format!("this build ships an invalid migration list: {error}"),
                        "this is a bug in agentuna; please report the command you ran",
                    ));
                    return;
                }
            };
            out.push(schema_finding(db_path, version, latest));
        }
        Err(error) => {
            let (failure, repairable) = classify_db_error(&error, db_path);
            if repairable {
                rebuild_index(db_path, failure, repair, out);
            } else {
                out.push(failure);
            }
        }
    }
}

/// Compares the recorded schema version with the newest migration this build ships.
fn schema_finding(db_path: &Path, version: u32, latest: u32) -> Finding {
    match version.cmp(&latest) {
        std::cmp::Ordering::Equal => Finding::ok(
            "database",
            "database_ok",
            format!(
                "the index {} is healthy at schema v{version} (integrity ok)",
                db_path.display()
            ),
        ),
        std::cmp::Ordering::Less => Finding::warn(
            "database",
            "database_schema_behind",
            format!(
                "the index {} is at schema v{version}, this build ships v{latest}",
                db_path.display()
            ),
            "run any command that opens the index, or `agentuna doctor --fix`, to apply the migrations",
        ),
        std::cmp::Ordering::Greater => Finding::error(
            "database",
            "database_schema_too_new",
            format!(
                "the index {} is at schema v{version}, newer than v{latest}",
                db_path.display()
            ),
            "upgrade agentuna; the index is not touched so no history is lost",
        ),
    }
}

/// Describes an open failure and reports whether `--fix` may rebuild the index.
fn classify_db_error(error: &DbError, db_path: &Path) -> (Finding, bool) {
    match error {
        DbError::SchemaTooNew { found, supported } => (
            Finding::error(
                "database",
                "database_schema_too_new",
                format!(
                    "the index {} was written by a newer AgenTuna (schema v{found}, this build supports v{supported})",
                    db_path.display()
                ),
                "upgrade agentuna; the index is not touched so no history is lost",
            ),
            false,
        ),
        DbError::MigrationChecksumMismatch { version, .. } => (
            Finding::error(
                "database",
                "database_checksum_mismatch",
                format!(
                    "the index {} records migration v{version} with a different checksum than this build ships",
                    db_path.display()
                ),
                "run `agentuna doctor --fix` to rebuild the index; the old file is kept",
            ),
            true,
        ),
        DbError::WalUnavailable { actual, .. } => (
            Finding::error(
                "database",
                "database_not_wal",
                format!(
                    "the index {} cannot use WAL journal mode (SQLite kept {actual})",
                    db_path.display()
                ),
                "move the index to a local filesystem that supports WAL",
            ),
            false,
        ),
        other => (
            Finding::error(
                "database",
                "database_unreadable",
                format!("the index {} cannot be opened: {other}", db_path.display()),
                "run `agentuna doctor --fix` to rebuild the index; the unreadable file is kept",
            ),
            true,
        ),
    }
}

/// Runs SQLite's own integrity check over an open index.
fn database_integrity(db: &Database) -> Result<(), String> {
    db.connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())
        .and_then(|result| if result == "ok" { Ok(()) } else { Err(result) })
}

/// Rebuilds the index after moving the unreadable file aside with `at-fs`.
///
/// The index is rebuildable from the files on disk; the old file is preserved under
/// `<name>.agentuna-backup-<timestamp>` rather than deleted, so the repair is undoable.
///
/// `failure` describes the problem an error finding reports. A successful repair
/// demotes it to a warning: the final state is healthy, so an `--fix` run that worked
/// must not exit with [`crate::exit_code::DOCTOR_FAILURE`].
fn rebuild_index(db_path: &Path, failure: Finding, repair: &mut Repair, out: &mut Vec<Finding>) {
    if !repair.enabled {
        out.push(failure);
        return;
    }
    if repair.dry_run {
        repair.record(
            "move_aside",
            db_path,
            "rename the unreadable index aside and rebuild an empty one".to_owned(),
        );
        out.push(failure);
        return;
    }
    match at_fs::move_aside(db_path, OffsetDateTime::now_utc()) {
        Ok(outcome) => {
            repair.record(
                "move_aside",
                db_path,
                format!(
                    "kept the unreadable index as {}",
                    outcome.moved_to.display()
                ),
            );
            match Database::open_file_inspecting(db_path) {
                Ok(db) => {
                    drop(db);
                    repair.record(
                        "create_index",
                        db_path,
                        "rebuilt an empty index with the shipped migrations applied".to_owned(),
                    );
                    out.push(Finding::new(
                        "database",
                        failure.code,
                        Severity::Warn,
                        format!(
                            "{}; the index was rebuilt and the old file kept as {}",
                            failure.message,
                            outcome.moved_to.display()
                        ),
                        "rescan the store from the app so the rebuilt index is repopulated",
                    ));
                }
                Err(error) => out.push(Finding::error(
                    "database",
                    "database_rebuild_failed",
                    format!(
                        "the index {} could not be rebuilt after the repair: {error}",
                        db_path.display()
                    ),
                    "check that the support directory is writable and rerun `agentuna doctor --fix`",
                )),
            }
        }
        Err(error) => out.push(Finding::error(
            "database",
            "database_rebuild_failed",
            format!(
                "the unreadable index {} could not be moved aside: {error}",
                db_path.display()
            ),
            "check the permissions of the support directory and rerun `agentuna doctor --fix`",
        )),
    }
}

fn check_agent_paths(agent_paths: &Capability<Vec<PathBuf>>, out: &mut Vec<Finding>) {
    let roots = match agent_paths {
        Capability::Ready(roots) => roots,
        Capability::Unavailable { reason } => {
            out.push(Finding::warn(
                "agent_paths",
                "agent_paths_unavailable",
                format!("agent folder readability cannot be checked: {reason}"),
                "connect an agent in the app so its configuration folder can be checked",
            ));
            return;
        }
    };
    if roots.is_empty() {
        out.push(Finding::warn(
            "agent_paths",
            "agent_paths_none",
            "no agent folders are registered, so none were checked",
            "connect an agent in the app so its configuration folder is watched",
        ));
        return;
    }
    for root in roots {
        out.push(inspect_agent_root(root));
    }
}

fn inspect_agent_root(root: &Path) -> Finding {
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if let Some(Err(error)) = entries.next() {
                return Finding::error(
                    "agent_paths",
                    "agent_path_unreadable",
                    format!(
                        "the agent folder {} cannot be listed: {error}",
                        root.display()
                    ),
                    "fix the permissions of the agent folder",
                );
            }
            Finding::ok(
                "agent_paths",
                "agent_path_ok",
                format!("the agent folder {} is readable", root.display()),
            )
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Finding::warn(
            "agent_paths",
            "agent_path_missing",
            format!("the agent folder {} does not exist", root.display()),
            "install or connect the agent, or remove it from the agent list",
        ),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Finding::error(
            "agent_paths",
            "agent_path_permission_denied",
            format!(
                "the agent folder {} is not readable: permission denied",
                root.display()
            ),
            format!(
                "grant this user read access: `chmod u+rX {}`",
                root.display()
            ),
        ),
        Err(error) => Finding::error(
            "agent_paths",
            "agent_path_unreadable",
            format!(
                "the agent folder {} cannot be listed: {error}",
                root.display()
            ),
            "fix the permissions of the agent folder",
        ),
    }
}

fn check_keychain(probes: &dyn Probes, out: &mut Vec<Finding>) {
    match probes.keychain() {
        Capability::Ready(()) => out.push(Finding::ok(
            "keychain",
            "keychain_ok",
            "the OS credential store is reachable",
        )),
        Capability::Unavailable { reason } => out.push(Finding::warn(
            "keychain",
            "keychain_unavailable",
            format!("Keychain access cannot be checked: {reason}"),
            "run the desktop app, which links the credential store, to check Keychain access",
        )),
    }
}

fn check_disk_space(paths: &AppPaths, probes: &dyn Probes, out: &mut Vec<Finding>) {
    let path = paths.support_dir();
    match probes.free_bytes(path) {
        Capability::Ready(bytes) if bytes < MIN_FREE_BYTES => out.push(Finding::error(
            "disk_space",
            "disk_space_low",
            format!(
                "{} has {} free, less than the {} an operation needs",
                path.display(),
                format_bytes(bytes),
                format_bytes(MIN_FREE_BYTES)
            ),
            "free disk space before the next write; backups go to the same volume",
        )),
        Capability::Ready(bytes) if bytes < WARN_FREE_BYTES => out.push(Finding::warn(
            "disk_space",
            "disk_space_tight",
            format!(
                "{} has {} free, below the {} AgenTuna recommends",
                path.display(),
                format_bytes(bytes),
                format_bytes(WARN_FREE_BYTES)
            ),
            "free disk space or point AGENTUNA_HOME at a larger volume",
        )),
        Capability::Ready(bytes) => out.push(Finding::ok(
            "disk_space",
            "disk_space_ok",
            format!("{} has {} free", path.display(), format_bytes(bytes)),
        )),
        Capability::Unavailable { reason } => out.push(Finding::warn(
            "disk_space",
            "disk_space_unavailable",
            format!("free disk space cannot be measured: {reason}"),
            "check free space with `df -h` on the volume holding the support directory",
        )),
    }
}

fn check_watcher(probes: &dyn Probes, out: &mut Vec<Finding>) {
    match probes.watcher() {
        Capability::Ready(()) => out.push(Finding::ok(
            "watcher",
            "watcher_ok",
            "the file watcher starts and observes the store",
        )),
        Capability::Unavailable { reason } => out.push(Finding::warn(
            "watcher",
            "watcher_unavailable",
            format!("the file watcher cannot be started: {reason}"),
            "run the desktop app, which owns the watcher, to check drift detection",
        )),
    }
}

fn check_version(out: &mut Vec<Finding>) {
    out.push(Finding::ok(
        "version",
        "binary_version",
        format!("agentuna {}", env!("CARGO_PKG_VERSION")),
    ));
}

/// The closest existing ancestor of `path`, which sits on the same filesystem.
fn nearest_existing(path: &Path) -> &Path {
    let mut candidate = path;
    loop {
        if std::fs::metadata(candidate).is_ok() {
            return candidate;
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => return path,
        }
    }
}

/// Human-readable byte count for messages.
fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}
