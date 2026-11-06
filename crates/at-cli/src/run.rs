//! Composition root: build the tree, parse, dispatch, render, map to an exit code.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::{ArgMatches, FromArgMatches};
use serde_json::json;

use crate::cli::Cli;
use crate::error::{CliError, ErrorCode, exit_code};
use crate::globals::GlobalArgs;
use crate::output::{Color, CommandOutput, Stdio, envelope_err, envelope_ok};
use crate::registry::{Registry, RegistryError};

/// The registry the shipped binary uses.
///
/// Only groups whose implementation has landed are listed, so an unregistered noun is
/// absent from `--help` instead of being a stub. Group owners: `config` -> TF-028,
/// TF-051; `skills` -> TS-041; `mcp` and nested `mcp secret` -> TM-027; `migrate`,
/// `adopt`, `drift`, `backups`, `sync`, `remote` -> TC-030, TC-031; `agents`,
/// `effective`, `why`, `overview`, `rules` -> TA-070, TA-071.
pub fn production_registry() -> Result<Registry, RegistryError> {
    Registry::try_new(vec![
        Box::new(crate::runtime::ConfigGroup),
        Box::new(crate::mcp::McpGroup),
        Box::new(crate::agents::AgentsGroup),
        Box::new(crate::effective::EffectiveGroup),
        Box::new(crate::why::WhyGroup),
    ])
}

/// Runs the CLI for the current process and returns its exit status.
#[must_use]
pub fn run_from_env() -> ExitCode {
    init_logging();
    let argv: Vec<OsString> = std::env::args_os().collect();
    let mut out = io::stdout().lock();
    let mut err = io::stderr().lock();
    let color = resolve_color(&argv, io::stderr().is_terminal());

    let mut stdio = Stdio {
        out: &mut out,
        err: &mut err,
        color,
    };
    let code = match production_registry() {
        Ok(registry) => run(&registry, &argv, &mut stdio),
        Err(error) => {
            // A broken composition root is a CLI defect, not user input, but it still
            // goes through the shared envelope so JSON consumers get one object.
            let failure = CliError::internal(format!("the command registry is invalid: {error}"));
            report_error("", &failure, flag_present(&argv, "--json"), &mut stdio)
        }
    };
    // Every documented code fits in a `u8`; the fallback keeps the promise that only
    // documented codes are ever returned.
    ExitCode::from(u8::try_from(code).unwrap_or(70))
}

/// Runs `argv` against `registry`, writing to `stdio`, and returns the process exit code.
#[must_use]
pub fn run(registry: &Registry, argv: &[OsString], stdio: &mut Stdio<'_>) -> i32 {
    let command = registry.command();
    let matches = match command.try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error) => return report_parse_error(&error, argv, stdio),
    };

    let path = command_path(&matches);
    let globals = match GlobalArgs::from_arg_matches(&matches) {
        Ok(globals) => globals,
        Err(error) => {
            let failure = CliError::internal(format!("cannot read the global flags: {error}"));
            return report_error(&path, &failure, false, stdio);
        }
    };

    if globals.verbose > 0 {
        let project = globals
            .project
            .as_ref()
            .map_or_else(|| "-".to_owned(), |path| path.display().to_string());
        let scope = globals.scope.map_or("unset", |scope| scope.as_str());
        let _ = writeln!(
            stdio.err,
            "[verbose] command={path} project={project} scope={scope} json={}",
            globals.json
        );
    }

    let Some((noun, sub_matches)) = matches.subcommand() else {
        let failure = CliError::internal("no subcommand remained after a successful parse");
        return report_error(&path, &failure, globals.json, stdio);
    };

    if let Some(group) = registry.get(noun) {
        return match group.dispatch(&globals, sub_matches) {
            Ok(output) => report_ok(&path, &output, globals.json, stdio),
            Err(failure) => report_error(&path, &failure, globals.json, stdio),
        };
    }

    match Cli::from_arg_matches(&matches) {
        Ok(cli) => match crate::builtin::dispatch(&cli, registry) {
            Ok(output) => report_ok(&path, &output, globals.json, stdio),
            Err(failure) => report_error(&path, &failure, globals.json, stdio),
        },
        Err(error) => {
            let failure = CliError::internal(format!("cannot read the parsed command: {error}"));
            report_error(&path, &failure, globals.json, stdio)
        }
    }
}

