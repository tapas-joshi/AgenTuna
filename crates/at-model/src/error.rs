//! Error kinds, stable codes, remedies and the report every layer returns.
//!
//! [`ErrorReport`] is plain data. The engine builds it, the CLI serialises it for
//! `--json` (code, message, user message and the remedy ids) and the app renders
//! [`ErrorReport::user_message`] in a toast plus every [`Remedy`] as an action.
//!
//! A report never holds a resolved secret. When a caller may have interpolated a
//! secret value into a message, path or source chain, it calls
//! [`ErrorReport::redact_with`] (or [`ErrorReport::redact_values`]) before the
//! report leaves the process.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::ser::SerializeMap;

use crate::wire::wire_enum;

/// Replacement text for a redacted value.
pub const REDACTION_MASK: &str = "***";

/// Masks known secret values in text.
///
/// `at-secrets::Redactor` implements this over the values resolved for the current
/// session. at-model only applies it, so no resolved value is ever stored in a
/// model type.
pub trait SecretRedactor {
    /// Returns `text` with every secret value it knows replaced by [`REDACTION_MASK`].
    fn redact(&self, text: &str) -> String;
}

wire_enum! {
    /// Machine-stable classification of a failure.
    ///
    /// The wire name is the code the CLI prints in `--json` output and that log
    /// lines and diagnostic exports carry. Codes never change once shipped;
    /// new kinds get new codes.
    #[non_exhaustive]
    pub enum ErrorKind {
        /// Any other input/output failure.
        Io => "E_IO",
        /// The operating system or an ownership rule denied access.
        PermissionDenied => "E_PERMISSION_DENIED",
        /// The requested file, folder or entry does not exist.
        NotFound => "E_NOT_FOUND",
        /// Content could not be parsed or had an unexpected shape.
        InvalidFormat => "E_INVALID_FORMAT",
        /// The change collides with an existing item or path.
        Conflict => "E_CONFLICT",
        /// A file changed between reading and writing it.
        ConcurrentModification => "E_CONCURRENT_MODIFICATION",
        /// The destination exists and AgenTuna does not manage it, so nothing was written.
        RefusedOverwrite => "E_REFUSED_OVERWRITE",
        /// Another operation holds the item or scope.
        Locked => "E_LOCKED",
        /// The action is not available on this operating system.
        UnsupportedPlatform => "E_UNSUPPORTED_PLATFORM",
        /// A network request failed.
        Network => "E_NETWORK",
        /// Authentication or authorization failed.
        Auth => "E_AUTH",
        /// A required secret is missing from the credential store.
        SecretMissing => "E_SECRET_MISSING",
        /// An update artifact failed signature or digest verification.
        UpdateSignatureInvalid => "E_UPDATE_SIGNATURE_INVALID",
        /// A bug or unexpected state inside AgenTuna.
        Internal => "E_INTERNAL",
        /// The user or the caller cancelled the operation.
        Cancelled => "E_CANCELLED",
    }
}

