//! Integration tests that run the real `agentuna` binary.
//!
//! These exercise the shipped process: exit codes, stream separation and the tree that
//! `--help` advertises.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const BUILT_IN_COMMANDS: &[&str] = &["projects", "scan", "doctor", "completions", "version"];

/// Every domain group TU-026 names, whether or not its owner has landed yet.
const DOMAIN_GROUPS: &[&str] = &[
    "skills",
    "mcp",
    "migrate",
    "adopt",
    "drift",
    "backups",
    "sync",
    "remote",
    "agents",
    "effective",
    "why",
    "overview",
    "rules",
];

fn agentuna(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentuna"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn agentuna")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

/// Command names from clap's `Commands:` block, in listing order.
fn listed_commands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line.trim() == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(name) = line.split_whitespace().next() {
            names.push(name.to_owned());
        }
    }
    names
}

fn registered_groups() -> Vec<&'static str> {
    at_cli::production_registry()
        .expect("the shipped registry builds")
        .names()
        .collect()
}

#[test]
fn help_lists_built_in_commands() {
    let output = agentuna(&["--help"]);
    assert_eq!(exit_code(&output), 0);
    assert!(stderr(&output).is_empty(), "help must not write to stderr");

    let names = listed_commands(&stdout(&output));
    for command in BUILT_IN_COMMANDS {
        assert!(
            names.iter().any(|name| name == command),
            "`--help` must list `{command}`; it listed {names:?}"
        );
    }
}

#[test]
fn help_lists_exactly_the_registered_groups() {
    let registry = registered_groups();
    let output = agentuna(&["--help"]);
    let names = listed_commands(&stdout(&output));

    for group in &registry {
        assert!(
            names.iter().any(|name| name == group),
            "registered group `{group}` must appear in `--help`; it listed {names:?}"
        );
    }

    for group in DOMAIN_GROUPS {
        if !registry.contains(group) {
            assert!(
                !names.iter().any(|name| name == group),
                "unregistered group `{group}` must be absent, not a stub"
            );
        }
    }

    // Nothing outside the owned commands, the registered groups and clap's `help`
    // may appear: the tree lists only what exists.
    let mut expected: Vec<&str> = BUILT_IN_COMMANDS.to_vec();
    expected.extend(registry.iter().copied());
    expected.push("help");
    for name in &names {
        assert!(
            expected.contains(&name.as_str()),
            "`--help` listed unexpected command `{name}`"
        );
    }
}

#[test]
fn help_documents_global_flags() {
    let help = stdout(&agentuna(&["--help"]));
    for flag in [
        "--json",
        "--project",
        "--scope",
        "--yes",
        "--verbose",
        "--no-color",
    ] {
        assert!(help.contains(flag), "`--help` must document `{flag}`");
    }
}

#[test]
fn unknown_subcommand_exits_two_with_usage_on_stderr() {
    let output = agentuna(&["does-not-exist"]);
    assert_eq!(exit_code(&output), 2);
    assert!(
        stdout(&output).is_empty(),
        "usage errors never touch stdout"
    );
    let err = stderr(&output);
    assert!(err.contains("Usage:"), "stderr must carry usage: {err}");
    assert!(err.contains("unrecognized subcommand"), "stderr: {err}");
}

#[test]
fn unknown_flag_exits_two_with_usage_on_stderr() {
    let output = agentuna(&["version", "--does-not-exist"]);
    assert_eq!(exit_code(&output), 2);
    assert!(stdout(&output).is_empty());
    let err = stderr(&output);
    assert!(err.contains("Usage:"), "stderr must carry usage: {err}");
    assert!(err.contains("unexpected argument"), "stderr: {err}");
}

#[test]
fn missing_required_argument_exits_two_with_usage_on_stderr() {
    let output = agentuna(&["projects", "add"]);
    assert_eq!(exit_code(&output), 2);
    assert!(stdout(&output).is_empty());
    let err = stderr(&output);
    assert!(err.contains("Usage:"), "stderr must carry usage: {err}");
    assert!(err.contains("required"), "stderr: {err}");
}

#[test]
fn bare_invocation_exits_two_with_help() {
    let output = agentuna(&[]);
    assert_eq!(exit_code(&output), 2);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("Usage:"));
}

#[test]
fn global_flags_parse_before_and_after_the_subcommand() {
    for args in [
        vec!["--json", "version"],
        vec!["version", "--json"],
        vec!["--json", "--scope", "local", "version"],
        vec!["version", "--json", "--scope", "local"],
        vec!["--project", "/tmp/agentuna-test", "version", "--json"],
        vec!["version", "--yes", "--json"],
        vec!["version", "--no-color", "--json"],
    ] {
        let output = agentuna(&args);
        assert_eq!(
            exit_code(&output),
            0,
            "args {args:?} failed: {}",
            stderr(&output)
        );
        let envelope: serde_json::Value =
            serde_json::from_str(&stdout(&output)).unwrap_or_else(|error| {
                panic!(
                    "args {args:?} did not emit JSON ({error}): {}",
                    stdout(&output)
                )
            });
        assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
        assert_eq!(envelope["schema_version"], 1);
    }
}