/// The subcommand path the user typed, used as the envelope's `command`.
fn command_path(matches: &ArgMatches) -> String {
    let mut parts = Vec::new();
    let mut current = Some(matches);
    while let Some(matches) = current {
        let Some((name, sub)) = matches.subcommand() else {
            break;
        };
        parts.push(name);
        current = Some(sub);
    }
    parts.join(" ")
}

fn report_ok(path: &str, output: &CommandOutput, json: bool, stdio: &mut Stdio<'_>) -> i32 {
    let text = if json {
        match serde_json::to_string(&envelope_ok(path, output)) {
            Ok(envelope) => envelope + "\n",
            Err(error) => {
                let failure = CliError::internal(format!("cannot encode JSON output: {error}"));
                return report_error(path, &failure, false, stdio);
            }
        }
    } else {
        write_warnings(output, stdio);
        output.human.clone()
    };

    // A failed write means the reader closed the pipe; the result was already produced.
    let _ = stdio.out.write_all(text.as_bytes());
    let _ = stdio.out.flush();
    exit_code::SUCCESS
}

/// Human mode has no envelope, so warnings are reported on stderr and stdout stays
/// exactly the command's own text. A report-style command whose rendering already
/// lists them opts out with [`CommandOutput::reporting_warnings`].
fn write_warnings(output: &CommandOutput, stdio: &mut Stdio<'_>) {
    if output.warnings_in_report {
        return;
    }
    for warning in &output.warnings {
        let _ = match stdio.color {
            Color::Ansi => writeln!(
                stdio.err,
                "\u{1b}[1;33mwarning\u{1b}[0m: {}",
                warning.message
            ),
            Color::Plain => writeln!(stdio.err, "warning: {}", warning.message),
        };
    }
}

fn report_error(path: &str, error: &CliError, json: bool, stdio: &mut Stdio<'_>) -> i32 {
    if json {
        emit_json_error(path, error, stdio);
    } else if let Some(report) = error.report() {
        // A report-style command such as `doctor` still shows its report to a human;
        // only the summary and the hint go to stderr.
        let _ = stdio.out.write_all(report.as_bytes());
    }
    write_diagnostic(error, stdio);
    error.exit_code()
}

fn emit_json_error(path: &str, error: &CliError, stdio: &mut Stdio<'_>) {
    match serde_json::to_string(&envelope_err(path, error)) {
        Ok(envelope) => {
            let _ = writeln!(stdio.out, "{envelope}");
        }
        Err(encode) => {
            let _ = writeln!(stdio.err, "error: cannot encode JSON output: {encode}");
        }
    }
}

fn write_diagnostic(error: &CliError, stdio: &mut Stdio<'_>) {
    match stdio.color {
        Color::Ansi => {
            let _ = writeln!(stdio.err, "\u{1b}[1;31merror\u{1b}[0m: {}", error.message());
            let _ = writeln!(stdio.err, "  \u{1b}[2mhint\u{1b}[0m: {}", error.hint());
        }
        Color::Plain => {
            let _ = writeln!(stdio.err, "error: {}", error.message());
            let _ = writeln!(stdio.err, "hint: {}", error.hint());
        }
    }
}

/// Renders a clap parse result.
///
/// Help and version are documentation requests: exit 0 and text on stdout. In JSON
/// mode they still produce exactly one object, with the text in `data.text`, so a
/// machine consumer never sees a bare stream of prose. Usage errors exit 2 on stderr,
/// as the acceptance contract requires.
fn report_parse_error(error: &clap::Error, argv: &[OsString], stdio: &mut Stdio<'_>) -> i32 {
    let code = error.exit_code();
    let text = render_clap(error, stdio.color);
    let json = flag_present(argv, "--json");

    if code == exit_code::SUCCESS {
        if json {
            let command = documentation_command(error, argv);
            let output = CommandOutput::new(json!({ "text": text }), String::new());
            return report_ok(&command, &output, true, stdio);
        }
        let _ = stdio.out.write_all(text.as_bytes());
        return code;
    }
    if is_mcp_secret_set(argv) {
        let failure = CliError::new(
            ErrorCode::Usage,
            "secret values must be supplied on stdin, never as arguments",
            "pipe the value to `agentuna mcp secret set <NAME>`",
        );
        write_diagnostic(&failure, stdio);
        if json {
            emit_json_error("mcp secret set", &failure, stdio);
        }
        return exit_code::USAGE;
    }

    let _ = stdio.err.write_all(text.as_bytes());
    if json {
        let first = text.lines().next().unwrap_or("invalid invocation");
        let message = first.strip_prefix("error: ").unwrap_or(first).to_owned();
        let failure = CliError::new(
            ErrorCode::Usage,
            message,
            "run `agentuna --help` to see the accepted commands and flags",
        );
        emit_json_error(&guess_command(argv), &failure, stdio);
    }
    code
}

