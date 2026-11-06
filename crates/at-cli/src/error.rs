//! Machine-readable errors and their process exit-code mapping.
//!
//! TU-033 owns this table. The codes are published, not incidental: agents branch on
//! them, so a code is added and never renamed, and every documented code is exercised
//! by a golden test.

use serde_json::Value;

/// Process exit codes returned by the binary.
///
/// This is the published table. Every command exits with one of these codes; adding a
/// code means adding it to [`exit_code::TABLE`] and to the contract test's golden cases.
pub mod exit_code {
    /// The command succeeded.
    pub const SUCCESS: i32 = 0;
    /// The invocation was malformed: unknown command, unknown flag, missing argument.
    pub const USAGE: i32 = 2;
    /// The arguments parsed but failed validation, for example a malformed slug.
    pub const VALIDATION: i32 = 3;
    /// The requested item, project or file does not exist.
    pub const NOT_FOUND: i32 = 4;
    /// A lock held by another operation blocked the command.
    pub const LOCKED: i32 = 5;
    /// The write conflicted: stale preview, native file or concurrent modification.
    pub const CONFLICT: i32 = 6;
    /// Some files were written and the rest failed; the operation was rolled back.
    pub const PARTIAL: i32 = 7;
    /// `doctor` reported at least one error finding.
    pub const DOCTOR_FAILURE: i32 = 8;
    /// The command needs `--yes` or an interactive confirmation first.
    pub const CONFIRMATION_REQUIRED: i32 = 9;
    /// The command needs network access that policy or configuration denied.
    pub const NETWORK_REQUIRED: i32 = 10;
    /// The command is present in the tree but its implementation has not landed yet.
    ///
    /// A distinct code keeps an agent from reading a stub as a real result. It is
    /// retired as the owning stories (TU-027, TS-041, TM-027, TC-030/031, TA-070/071)
    /// replace the stub bodies.
    pub const NOT_IMPLEMENTED: i32 = 11;
    /// The CLI is misconfigured internally; the user's input is not at fault.
    pub const INTERNAL: i32 = 70;

    /// One row of the published exit-code table.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub struct ExitCodeDoc {
        /// The process exit status.
        pub code: i32,
        /// Stable short name, as used in tests and documentation.
        pub name: &'static str,
        /// What the code means, in one sentence.
        pub meaning: &'static str,
    }

    /// The published table, ascending by code.
    pub const TABLE: &[ExitCodeDoc] = &[
        ExitCodeDoc {
            code: SUCCESS,
            name: "success",
            meaning: "the command succeeded",
        },
        ExitCodeDoc {
            code: USAGE,
            name: "usage",
            meaning: "the invocation was malformed",
        },
        ExitCodeDoc {
            code: VALIDATION,
            name: "validation",
            meaning: "the arguments parsed but failed validation",
        },
        ExitCodeDoc {
            code: NOT_FOUND,
            name: "not_found",
            meaning: "the requested item, project or file does not exist",
        },
        ExitCodeDoc {
            code: LOCKED,
            name: "locked",
            meaning: "a lock held by another operation blocked the command",
        },
        ExitCodeDoc {
            code: CONFLICT,
            name: "conflict",
            meaning: "the write conflicted with a native file, a stale preview or a concurrent modification",
        },
        ExitCodeDoc {
            code: PARTIAL,
            name: "partial",
            meaning: "part of a multi-file write failed and was rolled back",
        },
        ExitCodeDoc {
            code: DOCTOR_FAILURE,
            name: "doctor_failure",
            meaning: "`doctor` reported at least one error finding",
        },
        ExitCodeDoc {
            code: CONFIRMATION_REQUIRED,
            name: "confirmation_required",
            meaning: "the command needs `--yes` or an interactive confirmation",
        },
        ExitCodeDoc {
            code: NETWORK_REQUIRED,
            name: "network_required",
            meaning: "the command needs network access that policy denied",
        },
        ExitCodeDoc {
            code: NOT_IMPLEMENTED,
            name: "not_implemented",
            meaning: "the command exists but its implementation is not in this build",
        },
        ExitCodeDoc {
            code: INTERNAL,
            name: "internal",
            meaning: "the CLI itself is misconfigured; the user's input is not at fault",
        },
    ];

    /// Whether `code` is one the CLI documents.
    #[must_use]
    pub fn is_documented(code: i32) -> bool {
        TABLE.iter().any(|row| row.code == code)
    }
}

