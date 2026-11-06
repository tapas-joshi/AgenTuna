//! A deliberately minimal MCP server that speaks newline-delimited JSON-RPC over stdio.
//!
//! It exists only for `at-mcp`'s session acceptance tests (`--features testkit`) and is
//! never part of a shipped AgenTuna binary. Everything a test needs to observe is written
//! to files, because the process is expected to die:
//!
//! * `--report PATH` writes `pid`, `pgid`, `cwd` and every environment entry as
//!   `env.NAME=value` lines;
//! * `--signal-log PATH` appends `SIGINT` / `SIGTERM` as they arrive;
//! * `--request-log PATH` appends each request method, so a test can count pages;
//! * `--grandchild` starts a long `sleep` that inherits the process group and never
//!   leaves on its own, proving a group signal reaches it;
//! * `--stderr-bytes N` floods stderr before reading a single request;
//! * `--init-delay-ms N` stalls the `initialize` response;
//! * `--signals exit-on-any|ignore-int|ignore-all` chooses how it reacts to signals;
//! * `--tools N --page-size N` control `tools/list` pagination.

// The fake server is test tooling, not library code; the workspace lint that forbids
// `expect`/`panic` in production does not apply to this binary.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalMode {
    ExitOnAny,
    IgnoreInterrupt,
    IgnoreAll,
}

#[derive(Debug)]
struct Options {
    report: Option<PathBuf>,
    signal_log: Option<PathBuf>,
    request_log: Option<PathBuf>,
    tools: usize,
    page_size: usize,
    stderr_bytes: usize,
    init_delay: Duration,
    signals: SignalMode,
    grandchild: bool,
    protocol_version: Option<String>,
    exit_after_init: Option<i32>,
    exit_before_init: Option<i32>,
    ignore_eof: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            report: None,
            signal_log: None,
            request_log: None,
            tools: 0,
            page_size: 100,
            stderr_bytes: 0,
            init_delay: Duration::ZERO,
            signals: SignalMode::ExitOnAny,
            grandchild: false,
            exit_after_init: None,
            exit_before_init: None,
            protocol_version: None,
            ignore_eof: false,
        }
    }
}

fn parse_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("missing value for flag");
        match arg.as_str() {
            "--report" => options.report = Some(PathBuf::from(value())),
            "--signal-log" => options.signal_log = Some(PathBuf::from(value())),
            "--request-log" => options.request_log = Some(PathBuf::from(value())),
            "--tools" => options.tools = value().parse().expect("tools count"),
            "--page-size" => options.page_size = value().parse().expect("page size"),
            "--stderr-bytes" => options.stderr_bytes = value().parse().expect("stderr bytes"),
            "--init-delay-ms" => {
                options.init_delay = Duration::from_millis(value().parse().expect("delay"));
            }
            "--signals" => {
                options.signals = match value().as_str() {
                    "exit-on-any" => SignalMode::ExitOnAny,
                    "ignore-int" => SignalMode::IgnoreInterrupt,
                    "ignore-all" => SignalMode::IgnoreAll,
                    other => panic!("unknown signal mode {other}"),
                };
            }
            "--grandchild" => options.grandchild = true,
            "--exit-after-init" => {
                options.exit_after_init = Some(value().parse().expect("exit code"));
            }
            "--protocol-version" => options.protocol_version = Some(value()),
            "--exit-before-init" => {
                options.exit_before_init = Some(value().parse().expect("exit code"));
            }
            "--ignore-eof" => options.ignore_eof = true,
            other => panic!("unknown flag {other}"),
        }
    }
    if options.page_size == 0 {
        options.page_size = 100;
    }
    options
}

fn append_line(path: &PathBuf, line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn install_signal_log(path: Option<PathBuf>, mode: SignalMode) {
    let mut signals = match signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ]) {
        Ok(signals) => signals,
        Err(_) => return,
    };
    std::thread::spawn(move || {
        for signal in signals.forever() {
            let name = match signal {
                signal_hook::consts::SIGINT => "SIGINT",
                signal_hook::consts::SIGTERM => "SIGTERM",
                other => return_trace(other),
            };
            if let Some(path) = &path {
                append_line(path, name);
            }
            let exit = !matches!(
                (mode, signal),
                (SignalMode::IgnoreAll, _)
                    | (SignalMode::IgnoreInterrupt, signal_hook::consts::SIGINT)
            );
            if exit {
                std::process::exit(0);
            }
        }
    });
}

