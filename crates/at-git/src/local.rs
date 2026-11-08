//! Local-only git: repository detection, status, ignore checks and staging.
//!
//! Nothing here can reach a network. The module builds a handful of read-mostly commands —
//! `rev-parse --show-toplevel`, `status --porcelain -z`, `check-ignore` and `add` — and nothing
//! else. Every invocation is an argv array (no shell, no interpolation), runs with
//! `--literal-pathspecs` so a path is never interpreted as a pattern or an option, and passes
//! paths after `--`. `git add -f` is not constructible from this API, which is what makes "never
//! stage an ignored file" a property of the code rather than of the caller's discipline.
//! Commands that need the network go through [`crate::policy`] instead.
//!
//! Staging is deliberately narrow: [`add_paths`] adds exactly the paths it is given, git itself
//! refuses a path that an ignore rule covers, and nothing here commits. The commit stays the
//! user's action.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::policy::{
    GitError, GitOutput, MAX_GIT_OUTPUT_BYTES, POLL_INTERVAL, join_reader, read_bounded,
};

/// How long one local git command may run before it is killed.
pub const LOCAL_GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest diagnostic (stderr) this module keeps in an error message, in characters.
pub const MAX_DIAGNOSTIC_CHARS: usize = 2048;

/// Stops git from asking for a password on a terminal it does not have.
const GIT_TERMINAL_PROMPT_ENV: &str = "GIT_TERMINAL_PROMPT";

/// Stops read-only commands from refreshing the index and taking optional locks.
const GIT_OPTIONAL_LOCKS_ENV: &str = "GIT_OPTIONAL_LOCKS";

/// One local git invocation: the directory to run in and the arguments after the program name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalGitCall<'a> {
    /// Directory git runs in. Every path argument is relative to it.
    pub repo: &'a Path,
    /// Arguments after the program name.
    pub args: &'a [&'a str],
    /// Bytes written to standard input before git's output is read, for `--stdin` commands.
    ///
    /// Written from a separate thread, so a child that answers while still reading cannot
    /// deadlock against a full output pipe.
    pub stdin: Option<&'a [u8]>,
    /// Whether git runs with `--literal-pathspecs`.
    ///
    /// `status` and `add` support it; `check-ignore` rejects it as unsupported magic, so its
    /// checks rely on strict pathspec validation instead.
    pub literal_pathspecs: bool,
}

/// Runs local git invocations. The real implementation spawns git; tests record instead.
pub trait LocalGitRunner: Send + Sync {
    /// Runs the invocation.
    ///
    /// # Errors
    ///
    /// [`GitError`] when the process cannot start, its output cannot be read or it times out.
    fn run(&self, call: &LocalGitCall<'_>) -> Result<GitOutput, GitError>;
}

/// Spawns git from `PATH` with prompts disabled, bounded output and a deadline.
///
/// The program and the timeout are injectable so the pump can be exercised without git.
#[derive(Debug, Clone)]
pub struct ProcessLocalGitRunner {
    program: PathBuf,
    timeout: Duration,
    output_limit: usize,
    env: Vec<(OsString, OsString)>,
}

impl Default for ProcessLocalGitRunner {
    fn default() -> Self {
        Self::git()
    }
}

impl ProcessLocalGitRunner {
    /// The system git with the default deadline and output limit.
    #[must_use]
    pub fn git() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: LOCAL_GIT_TIMEOUT,
            output_limit: MAX_GIT_OUTPUT_BYTES,
            env: Vec::new(),
        }
    }

    /// Runs `program` instead of git, with an explicit deadline.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
            output_limit: MAX_GIT_OUTPUT_BYTES,
            env: Vec::new(),
        }
    }

    /// Caps how many bytes of stdout and stderr are kept.
    #[must_use]
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// Passes an extra environment variable to git, overriding the inherited value.
    ///
    /// Tests use this to point git at a sandboxed `HOME` and neutral configuration so a
    /// developer's global ignore rules cannot change the result.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The program this runner executes.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