/// Stable machine-readable error class, written to the JSON envelope.
///
/// The string form (`error.code`) is frozen by the TU-033 contract test: variants are
/// added, never renamed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The invocation was malformed. Pairs with [`exit_code::USAGE`].
    Usage,
    /// The arguments parsed but failed validation. Pairs with [`exit_code::VALIDATION`].
    Validation,
    /// The requested item does not exist. Pairs with [`exit_code::NOT_FOUND`].
    NotFound,
    /// A lock blocked the command. Pairs with [`exit_code::LOCKED`].
    Locked,
    /// The write conflicted with a native file, a stale preview or another writer.
    Conflict,
    /// Part of a multi-file write failed and was rolled back.
    Partial,
    /// `doctor` reported at least one error finding.
    DoctorFailure,
    /// The command needs a confirmation or `--yes` first.
    ConfirmationRequired,
    /// The command needs network access that policy denied.
    NetworkRequired,
    /// The command exists but has no implementation in this build.
    NotImplemented,
    /// An invariant of the CLI itself was violated.
    Internal,
}

impl ErrorCode {
    /// Every class, in table order.
    pub const ALL: &'static [ErrorCode] = &[
        Self::Usage,
        Self::Validation,
        Self::NotFound,
        Self::Locked,
        Self::Conflict,
        Self::Partial,
        Self::DoctorFailure,
        Self::ConfirmationRequired,
        Self::NetworkRequired,
        Self::NotImplemented,
        Self::Internal,
    ];

    /// The stable string written to `error.code`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Validation => "validation",
            Self::NotFound => "not_found",
            Self::Locked => "locked",
            Self::Conflict => "conflict",
            Self::Partial => "partial",
            Self::DoctorFailure => "doctor_failed",
            Self::ConfirmationRequired => "confirmation_required",
            Self::NetworkRequired => "network_required",
            Self::NotImplemented => "not_implemented",
            Self::Internal => "internal",
        }
    }

    /// The process exit code for this class.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Usage => exit_code::USAGE,
            Self::Validation => exit_code::VALIDATION,
            Self::NotFound => exit_code::NOT_FOUND,
            Self::Locked => exit_code::LOCKED,
            Self::Conflict => exit_code::CONFLICT,
            Self::Partial => exit_code::PARTIAL,
            Self::DoctorFailure => exit_code::DOCTOR_FAILURE,
            Self::ConfirmationRequired => exit_code::CONFIRMATION_REQUIRED,
            Self::NetworkRequired => exit_code::NETWORK_REQUIRED,
            Self::NotImplemented => exit_code::NOT_IMPLEMENTED,
            Self::Internal => exit_code::INTERNAL,
        }
    }
}

/// The hint substituted when a caller passes an empty one.
///
/// The envelope contract says `error.hint` is never empty, so the constructor enforces
/// it rather than trusting every future call site.
const FALLBACK_HINT: &str = "run `agentuna --help` for the accepted commands and flags";

/// A failure the CLI can render for a human and for a machine.
///
/// Every error carries a hint: TU-033 requires a non-empty `hint` in the JSON error
/// object, so an empty one is replaced by `FALLBACK_HINT` at construction.
///
/// Two optional payloads exist for report-style commands such as `doctor`, whose
/// failure still carries a machine-readable report: [`data`](Self::data) becomes the
/// envelope's `data` and [`report`](Self::report) is the human rendering written to
/// stdout before the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct CliError {
    code: ErrorCode,
    message: String,
    hint: String,
    details: Option<Value>,
    payload: Option<Box<FailurePayload>>,
}

/// The payload a report-style command attaches to a failure.
///
/// Boxed so a failure stays small enough for `Result<_, CliError>` to travel by value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FailurePayload {
    data: Value,
    human: Option<String>,
}

impl CliError {
    /// Creates an error with the given class, message and actionable hint.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        Self {
            code,
            message: message.into(),
            hint: if hint.trim().is_empty() {
                FALLBACK_HINT.to_owned()
            } else {
                hint
            },
            details: None,
            payload: None,
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

    /// Payload written to the envelope's `data` even though the command failed.
    #[must_use]
    pub fn data(&self) -> Option<&Value> {
        self.payload.as_ref().map(|payload| &payload.data)
    }

    /// Attaches the payload a report-style command produced before failing.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.payload_mut().data = data;
        self
    }

    /// The human rendering of the failed command's report, if it has one.
    #[must_use]
    pub fn report(&self) -> Option<&str> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.human.as_deref())
    }

    /// Attaches the human rendering written to stdout before the diagnostic.
    #[must_use]
    pub fn with_report(mut self, report: impl Into<String>) -> Self {
        self.payload_mut().human = Some(report.into());
        self
    }

    /// The payload, created on first use so both accessors can be chained.
    fn payload_mut(&mut self) -> &mut FailurePayload {
        self.payload.get_or_insert_with(|| {
            Box::new(FailurePayload {
                data: Value::Null,
                human: None,
            })
        })
    }
}
