//! TM-030 scheduler fixture: a minimal MCP server over stdio with a control file.
//!
//! The test writes `pass`, `fail`, `hang` or `partial` into the control file and the
//! server re-reads it for every request, so one spawned process can be driven through
//! all four behaviours. It is a test binary only (`--features testkit`), never shipped.
//!
//! * `--control PATH` — file whose trimmed content selects the behaviour (default `pass`).
//! * `--report PATH` — written at startup with `pid=` and, when `--child-sleep` is set,
//!   `child=` so the quit test can prove the whole process tree is gone.
//! * `--child-sleep` — start a `sleep` child that inherits the process group.
//!
//! Behaviours: `pass` answers `initialize` and `tools/list`; `fail` answers `tools/list`
//! with a JSON-RPC error; `partial` writes a truncated JSON line; `hang` never answers.

// The fixture is test tooling, not library code; the workspace lint that forbids
// `expect`/`panic` in production does not apply to this binary.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug, Default)]
struct Options {
    control: Option<PathBuf>,
    report: Option<PathBuf>,
    child_sleep: bool,
}

fn parse_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("missing value for flag");
        match arg.as_str() {
            "--control" => options.control = Some(PathBuf::from(value())),
            "--report" => options.report = Some(PathBuf::from(value())),
            "--child-sleep" => options.child_sleep = true,
            other => panic!("unknown flag {other}"),
        }
    }
    options
}

/// Current behaviour: the trimmed control-file content, or `pass` while it is absent.
fn mode(control: Option<&Path>) -> String {
    control
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or_else(|| "pass".to_owned(), |text| text.trim().to_owned())
}

fn write_report(options: &Options, child: Option<u32>) {
    let Some(path) = &options.report else {
        return;
    };
    let Ok(file) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    else {
        return;
    };
    let mut file = std::io::BufWriter::new(file);
    let _ = writeln!(file, "pid={}", std::process::id());
    if let Some(pid) = child {
        let _ = writeln!(file, "child={pid}");
    }
    let _ = file.flush();
}

fn respond(stdout: &mut impl Write, id: Value, result: Value) {
    let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn serve(options: &Options) {
    let control = options.control.as_deref();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let behaviour = mode(control);
        if behaviour == "hang" {
            // Never answer, never exit: only the scheduler deadline or a signal ends this.
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(id) = message.get("id").cloned() else {
            // `notifications/initialized` needs no answer.
            continue;
        };

        match method.as_str() {
            "initialize" => {
                let version = message
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-06-18");
                respond(
                    &mut stdout,
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "at-app-health-fixture", "version": "0.1.0" },
                    }),
                );
            }
            "tools/list" if behaviour == "fail" => {
                let message = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": "fixture failure" },
                });
                let _ = writeln!(stdout, "{message}");
                let _ = stdout.flush();
            }
            "tools/list" if behaviour == "partial" => {
                // A well-formed envelope whose result is missing the payload the protocol
                // requires: a partial response the client must report as a failure.
                let message = json!({ "jsonrpc": "2.0", "id": id, "result": {} });
                let _ = writeln!(stdout, "{message}");
                let _ = stdout.flush();
            }
            "tools/list" => {
                respond(
                    &mut stdout,
                    id,
                    json!({ "tools": [{ "name": "fixture-tool", "inputSchema": { "type": "object" } }] }),
                );
            }
            "ping" => respond(&mut stdout, id, json!({})),
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
}

fn main() {
    let options = parse_options();
    let child = if options.child_sleep {
        Command::new("sleep")
            .arg("300")
            .spawn()
            .ok()
            .map(|child| child.id())
    } else {
        None
    };
    write_report(&options, child);
    serve(&options);
}