impl ErrorKind {
    /// The stable code, for example `E_PERMISSION_DENIED`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.wire_name()
    }

    /// Looks a code up. Unknown codes return `None`.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        code.parse().ok()
    }

    /// Short technical title used when a report has no message of its own.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Io => "I/O error",
            Self::PermissionDenied => "Permission denied",
            Self::NotFound => "Not found",
            Self::InvalidFormat => "Invalid format",
            Self::Conflict => "Conflict",
            Self::ConcurrentModification => "Concurrent modification",
            Self::RefusedOverwrite => "Refused to overwrite",
            Self::Locked => "Locked",
            Self::UnsupportedPlatform => "Unsupported platform",
            Self::Network => "Network error",
            Self::Auth => "Authentication failed",
            Self::SecretMissing => "Secret missing",
            Self::UpdateSignatureInvalid => "Invalid update signature",
            Self::Internal => "Internal error",
            Self::Cancelled => "Cancelled",
        }
    }

    /// Whether the same operation may succeed if the user retries it.
    #[must_use]
    pub const fn default_retryable(self) -> bool {
        matches!(
            self,
            Self::Io | Self::Network | Self::Locked | Self::ConcurrentModification
        )
    }

    /// The actions the app offers when a report does not name its own.
    ///
    /// [`ErrorKind::RefusedOverwrite`] always offers Adopt, Move aside and Cancel;
    /// [`ErrorKind::Cancelled`] offers none.
    #[must_use]
    pub const fn default_remedies(self) -> &'static [Remedy] {
        use Remedy::{Adopt, Cancel, CopyDetails, MoveAside, OpenSettings, Retry};
        use Remedy::{Reauthenticate, RevealInFinder};

        match self {
            Self::Io => &[Retry, CopyDetails],
            Self::PermissionDenied => &[RevealInFinder, CopyDetails],
            Self::NotFound => &[CopyDetails],
            Self::InvalidFormat => &[RevealInFinder, CopyDetails],
            Self::Conflict => &[MoveAside, CopyDetails],
            Self::ConcurrentModification | Self::Locked | Self::Network => &[Retry, CopyDetails],
            Self::RefusedOverwrite => &[Adopt, MoveAside, Cancel],
            Self::UnsupportedPlatform => &[CopyDetails],
            Self::Auth => &[Reauthenticate, CopyDetails],
            Self::SecretMissing => &[Reauthenticate, OpenSettings],
            Self::UpdateSignatureInvalid | Self::Internal => &[CopyDetails],
            Self::Cancelled => &[],
        }
    }
}

wire_enum! {
    /// An action that can resolve a failure, in the order the app shows it.
    ///
    /// The wire name is the stable id the CLI prints and the app dispatches.
    #[non_exhaustive]
    pub enum Remedy {
        /// Take an unmanaged file into the AgenTuna store.
        Adopt => "adopt",
        /// Rename the unmanaged original out of the way and continue.
        MoveAside => "move_aside",
        /// Run the operation again.
        Retry => "retry",
        /// Show the path in the file manager.
        OpenFolder => "open_folder",
        /// Select the path in the file manager.
        RevealInFinder => "reveal_in_finder",
        /// Sign in again.
        Reauthenticate => "reauthenticate",
        /// Install or point AgenTuna at an agent.
        InstallAgent => "install_agent",
        /// Choose a different path.
        SetPath => "set_path",
        /// Open the relevant settings screen.
        OpenSettings => "open_settings",
        /// Copy the technical details for a bug report.
        CopyDetails => "copy_details",
        /// Dismiss the failure without further action.
        Cancel => "cancel",
    }
}

impl Remedy {
    /// The stable id, for example `move_aside`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.wire_name()
    }

    /// Button text the app and the CLI use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Adopt => "Adopt",
            Self::MoveAside => "Move aside",
            Self::Retry => "Retry",
            Self::OpenFolder => "Open Folder",
            Self::RevealInFinder => "Reveal in Finder",
            Self::Reauthenticate => "Sign in again",
            Self::InstallAgent => "Install agent",
            Self::SetPath => "Choose path",
            Self::OpenSettings => "Open Settings",
            Self::CopyDetails => "Copy details",
            Self::Cancel => "Cancel",
        }
    }
}

/// A failure that crosses the engine, CLI and app boundary as plain data.
///
/// `message` is the technical sentence for the Details drawer. `user_message` is
/// derived from the kind and the path and never includes the source chain; the app
/// shows it in a toast. `source_chain` is diagnostic only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorReport {
    /// What kind of failure this is.
    pub kind: ErrorKind,
    /// Technical message, including the operation and its context.
    pub message: String,
    /// The path the failure is about, when there is exactly one.
    pub path: Option<PathBuf>,
    /// Journaled operation this failure belongs to, when there is one.
    pub operation_id: Option<String>,
    /// Underlying causes, outermost first, already redacted by the caller.
    pub source_chain: Vec<String>,
    /// Actions that can resolve the failure, in display order.
    pub remedy: Vec<Remedy>,
    /// Whether retrying the same operation may succeed.
    pub retryable: bool,
}

