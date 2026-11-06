//! Output model: the versioned JSON envelope, warnings and the run's writers.
//!
//! The envelope shape is the TU-033 machine contract:
//! `{schema_version, command, ok, data, warnings, error}` with
//! `error = {code, message, details, hint}`. TU-033 owns future changes; commands build
//! their output here so exactly one encoder exists.

use std::io::Write;

use serde_json::{Value, json};

use crate::error::CliError;

/// Envelope schema version written into every JSON object.
///
/// Adding a field is allowed within version 1; removing or renaming one requires 2.
pub const SCHEMA_VERSION: u32 = 1;

/// Whether a run may write ANSI escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Emit ANSI escape sequences.
    Ansi,
    /// Plain text only.
    Plain,
}

/// A non-fatal note carried in the envelope's `warnings` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Stable machine code, for example `already_registered`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Warning {
    /// Creates a warning from a stable code and a message.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

/// The result of a successful command, rendered by the CLI root.
///
/// In JSON mode only [`data`](Self::data) and [`warnings`](Self::warnings) reach
/// stdout; otherwise [`human`](Self::human) is written verbatim, so a command that
/// emits a script or a table controls its own bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommandOutput {
    /// Payload for the envelope's `data` field.
    pub data: Value,
    /// Text written to stdout when `--json` is absent. Must end with its own newline.
    pub human: String,
    /// Warnings carried in the envelope.
    pub warnings: Vec<Warning>,
}

impl CommandOutput {
    /// Creates output with both a machine payload and a human rendering.
    #[must_use]
    pub fn new(data: Value, human: impl Into<String>) -> Self {
        Self {
            data,
            human: human.into(),
            warnings: Vec::new(),
        }
    }

    /// Creates output that only has a human rendering; `data` is JSON `null`.
    #[must_use]
    pub fn human_only(human: impl Into<String>) -> Self {
        Self::new(Value::Null, human)
    }

    /// Attaches warnings to this output.
    #[must_use]
    pub fn with_warnings(mut self, warnings: Vec<Warning>) -> Self {
        self.warnings = warnings;
        self
    }
}

/// Where one CLI run writes, and whether it may colour text.
///
/// The roots own stdout and stderr so tests can capture both without spawning.
pub struct Stdio<'a> {
    /// Machine output and human results.
    pub out: &'a mut dyn Write,
    /// Diagnostics, usage text and progress. Never parsed.
    pub err: &'a mut dyn Write,
    /// Colour policy resolved once at startup from `--no-color`, `NO_COLOR` and the
    /// terminal.
    pub color: Color,
}

/// Builds the success envelope for `command`.
#[must_use]
pub fn envelope_ok(command: &str, output: &CommandOutput) -> Value {
    let warnings: Vec<Value> = output.warnings.iter().map(Warning::to_json).collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "command": command,
        "ok": true,
        "data": output.data,
        "warnings": warnings,
        "error": Value::Null,
    })
}

/// Builds the failure envelope for `command`.
#[must_use]
pub fn envelope_err(command: &str, error: &CliError) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "command": command,
        "ok": false,
        "data": Value::Null,
        "warnings": Vec::<Value>::new(),
        "error": {
            "code": error.code().as_str(),
            "message": error.message(),
            "details": error.details().cloned().unwrap_or(Value::Null),
            "hint": error.hint(),
        },
    })
}
