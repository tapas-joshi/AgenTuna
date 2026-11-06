//! Machine-readable errors and their process exit-code mapping.

use serde_json::Value;

/// Process exit codes returned by the binary.
///
/// TU-033 owns the documented table; these constants are the in-tree encoding of it.
pub mod exit_code {
    /// The command succeeded.
    pub const SUCCESS: i32 = 0;
    /// The invocation was malformed: unknown command, unknown flag, missing argument.
    pub const USAGE: i32 = 2;
    /// The command is present in the tree but its implementation has not landed yet.
    ///
    /// A distinct code keeps an agent from reading a stub as a real result. It is
    /// retired as the owning stories (TU-027, TU-032, TS-041, TM-027, TC-030/031,
    /// TA-070/071) replace the stub bodies.
    pub const NOT_IMPLEMENTED: i32 = 11;
    /// The CLI is misconfigured internally; the user's input is not at fault.
    pub const INTERNAL: i32 = 70;
}

/// Stable machine-readable error class, written to the JSON envelope.
///
/// TU-033 owns the full table of codes; variants are added, never renamed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The invocation was malformed. Pairs with [`exit_code::USAGE`].
    Usage,
    /// The command exists but has no implementation in this build.
    NotImplemented,
    /// An invariant of the CLI itself was violated.
    Internal,
}

impl ErrorCode {
    /// The stable string written to `error.code`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "usage",
            ErrorCode::NotImplemented => "not_implemented",
            ErrorCode::Internal => "internal",
        }
    }

    /// The process exit code for this class.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Usage => exit_code::USAGE,
            ErrorCode::NotImplemented => exit_code::NOT_IMPLEMENTED,
            ErrorCode::Internal => exit_code::INTERNAL,
        }
    }
}

/// A failure the CLI can render for a human and for a machine.
///
/// Every error carries a hint: TU-033 requires a non-empty `hint` in the JSON error
/// object, so it is not optional here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct CliError {
    code: ErrorCode,
    message: String,
    hint: String,
    details: Option<Value>,
}

impl CliError {
    /// Creates an error with the given class, message and actionable hint.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            details: None,
        }
    }

    /// Builds the error returned by a command whose engine implementation is missing.
    #[must_use]
    pub fn not_implemented(command: &str, owner: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("`agentuna {command}` is not implemented in this build"),
            format!(
                "`{command}` is owned by {owner}; until it lands the command exits {}",
                exit_code::NOT_IMPLEMENTED
            ),
        )
    }

    /// Builds an error caused by a defect in the CLI rather than by user input.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::Internal,
            message,
            "this is a bug in agentuna; please report the command you ran",
        )
    }

    /// The error class.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// The process exit code for this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// The human-readable message, without decoration.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// What the reader can do next. Never empty.
    #[must_use]
    pub fn hint(&self) -> &str {
        &self.hint
    }

    /// Structured detail for machines, if any.
    #[must_use]
    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    /// Attaches structured detail.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}