impl LocalGitRunner for ProcessLocalGitRunner {
    fn run(&self, call: &LocalGitCall<'_>) -> Result<GitOutput, GitError> {
        let stdin = call.stdin.map(<[u8]>::to_vec);
        let mut command = Command::new(&self.program);
        if call.literal_pathspecs {
            command.arg("--literal-pathspecs");
        }
        command
            .current_dir(call.repo)
            .args(call.args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(GIT_TERMINAL_PROMPT_ENV, "0")
            .env(GIT_OPTIONAL_LOCKS_ENV, "0")
            .envs(self.env.iter().map(|(key, value)| (key, value)));
        let mut child = at_mcp::spawn::spawn_retrying(&mut command).map_err(GitError::Spawn)?;

        // A failed write means git stopped reading (usually because it already rejected the
        // command); its exit status and stderr carry the real error, so the write result is
        // deliberately dropped and only joined to close the pipe.
        let writer = stdin.and_then(|bytes| {
            child.stdin.take().map(|mut pipe| {
                thread::spawn(move || {
                    use std::io::Write;
                    let _ = pipe.write_all(&bytes);
                })
            })
        });

        let stdout = child.stdout.take().map(|pipe| {
            let limit = self.output_limit;
            thread::spawn(move || read_bounded(pipe, limit))
        });
        let stderr = child.stderr.take().map(|pipe| {
            let limit = self.output_limit;
            thread::spawn(move || read_bounded(pipe, limit))
        });

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = join_reader(stdout);
                        let _ = join_reader(stderr);
                        join_writer(writer);
                        return Err(GitError::TimedOut(self.timeout));
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = join_reader(stdout);
                    let _ = join_reader(stderr);
                    join_writer(writer);
                    return Err(GitError::Wait(error));
                }
            }
        };

        let stdout = join_reader(stdout)?;
        let stderr = join_reader(stderr)?;
        join_writer(writer);
        Ok(GitOutput {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

/// Waits for the stdin writer, ignoring a write that failed because git stopped reading.
fn join_writer(handle: Option<thread::JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

/// The work tree that contains a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    /// Absolute path of the work-tree root, as git reports it.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// One path in `git status --porcelain -z`, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The two-character `XY` status, for example `??` (untracked) or `M ` (staged change).
    pub code: String,
    /// Path relative to the repository root.
    pub path: String,
    /// Source path of a rename or copy.
    pub from: Option<String>,
}

impl StatusEntry {
    /// Whether the index (left column) records a change for this path, as opposed to only the
    /// work tree or an untracked file.
    #[must_use]
    pub fn is_staged(&self) -> bool {
        let mut chars = self.code.chars();
        match (chars.next(), chars.next()) {
            (Some(left), Some(right)) => left != ' ' && left != '?' && right != '!',
            _ => false,
        }
    }
}

/// The ignore rule that covers a path, as `git check-ignore --verbose` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreRule {
    /// The path the rule covers, relative to the repository root.
    pub path: String,
    /// The file that holds the rule, relative to the repository root when it lives in the
    /// repository (`$GIT_DIR/info/exclude` and global excludes are named as git prints them).
    /// Empty when git took the rule from the command line.
    pub source: String,
    /// Line number inside [`Self::source`], when it has one.
    pub line: Option<u32>,
    /// The pattern text, for example `.state/`.
    pub pattern: String,
}

/// Failure of a local git invocation or of a check around it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LocalGitError {
    /// The directory does not exist or is not a directory.
    #[error("{}: not a directory", .dir.display())]
    MissingDirectory {
        /// Directory git was asked about.
        dir: PathBuf,
    },
    /// The directory is not inside a git work tree.
    #[error("{}: not a git repository", .dir.display())]
    NotARepository {
        /// Directory git was asked about.
        dir: PathBuf,
    },
    /// A path would be read by git as something other than one plain path inside the repository.
    #[error("{}: refusing unsafe git pathspec", .path.display())]
    UnsafePathspec {
        /// Rejected path.
        path: PathBuf,
    },
    /// Git ran and failed for another reason.
    #[error("git {args} failed with {code}: {stderr}")]
    CommandFailed {
        /// Arguments after the program name, joined for display.
        args: String,
        /// Exit code, when the process exited normally.
        code: String,
        /// Bounded standard error.
        stderr: String,
    },
    /// Git printed something this module cannot parse (for example a non-UTF-8 path).
    #[error("could not parse git {args} output: {reason}")]
    InvalidOutput {
        /// Arguments after the program name, joined for display.
        args: String,
        /// What did not parse.
        reason: String,
    },
    /// The process boundary failed.
    #[error(transparent)]
    Process(#[from] GitError),
}

impl LocalGitError {
    /// Whether this failure means "there is no repository here".
    #[must_use]
    pub fn is_not_a_repository(&self) -> bool {
        matches!(self, Self::NotARepository { .. })
    }
}

/// Finds the work tree that contains `dir`, using `git rev-parse --show-toplevel`.
///
/// # Errors
///
/// [`LocalGitError::MissingDirectory`] when `dir` is not a directory,
/// [`LocalGitError::NotARepository`] when git reports no work tree (including a bare
/// repository), and [`LocalGitError::CommandFailed`] for any other git failure.
pub fn detect_repository(
    runner: &dyn LocalGitRunner,
    dir: &Path,
) -> Result<Repository, LocalGitError> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(LocalGitError::MissingDirectory {
                dir: dir.to_path_buf(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(LocalGitError::MissingDirectory {
                dir: dir.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(LocalGitError::CommandFailed {
                args: "rev-parse --show-toplevel".to_owned(),
                code: "io error".to_owned(),
                stderr: error.to_string(),
            });
        }
    }
    let args = ["rev-parse", "--show-toplevel"];
    let output = run(runner, dir, &args)?;
    if !output.success {
        let stderr = diagnostic(&output.stderr);
        if stderr.contains("not a git repository") || stderr.contains("must be run in a work tree")
        {
            return Err(LocalGitError::NotARepository {
                dir: dir.to_path_buf(),
            });
        }
        return Err(command_failed(&args, &output));
    }
    let text = parse_text(&args, &output.stdout)?;
    let root = text.trim();
    if root.is_empty() {
        return Err(LocalGitError::InvalidOutput {
            args: args.join(" "),
            reason: "empty work-tree root".to_owned(),
        });
    }
    Ok(Repository {
        root: PathBuf::from(root),
    })
}

/// Reads `git status --porcelain -z`, optionally limited to one path inside the repository.
///
/// `pathspec` must be repository-relative; pass `None` for the whole work tree.
///
/// # Errors
///
/// [`LocalGitError::UnsafePathspec`], [`LocalGitError::CommandFailed`] or
/// [`LocalGitError::InvalidOutput`].
pub fn status_entries(
    runner: &dyn LocalGitRunner,
    repository: &Repository,
    pathspec: Option<&str>,
) -> Result<Vec<StatusEntry>, LocalGitError> {
    let mut args = vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"];
    if let Some(path) = pathspec {
        validate_pathspec(Path::new(path))?;
        args.push("--");
        args.push(path);
    }
    let output = run_literal(runner, &repository.root, &args)?;
    if !output.success {
        return Err(command_failed(&args, &output));
    }
    parse_status(&args, &output.stdout)
}

/// Returns the subset of `paths` that an ignore rule covers.
///
/// `paths` must be repository-relative and are passed to `git check-ignore --stdin -z` as NUL
/// separators, so a name containing a newline or a tab is still one path. An empty result means
/// nothing is ignored; a path git cannot check (outside the work tree) is reported by git as an
/// error.
///
/// # Errors
///
/// [`LocalGitError::UnsafePathspec`], [`LocalGitError::CommandFailed`] or
/// [`LocalGitError::InvalidOutput`].
pub fn ignored_paths(
    runner: &dyn LocalGitRunner,
    repository: &Repository,
    paths: &[&str],
) -> Result<Vec<String>, LocalGitError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    for path in paths {
        validate_pathspec(Path::new(path))?;
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let args = ["check-ignore", "-z", "--no-index", "--stdin"];
    let output = run_stdin(runner, &repository.root, &args, &input)?;
    // 0 means "some paths are ignored", 1 means "none are ignored"; both are normal.
    match output.code {
        Some(0) => {}
        Some(1) => return Ok(Vec::new()),
        _ => return Err(command_failed(&args, &output)),
    }
    let text = parse_text(&args, &output.stdout)?;
    Ok(text
        .split('\0')
        .filter(|field| !field.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Explains the ignore rule that covers `path`, or `None` when no rule does.
///
/// # Errors
///
/// [`LocalGitError::UnsafePathspec`], [`LocalGitError::CommandFailed`] or
/// [`LocalGitError::InvalidOutput`].
pub fn explain_ignore(
    runner: &dyn LocalGitRunner,
    repository: &Repository,
    path: &str,
) -> Result<Option<IgnoreRule>, LocalGitError> {
    validate_pathspec(Path::new(path))?;
    let mut input = path.as_bytes().to_vec();
    input.push(0);
    let args = ["check-ignore", "--verbose", "-z", "--no-index", "--stdin"];
    let output = run_stdin(runner, &repository.root, &args, &input)?;
    match output.code {
        Some(0) => {}
        Some(1) => return Ok(None),
        _ => return Err(command_failed(&args, &output)),
    }
    let text = parse_text(&args, &output.stdout)?;
    // With `-v -z` each match is four NUL-terminated fields: source, line, pattern, path.
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    let (Some(source), Some(line), Some(pattern), Some(path)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(LocalGitError::InvalidOutput {
            args: args.join(" "),
            reason: "expected source, line, pattern and path".to_owned(),
        });
    };
    Ok(Some(IgnoreRule {
        path: path.to_owned(),
        source: source.to_owned(),
        line: line.parse().ok(),
        pattern: pattern.to_owned(),
    }))
}

/// Stages exactly `paths` with `git add -- <paths>`.
///
/// The paths must be repository-relative and exist. git refuses a path that an ignore rule
/// covers and this module never passes `-f`, so an ignored path can never be staged. Nothing is
/// committed.
///
/// # Errors
///
/// [`LocalGitError::UnsafePathspec`] for a path that is not a plain path inside the repository,
/// [`LocalGitError::CommandFailed`] when git refuses (including an ignored or missing path).
pub fn add_paths(
    runner: &dyn LocalGitRunner,
    repository: &Repository,
    paths: &[&str],
) -> Result<(), LocalGitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    for path in paths {
        validate_pathspec(Path::new(path))?;
    }
    args.extend_from_slice(paths);
    let output = run_literal(runner, &repository.root, &args)?;
    if !output.success {
        return Err(command_failed(&args, &output));
    }
    Ok(())
}

/// Runs one command and maps a process failure into a typed error.
fn run(
    runner: &dyn LocalGitRunner,
    repo: &Path,
    args: &[&str],
) -> Result<GitOutput, LocalGitError> {
    run_call(runner, repo, args, None, false)
}

/// Runs one command whose paths are literal pathspecs.
fn run_literal(
    runner: &dyn LocalGitRunner,
    repo: &Path,
    args: &[&str],
) -> Result<GitOutput, LocalGitError> {
    run_call(runner, repo, args, None, true)
}

/// Runs one command with standard input.
fn run_stdin(
    runner: &dyn LocalGitRunner,
    repo: &Path,
    args: &[&str],
    stdin: &[u8],
) -> Result<GitOutput, LocalGitError> {
    run_call(runner, repo, args, Some(stdin), false)
}

fn run_call(
    runner: &dyn LocalGitRunner,
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    literal_pathspecs: bool,
) -> Result<GitOutput, LocalGitError> {
    Ok(runner.run(&LocalGitCall {
        repo,
        args,
        stdin,
        literal_pathspecs,
    })?)
}

/// Builds the error for a command git ran and failed.
fn command_failed(args: &[&str], output: &GitOutput) -> LocalGitError {
    LocalGitError::CommandFailed {
        args: args.join(" "),
        code: output
            .code
            .map_or_else(|| "no exit code".to_owned(), |code| code.to_string()),
        stderr: diagnostic(&output.stderr),
    }
}

/// Decodes git's standard error for a message, redacted of nothing but bounded: local git
/// diagnostics name paths and options, never credentials.
fn diagnostic(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let bounded: String = chars.by_ref().take(MAX_DIAGNOSTIC_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// Decodes standard output that must be UTF-8.
fn parse_text(args: &[&str], bytes: &[u8]) -> Result<String, LocalGitError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| LocalGitError::InvalidOutput {
        args: args.join(" "),
        reason: "output is not UTF-8".to_owned(),
    })
}

/// Parses `git status --porcelain -z` records: `XY PATH\0`, plus a second `ORIGIN\0` field for a
/// rename or a copy.
fn parse_status(args: &[&str], bytes: &[u8]) -> Result<Vec<StatusEntry>, LocalGitError> {
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        if field.len() < 4 || field.get(2) != Some(&b' ') {
            return Err(LocalGitError::InvalidOutput {
                args: args.join(" "),
                reason: "record is shorter than `XY PATH`".to_owned(),
            });
        }
        let code = parse_text(args, &field[..2])?;
        let path = parse_text(args, &field[3..])?;
        let from = if code.starts_with('R') || code.starts_with('C') {
            match fields.next() {
                Some(source) => Some(parse_text(args, source)?),
                None => {
                    return Err(LocalGitError::InvalidOutput {
                        args: args.join(" "),
                        reason: format!("rename record for {path} has no source path"),
                    });
                }
            }
        } else {
            None
        };
        entries.push(StatusEntry { code, path, from });
    }
    Ok(entries)
}

/// Accepts only a plain, relative path: no absolute path, no `.` or `..` component, no NUL.
fn validate_pathspec(path: &Path) -> Result<(), LocalGitError> {
    let rejected = path.as_os_str().is_empty()
        || path.as_os_str().to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)));
    if rejected {
        return Err(LocalGitError::UnsafePathspec {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pathspec_must_be_a_plain_relative_path() {
        for path in ["../escape", "/etc/passwd", "", "./x", "a/../../b"] {
            assert!(
                validate_pathspec(Path::new(path)).is_err(),
                "{path:?} must be rejected"
            );
        }
        for path in [".site-store", ".site-store/skills/x/SKILL.md", "a-b_c.d"] {
            assert!(
                validate_pathspec(Path::new(path)).is_ok(),
                "{path:?} must be accepted"
            );
        }
    }

    #[test]
    fn status_records_parse_including_a_rename() {
        let raw = b"?? .site-store/\0M  src/a.rs\0R  new.rs\0old.rs\0";
        let entries = parse_status(&["status"], raw).expect("parse");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].code, "??");
        assert_eq!(entries[0].path, ".site-store/");
        assert!(!entries[0].is_staged());
        assert_eq!(entries[1].code, "M ");
        assert!(entries[1].is_staged());
        assert_eq!(entries[2].from.as_deref(), Some("old.rs"));
        assert!(entries[2].is_staged());
    }

    #[test]
    fn a_status_record_without_the_expected_shape_is_an_error() {
        let error = parse_status(&["status"], b"x\0").expect_err("short record");
        assert!(matches!(error, LocalGitError::InvalidOutput { .. }));
    }

    #[test]
    fn the_local_runner_uses_the_system_git_and_a_non_prompting_environment() {
        let runner = ProcessLocalGitRunner::git();
        assert_eq!(runner.program(), Path::new("git"));
        assert_eq!(runner.timeout, LOCAL_GIT_TIMEOUT);
        assert_eq!(runner.output_limit, MAX_GIT_OUTPUT_BYTES);
    }

    #[test]
    fn the_local_runner_caps_output_and_kills_an_overrunning_command() {
        let runner =
            ProcessLocalGitRunner::new("/bin/sh", Duration::from_secs(10)).with_output_limit(64);
        let output = runner
            .run(&LocalGitCall {
                repo: Path::new("/"),
                args: &["-c", "head -c 4096 /dev/zero"],
                stdin: None,
                literal_pathspecs: false,
            })
            .expect("sh runs");
        assert!(output.success);
        assert_eq!(output.stdout.len(), 64);

        let slow = ProcessLocalGitRunner::new("/bin/sleep", Duration::from_millis(150));
        let error = slow
            .run(&LocalGitCall {
                repo: Path::new("/"),
                args: &["10"],
                stdin: None,
                literal_pathspecs: false,
            })
            .expect_err("sleep 10 must time out");
        assert!(matches!(error, GitError::TimedOut(_)));
    }

    #[test]
    fn a_missing_directory_is_reported_before_git_runs() {
        let runner = ProcessLocalGitRunner::git();
        let dir = Path::new("/nonexistent-agentuna-test-dir");
        let error = detect_repository(&runner, dir).expect_err("must fail");
        assert!(matches!(error, LocalGitError::MissingDirectory { .. }));
    }
}