fn is_mcp_secret_set(argv: &[OsString]) -> bool {
    argv.windows(3).any(|window| {
        window[0].to_str() == Some("mcp")
            && window[1].to_str() == Some("secret")
            && window[2].to_str() == Some("set")
    })
}

/// The noun a help or version request names, for the JSON envelope.
fn documentation_command(error: &clap::Error, argv: &[OsString]) -> String {
    let guessed = guess_command(argv);
    if !guessed.is_empty() {
        return guessed;
    }
    match error.kind() {
        clap::error::ErrorKind::DisplayVersion => "version",
        _ => "help",
    }
    .to_owned()
}

fn render_clap(error: &clap::Error, color: Color) -> String {
    let styled = error.render();
    let mut text = String::new();
    let _ = match color {
        Color::Ansi => write!(text, "{}", styled.ansi()),
        Color::Plain => write!(text, "{styled}"),
    };
    text
}

/// Best-effort command name for a failed parse, where no `ArgMatches` exist.
fn guess_command(argv: &[OsString]) -> String {
    argv.iter()
        .skip(1)
        .filter_map(|arg| arg.to_str())
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or("")
        .to_owned()
}

fn flag_present(argv: &[OsString], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

/// Colour is on only for a terminal that neither `--no-color` nor `NO_COLOR` silenced.
fn resolve_color(argv: &[OsString], stderr_is_terminal: bool) -> Color {
    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    if flag_present(argv, "--no-color") || no_color_env || !stderr_is_terminal {
        Color::Plain
    } else {
        Color::Ansi
    }
}

/// Installs process logging for the CLI: same daily file, filter and redaction as the app.
///
/// The project flag is parsed later, but the log directory lives under the support root,
/// which does not depend on it, so paths resolve without a project root here. The level
/// comes from `RUST_LOG` or the `[advanced] log_level` setting (default `info`); a config
/// that cannot be read degrades to `info` — logging must never block a command.
///
/// `--read-only` and `--dry-run` runs keep their "writes nothing" contract: file logging
/// is disabled and events go to stderr only. The appender itself is lazy, so even a
/// normal run creates the log directory on the first event, not at startup.
fn init_logging() {
    use at_store::config::LogLevel;
    use at_store::logging::{LoggingOptions, init};
    use at_store::paths::PathEnvironment;

    let Ok(paths) = at_store::paths::AppPaths::resolve(&PathEnvironment::from_process()) else {
        return;
    };
    let level = at_store::config::RuntimeConfig::load(
        paths.support_dir().join(at_store::config::CONFIG_FILE_NAME),
        at_store::config::ConfigInput {
            create: false,
            ..at_store::config::ConfigInput::production(Vec::new())
        },
    )
    .map(|config| config.advanced().log_level)
    .unwrap_or(LogLevel::Info);
    let mut options = LoggingOptions::for_paths(&paths, level);
    options.file_logging = !read_only_invocation();
    let _guard = init(&options);
    // The guard is intentionally dropped: the global subscriber outlives it and the CLI
    // process exits right after `run` returns.
}

/// Whether the raw argv asks for a run that must not write anything.
///
/// Clap parses the real flags later; this scan only looks for the two global boolean
/// flags that forbid writes. A token equal to a flag name can only be the flag itself —
/// values are paths or scopes and never start with `--`.
fn read_only_invocation() -> bool {
    std::env::args_os()
        .skip(1)
        .any(|arg| matches!(arg.to_str(), Some("--read-only" | "--dry-run")))
}