impl ErrorReport {
    /// Builds a report with the kind's default remedies and retryability.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            operation_id: None,
            source_chain: Vec::new(),
            remedy: kind.default_remedies().to_vec(),
            retryable: kind.default_retryable(),
        }
    }

    /// A cancelled operation with no message of its own.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "the operation was cancelled")
    }

    /// Maps an IO error on `path` to a report.
    ///
    /// The OS message becomes the technical `message`; the kind comes from
    /// [`io::ErrorKind`]. The path is always populated, which is what
    /// [`ErrorReport::user_message`] needs to name the file.
    #[must_use]
    pub fn from_io(path: &Path, source: io::Error) -> Self {
        Self::from_io_opt(Some(path), source)
    }

    fn from_io_opt(path: Option<&Path>, source: io::Error) -> Self {
        let kind = match source.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                ErrorKind::PermissionDenied
            }
            io::ErrorKind::NotFound => ErrorKind::NotFound,
            io::ErrorKind::AlreadyExists => ErrorKind::Conflict,
            io::ErrorKind::InvalidData
            | io::ErrorKind::InvalidInput
            | io::ErrorKind::UnexpectedEof => ErrorKind::InvalidFormat,
            io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::NetworkUnreachable => ErrorKind::Network,
            _ => ErrorKind::Io,
        };
        let retryable = matches!(
            source.kind(),
            io::ErrorKind::Interrupted
                | io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkDown
                | io::ErrorKind::NetworkUnreachable
        ) || kind.default_retryable();
        Self {
            kind,
            message: source.to_string(),
            path: path.map(Path::to_path_buf),
            operation_id: None,
            source_chain: Vec::new(),
            remedy: kind.default_remedies().to_vec(),
            retryable,
        }
    }

    /// Attaches the path the failure is about.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Attaches the journaled operation the failure belongs to.
    #[must_use]
    pub fn with_operation_id(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }

    /// Appends one cause to the source chain.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_chain.push(source.into());
        self
    }

    /// Overrides retryability.
    #[must_use]
    pub const fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Adds one action after the default remedies, without duplicating it.
    #[must_use]
    pub fn with_remedy(mut self, remedy: Remedy) -> Self {
        if !self.remedy.contains(&remedy) {
            self.remedy.push(remedy);
        }
        self
    }

    /// Replaces the remedies, for cases where the defaults would mislead.
    #[must_use]
    pub fn with_remedies(mut self, remedies: impl IntoIterator<Item = Remedy>) -> Self {
        self.remedy = remedies.into_iter().collect();
        self
    }

    /// Short, plain-language sentence for a toast, derived from the kind and path.
    ///
    /// Never includes the source chain and never prints a bare OS string.
    #[must_use]
    pub fn user_message(&self) -> String {
        let path = self
            .path
            .as_deref()
            .map(Path::display)
            .map(|p| p.to_string());
        match self.kind {
            ErrorKind::Io => match path {
                Some(path) => format!("A file operation on {path} failed."),
                None => "A file operation failed.".to_owned(),
            },
            ErrorKind::PermissionDenied => match path {
                Some(path) => format!("Cannot write {path} because the folder is read-only."),
                None => "Cannot write because the folder is read-only.".to_owned(),
            },
            ErrorKind::NotFound => match path {
                Some(path) => format!("Cannot find {path}."),
                None => "The item no longer exists.".to_owned(),
            },
            ErrorKind::InvalidFormat => match path {
                Some(path) => {
                    format!("Cannot read {path}: it is not in a format AgenTuna understands.")
                }
                None => "The file is not in a format AgenTuna understands.".to_owned(),
            },
            ErrorKind::Conflict => match path {
                Some(path) => format!("Something already exists at {path}."),
                None => "This conflicts with an existing item.".to_owned(),
            },
            ErrorKind::ConcurrentModification => match path {
                Some(path) => format!("{path} changed on disk after AgenTuna read it."),
                None => "A file changed on disk after AgenTuna read it.".to_owned(),
            },
            ErrorKind::RefusedOverwrite => match path {
                Some(path) => format!(
                    "{path} already exists and is not managed by AgenTuna, so it was not overwritten."
                ),
                None => {
                    "That path already exists and is not managed by AgenTuna, so it was not overwritten."
                        .to_owned()
                }
            },
            ErrorKind::Locked => match path {
                Some(path) => format!("{path} is locked by another operation."),
                None => "This item is locked by another operation.".to_owned(),
            },
            ErrorKind::UnsupportedPlatform => {
                "This action is not available on this platform.".to_owned()
            }
            ErrorKind::Network => "Could not reach the network service.".to_owned(),
            ErrorKind::Auth => "Authentication failed.".to_owned(),
            ErrorKind::SecretMissing => {
                "A required secret is missing from the keychain.".to_owned()
            }
            ErrorKind::UpdateSignatureInvalid => {
                "The update could not be verified, so it was not installed.".to_owned()
            }
            ErrorKind::Internal => {
                "AgenTuna hit an internal error. Copy the details for a bug report.".to_owned()
            }
            ErrorKind::Cancelled => "The operation was cancelled.".to_owned(),
        }
    }

    /// Applies a redactor to every string the user or a log can see.
    ///
    /// The path is rebuilt only when masking changed it, so a clean path keeps its
    /// exact bytes. Call this before the report is displayed, logged or serialised.
    pub fn redact_with(&mut self, redactor: &dyn SecretRedactor) {
        self.message = redactor.redact(&self.message);
        if let Some(operation_id) = &mut self.operation_id {
            *operation_id = redactor.redact(operation_id);
        }
        for entry in &mut self.source_chain {
            *entry = redactor.redact(entry);
        }
        if let Some(path) = &mut self.path {
            let original = path.to_string_lossy().into_owned();
            let masked = redactor.redact(&original);
            if masked != original {
                *path = PathBuf::from(masked);
            }
        }
    }

    /// Masks every occurrence of the given secret values in place.
    ///
    /// Values are matched longest first so a secret that contains another one is
    /// masked as a whole.
    pub fn redact_values<'a>(&mut self, secrets: impl IntoIterator<Item = &'a str>) {
        let mut values: Vec<&str> = secrets
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        if values.is_empty() {
            return;
        }
        values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        self.redact_with(&ValuesMasker(&values));
    }

    /// Builder form of [`ErrorReport::redact_values`].
    #[must_use]
    pub fn redacted_with<'a>(mut self, secrets: impl IntoIterator<Item = &'a str>) -> Self {
        self.redact_values(secrets);
        self
    }
}