/// Keeps the match exhaustive without inventing names for signals the tests never send.
fn return_trace(signal: i32) -> &'static str {
    let _ = signal;
    "UNKNOWN"
}

fn start_grandchild() -> Option<u32> {
    Command::new("sleep")
        .arg("300")
        .spawn()
        .ok()
        .map(|child| child.id())
}

fn write_report(options: &Options, grandchild: Option<u32>) {
    let Some(path) = &options.report else {
        return;
    };
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let mut file = std::io::BufWriter::new(file);
    let _ = writeln!(file, "pid={}", std::process::id());
    let _ = writeln!(file, "pgid={}", rustix::process::getpgrp().as_raw_pid());
    if let Some(pid) = grandchild {
        let _ = writeln!(file, "grandchild={pid}");
    }
    if let Ok(cwd) = std::env::current_dir() {
        let _ = writeln!(file, "cwd={}", cwd.display());
    }
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.sort();
    for (name, value) in env {
        let _ = writeln!(file, "env.{name}={value}");
    }
    let _ = file.flush();
}

fn flood_stderr(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let chunk = vec![b'e'; 4096];
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    let mut written = 0;
    while written < bytes {
        let take = chunk.len().min(bytes - written);
        if stderr.write_all(&chunk[..take]).is_err() {
            return;
        }
        written += take;
    }
    let _ = writeln!(stderr, "stderr-flood-complete");
    let _ = stderr.flush();
}

fn serve(options: &Options) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(path) = &options.request_log {
            append_line(path, &method);
        }
        let Some(id) = message.get("id").cloned() else {
            // A notification: `notifications/initialized` needs no answer.
            continue;
        };

        match method.as_str() {
            "initialize" => {
                if !options.init_delay.is_zero() {
                    std::thread::sleep(options.init_delay);
                }
                let version = options.protocol_version.as_deref().unwrap_or_else(|| {
                    message
                        .pointer("/params/protocolVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("2025-06-18")
                });
                respond(
                    &mut stdout,
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "at-mcp-fake-server", "version": "0.1.0" },
                    }),
                );
                if let Some(code) = options.exit_after_init {
                    let _ = stdout.flush();
                    std::process::exit(code);
                }
            }
            "ping" => respond(&mut stdout, id, json!({})),
            "tools/list" => {
                let cursor = message
                    .pointer("/params/cursor")
                    .and_then(Value::as_str)
                    .and_then(|cursor| cursor.parse::<usize>().ok())
                    .unwrap_or(0);
                let start = cursor.min(options.tools);
                let end = start.saturating_add(options.page_size).min(options.tools);
                let tools: Vec<Value> = (start..end)
                    .map(|index| {
                        json!({
                            "name": format!("tool-{index:03}"),
                            "description": format!("fake tool {index}"),
                            "inputSchema": { "type": "object", "properties": {} },
                        })
                    })
                    .collect();
                let mut result = json!({ "tools": tools });
                if end < options.tools {
                    result["nextCursor"] = json!(end.to_string());
                }
                respond(&mut stdout, id, result);
            }
            _ => {
                let message = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "method not found" },
                });
                let _ = writeln!(stdout, "{message}");
                let _ = stdout.flush();
            }
        }
    }

    // A server that will not exit because stdin closed: the tests use this to model a
    // process that only a signal can stop.
    if options.ignore_eof {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn respond(stdout: &mut impl Write, id: Value, result: Value) {
    let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn main() {
    let options = parse_options();
    // Always handle SIGINT/SIGTERM so `--signals` alone is enough to model a server
    // that ignores them; the log is optional.
    install_signal_log(options.signal_log.clone(), options.signals);
    let grandchild = if options.grandchild {
        start_grandchild()
    } else {
        None
    };
    flood_stderr(options.stderr_bytes);
    write_report(&options, grandchild);
    if let Some(code) = options.exit_before_init {
        std::process::exit(code);
    }
    serve(&options);
}