#[test]
fn verbose_goes_to_stderr_only() {
    let output = agentuna(&["--verbose", "version"]);
    assert_eq!(exit_code(&output), 0);
    assert!(
        stderr(&output).contains("[verbose]"),
        "--verbose must report on stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        stdout(&output).trim(),
        format!("agentuna {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn invalid_scope_is_a_usage_error() {
    let output = agentuna(&["version", "--scope", "galaxy"]);
    assert_eq!(exit_code(&output), 2);
    let err = stderr(&output);
    assert!(err.contains("invalid value"), "stderr: {err}");
    assert!(
        err.contains("global"),
        "stderr should list valid scopes: {err}"
    );
}

#[test]
fn version_subcommand_prints_name_and_version() {
    let output = agentuna(&["version"]);
    assert_eq!(exit_code(&output), 0);
    assert_eq!(
        stdout(&output).trim(),
        format!("agentuna {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn unimplemented_commands_fail_with_a_distinct_exit_code() {
    // `doctor` left this list with TU-032: it now runs checks, so its sandboxed tests
    // live in tests/doctor.rs where the real home is never touched.
    for args in [
        vec!["projects", "list"],
        vec!["projects", "add", "/tmp/agentuna-test"],
        vec!["projects", "remove", "demo"],
        vec!["projects", "use", "demo"],
        vec!["scan"],
        vec!["scan", "--depth", "3"],
    ] {
        let output = agentuna(&args);
        assert_eq!(
            exit_code(&output),
            11,
            "args {args:?} must exit 11 (not implemented); stderr {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "human errors go to stderr");
        let err = stderr(&output);
        assert!(err.contains("not implemented"), "stderr: {err}");
        assert!(err.contains("hint:"), "stderr must carry a hint: {err}");
    }
}

#[test]
fn unimplemented_commands_carry_the_error_envelope_in_json_mode() {
    let output = agentuna(&["scan", "--json"]);
    assert_eq!(exit_code(&output), 11);
    assert_eq!(
        stdout(&output).lines().count(),
        1,
        "JSON mode writes exactly one line: {}",
        stdout(&output)
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("valid JSON envelope");

    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["command"], "scan");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "not_implemented");
    assert!(
        envelope["error"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty())
    );
    assert!(stderr(&output).contains("error:"));
}

#[test]
fn json_usage_errors_still_exit_two() {
    let output = agentuna(&["does-not-exist", "--json"]);
    assert_eq!(exit_code(&output), 2);
    assert!(stderr(&output).contains("Usage:"));
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("usage failures also carry an envelope");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "usage");
}

#[test]
fn completions_are_accepted_by_the_shell_parser() {
    for shell in ["zsh", "bash"] {
        let output = agentuna(&["completions", shell]);
        assert_eq!(exit_code(&output), 0, "completions {shell} failed");
        let script = output.stdout;
        assert!(!script.is_empty(), "completions {shell} wrote nothing");

        match shell_syntax_check(shell, &script) {
            Some(valid) => assert!(valid, "`{shell} -n` rejected the generated script"),
            None => eprintln!("skipping `{shell} -n` check: {shell} is not installed"),
        }

        let text = String::from_utf8_lossy(&script);
        for command in BUILT_IN_COMMANDS {
            assert!(
                text.contains(command),
                "completions {shell} must mention `{command}`"
            );
        }
        for group in registered_groups() {
            assert!(
                text.contains(group),
                "completions {shell} must mention registered group `{group}`"
            );
        }
    }
}

#[test]
fn completions_have_a_json_payload() {
    let output = agentuna(&["completions", "zsh", "--json"]);
    assert_eq!(exit_code(&output), 0);
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("valid JSON envelope");
    assert_eq!(envelope["command"], "completions");
    assert_eq!(envelope["data"]["shell"], "zsh");
    let script = envelope["data"]["script"]
        .as_str()
        .expect("script is a string");
    assert!(script.contains("agentuna"));
}

/// Runs `<shell> -n` over `script`. `None` means the shell is not installed.
fn shell_syntax_check(shell: &str, script: &[u8]) -> Option<bool> {
    let mut child = Command::new(shell)
        .arg("-n")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(script).ok()?;
    let output = child.wait_with_output().ok()?;
    Some(output.status.success())
}