/// Masks a plain value set, longest value first.
struct ValuesMasker<'a>(&'a [&'a str]);

impl SecretRedactor for ValuesMasker<'_> {
    fn redact(&self, text: &str) -> String {
        if !self.0.iter().any(|secret| text.contains(secret)) {
            return text.to_owned();
        }
        let mut masked = text.to_owned();
        for secret in self.0 {
            masked = masked.replace(secret, REDACTION_MASK);
        }
        masked
    }
}

impl fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.kind.code())?;
        if self.message.is_empty() {
            f.write_str(self.kind.title())?;
        } else {
            f.write_str(&self.message)?;
        }
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorReport {}

impl Serialize for ErrorReport {
    /// The `--json` shape: code, message, user message, path, operation, source
    /// chain, remedy ids and retryability. The remedy list serialises as stable
    /// ids so the app and scripts can dispatch them.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(8))?;
        map.serialize_entry("code", self.kind.code())?;
        map.serialize_entry("message", &self.message)?;
        map.serialize_entry("user_message", &self.user_message())?;
        map.serialize_entry("path", &self.path)?;
        map.serialize_entry("operation_id", &self.operation_id)?;
        map.serialize_entry("source_chain", &self.source_chain)?;
        map.serialize_entry("remedy", &self.remedy)?;
        map.serialize_entry("retryable", &self.retryable)?;
        map.end()
    }
}

impl From<io::Error> for ErrorReport {
    fn from(source: io::Error) -> Self {
        Self::from_io_opt(None, source)
    }
}

impl From<serde_json::Error> for ErrorReport {
    fn from(source: serde_json::Error) -> Self {
        Self::new(ErrorKind::InvalidFormat, source.to_string())
    }
}
