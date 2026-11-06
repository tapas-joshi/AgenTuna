//! Explicit, consent-bound MCP connection tests.

use std::time::Instant;

use at_model::{McpServer, McpTransport};
use tokio_util::sync::CancellationToken;

use crate::{
    HttpClient, HttpErrorKind, LaunchConsent, SessionErrorKind, StdioClient, StdioLaunch,
    ToolSummary,
};

/// Stable error category returned by a connection test.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum McpErrorKind {
    /// Executable could not be resolved from PATH.
    UnresolvedCommand,
    /// Resolved file is not executable.
    NotExecutable,
    /// Operating system denied process execution.
    PermissionDenied,
    /// Server did not start before its budget elapsed.
    StartupTimeout,
    /// MCP initialize did not complete.
    HandshakeTimeout,
    /// Tool listing did not complete.
    ToolListTimeout,
    /// No supported MCP protocol version was negotiated.
    ProtocolVersionMismatch,
    /// HTTP endpoint returned unauthorized.
    Unauthorized401,
    /// Server requires interactive sign-in.
    SignInRequired,
    /// TLS validation failed.
    TlsError,
    /// Server returned a non-401 HTTP status or an MCP error.
    ServerError {
        /// HTTP or MCP error code.
        code: i32,
        /// Redacted server-provided message.
        message: String,
    },
    /// The configured transport is unsupported by this client.
    TransportUnsupported,
    /// User approval is required before a local process can start.
    ConsentRequired,
    /// User cancelled the connection test.
    Cancelled,
    /// Any other transport or protocol failure.
    ConnectionFailed,
}

/// A connection failure safe to show in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    kind: McpErrorKind,
    message: String,
    suggested_action: String,
}

impl McpError {
    /// Returns the stable error category.
    #[must_use]
    pub fn kind(&self) -> &McpErrorKind {
        &self.kind
    }
    /// Returns the redacted user-facing message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    /// Returns the suggested next action for the UI.
    #[must_use]
    pub fn suggested_action(&self) -> &str {
        &self.suggested_action
    }
}

/// Summary of a connection attempt; failed attempts retain their typed error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    /// Whether initialization and tool listing succeeded.
    pub ok: bool,
    /// Name reported by the server, falling back to its configured name.
    pub server_name: String,
    /// Version reported by the server, if available.
    pub server_version: Option<String>,
    /// Transport label (`stdio` or `http`).
    pub transport: String,
    /// Milliseconds from connection start through the first tools response.
    pub latency_ms: u128,
    /// Number of tools returned.
    pub tools_count: usize,
    /// Sorted tool summaries.
    pub tools: Vec<ToolSummary>,
    /// Failure, when the attempt did not complete.
    pub error: Option<McpError>,
    /// Redacted server stderr excerpt, when available.
    pub stderr_excerpt: Option<String>,
}

/// Tests one server using already-configured clients.
///
/// Stdio execution requires consent bound to this exact launch. HTTP access passes
/// through the client's explicit-user-test network policy. The operation does not
/// persist a health row or modify agent configuration.
pub async fn test_connection(
    server: &McpServer,
    stdio: &StdioClient,
    http: &HttpClient,
    consent: Option<&LaunchConsent>,
    cancel: &CancellationToken,
) -> TestResult {
    let started = Instant::now();
    let transport = server.transport.kind().to_owned();
    let fallback_name = server.display_name.clone();
    let fail = |error| TestResult {
        ok: false,
        server_name: fallback_name.clone(),
        server_version: None,
        transport: transport.clone(),
        latency_ms: started.elapsed().as_millis(),
        tools_count: 0,
        tools: Vec::new(),
        error: Some(error),
        stderr_excerpt: None,
    };
    match &server.transport {
        McpTransport::Stdio { .. } => {
            let launch = match StdioLaunch::from_server(server) {
                Ok(launch) => launch,
                Err(error) => return fail(session_error(error.kind(), error.ui_message())),
            };
            let Some(consent) = consent else {
                return fail(McpError {
                    kind: McpErrorKind::ConsentRequired,
                    message: "Starting this server requires explicit approval.".into(),
                    suggested_action: "Approve Test connection to continue.".into(),
                });
            };
            let mut session = match stdio.open(&launch, consent, cancel).await {
                Ok(session) => session,
                Err(error) => return fail(session_error(error.kind(), error.ui_message())),
            };
            let identity = session.server_identity();
            let listing = session.list_tools(cancel).await;
            let latency_ms = started.elapsed().as_millis().max(1);
            let stderr_excerpt = session.stderr_excerpt().map(truncate_excerpt);
            let _shutdown = session.shutdown(cancel).await;
            match listing {
                Ok(listing) => TestResult {
                    ok: true,
                    server_name: identity
                        .as_ref()
                        .and_then(|i| i.name.clone())
                        .unwrap_or(fallback_name),
                    server_version: identity.and_then(|i| i.version),
                    transport,
                    latency_ms,
                    tools_count: listing.tools.len(),
                    tools: listing.tools,
                    error: None,
                    stderr_excerpt,
                },
                Err(error) => {
                    let mut result = fail(session_error(error.kind(), error.ui_message()));
                    result.stderr_excerpt = stderr_excerpt;
                    result
                }
            }
        }
        McpTransport::Http { .. } => {
            let session = match http.open(server, cancel).await {
                Ok(session) => session,
                Err(error) => return fail(http_error(error.kind(), error.ui_message(), true)),
            };
            let identity = session.server_identity();
            let listing = session.list_tools(cancel).await;
            let latency_ms = started.elapsed().as_millis().max(1);
            session.close().await;
            match listing {
                Ok(listing) => TestResult {
                    ok: true,
                    server_name: identity
                        .as_ref()
                        .and_then(|i| i.name.clone())
                        .unwrap_or(fallback_name),
                    server_version: identity.and_then(|i| i.version),
                    transport,
                    latency_ms,
                    tools_count: listing.tools.len(),
                    tools: listing.tools,
                    error: None,
                    stderr_excerpt: None,
                },
                Err(error) => fail(http_error(error.kind(), error.ui_message(), false)),
            }
        }
    }
}

fn session_error(kind: SessionErrorKind, message: &str) -> McpError {
    let (kind, suggested_action) = match kind {
        SessionErrorKind::UnresolvedCommand => (
            McpErrorKind::UnresolvedCommand,
            "Install the command or fix its path.",
        ),
        SessionErrorKind::NotExecutable => {
            (McpErrorKind::NotExecutable, "Make the server executable.")
        }
        SessionErrorKind::PermissionDenied => (
            McpErrorKind::PermissionDenied,
            "Allow execution in system settings.",
        ),
        SessionErrorKind::StartupTimeout => (
            McpErrorKind::StartupTimeout,
            "Check the server startup and try again.",
        ),
        SessionErrorKind::ToolListTimeout => (
            McpErrorKind::ToolListTimeout,
            "Check whether the server responds to tools/list.",
        ),
        SessionErrorKind::ProtocolVersionMismatch => (
            McpErrorKind::ProtocolVersionMismatch,
            "Update the server to a supported MCP protocol version.",
        ),
        SessionErrorKind::Cancelled => (McpErrorKind::Cancelled, "Retry the connection test."),
        SessionErrorKind::NotStdio | SessionErrorKind::PlatformUnsupported => (
            McpErrorKind::TransportUnsupported,
            "Use a supported transport.",
        ),
        _ => (
            McpErrorKind::ConnectionFailed,
            "Review the server configuration and try again.",
        ),
    };
    McpError {
        kind,
        message: message.to_owned(),
        suggested_action: suggested_action.to_owned(),
    }
}

fn http_error(kind: HttpErrorKind, message: &str, initializing: bool) -> McpError {
    let (kind, suggested_action) = match kind {
        HttpErrorKind::SignInRequired => (McpErrorKind::SignInRequired, "Sign in to this server."),
        HttpErrorKind::TlsError => (McpErrorKind::TlsError, "Review the server certificate."),
        HttpErrorKind::ProtocolVersionMismatch => (
            McpErrorKind::ProtocolVersionMismatch,
            "Update the server to a supported MCP protocol version.",
        ),
        HttpErrorKind::Timeout if initializing => (
            McpErrorKind::HandshakeTimeout,
            "Check whether the endpoint completes MCP initialization.",
        ),
        HttpErrorKind::Timeout => (
            McpErrorKind::ToolListTimeout,
            "Check the endpoint and try again.",
        ),
        HttpErrorKind::Cancelled => (McpErrorKind::Cancelled, "Retry the connection test."),
        HttpErrorKind::HttpStatus => {
            let code = message
                .strip_prefix("The server returned HTTP ")
                .and_then(|rest| rest.trim_end_matches('.').parse::<i32>().ok());
            match code {
                Some(401) => (
                    McpErrorKind::Unauthorized401,
                    "Check the server's authentication settings.",
                ),
                Some(code) => (
                    McpErrorKind::ServerError {
                        code,
                        message: message.to_owned(),
                    },
                    "Review the server response and configuration.",
                ),
                None => (
                    McpErrorKind::ConnectionFailed,
                    "Review the server response.",
                ),
            }
        }
        HttpErrorKind::Denied
        | HttpErrorKind::UnsupportedEndpoint
        | HttpErrorKind::ProtocolError
        | HttpErrorKind::Transport => (
            McpErrorKind::ConnectionFailed,
            "Review the endpoint and network policy.",
        ),
    };
    McpError {
        kind,
        message: message.to_owned(),
        suggested_action: suggested_action.to_owned(),
    }
}

fn truncate_excerpt(text: String) -> String {
    const MAX_BYTES: usize = 2 * 1024;
    if text.len() <= MAX_BYTES {
        return text;
    }
    let mut end = MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{McpErrorKind, http_error, session_error, truncate_excerpt};
    use crate::{HttpErrorKind, SessionErrorKind};

    #[test]
    fn stderr_excerpt_is_bounded_by_bytes_without_splitting_utf8() {
        let excerpt = truncate_excerpt("é".repeat(2_000));
        assert_eq!(excerpt.len(), 2 * 1024);
        assert!(excerpt.is_char_boundary(excerpt.len()));
        assert_eq!(truncate_excerpt("short".to_owned()), "short");
    }

    #[test]
    fn injected_failures_cover_every_connection_error_kind() {
        let consent_required = super::McpError {
            kind: McpErrorKind::ConsentRequired,
            message: "approval required".to_owned(),
            suggested_action: "approve".to_owned(),
        };
        let cases = [
            (
                session_error(SessionErrorKind::UnresolvedCommand, "missing"),
                McpErrorKind::UnresolvedCommand,
            ),
            (
                session_error(SessionErrorKind::NotExecutable, "mode"),
                McpErrorKind::NotExecutable,
            ),
            (
                session_error(SessionErrorKind::PermissionDenied, "denied"),
                McpErrorKind::PermissionDenied,
            ),
            (
                session_error(SessionErrorKind::StartupTimeout, "startup"),
                McpErrorKind::StartupTimeout,
            ),
            (
                session_error(SessionErrorKind::ToolListTimeout, "tools"),
                McpErrorKind::ToolListTimeout,
            ),
            (
                session_error(SessionErrorKind::ProtocolVersionMismatch, "version"),
                McpErrorKind::ProtocolVersionMismatch,
            ),
            (
                http_error(
                    HttpErrorKind::ProtocolVersionMismatch,
                    "unsupported protocol version",
                    true,
                ),
                McpErrorKind::ProtocolVersionMismatch,
            ),
            (
                session_error(SessionErrorKind::NotStdio, "transport"),
                McpErrorKind::TransportUnsupported,
            ),
            (
                session_error(SessionErrorKind::Cancelled, "cancelled"),
                McpErrorKind::Cancelled,
            ),
            (consent_required, McpErrorKind::ConsentRequired),
            (
                session_error(SessionErrorKind::SpawnFailed, "spawn"),
                McpErrorKind::ConnectionFailed,
            ),
            (
                http_error(HttpErrorKind::Timeout, "timeout", true),
                McpErrorKind::HandshakeTimeout,
            ),
            (
                http_error(HttpErrorKind::Timeout, "timeout", false),
                McpErrorKind::ToolListTimeout,
            ),
            (
                http_error(HttpErrorKind::SignInRequired, "auth", false),
                McpErrorKind::SignInRequired,
            ),
            (
                http_error(HttpErrorKind::TlsError, "tls", false),
                McpErrorKind::TlsError,
            ),
            (
                http_error(
                    HttpErrorKind::HttpStatus,
                    "The server returned HTTP 401.",
                    false,
                ),
                McpErrorKind::Unauthorized401,
            ),
            (
                http_error(
                    HttpErrorKind::HttpStatus,
                    "The server returned HTTP 503.",
                    false,
                ),
                McpErrorKind::ServerError {
                    code: 503,
                    message: "The server returned HTTP 503.".to_owned(),
                },
            ),
        ];
        let covered: HashSet<_> = cases.iter().map(|(_, kind)| kind).collect();
        assert_eq!(
            covered,
            HashSet::from([
                &McpErrorKind::UnresolvedCommand,
                &McpErrorKind::NotExecutable,
                &McpErrorKind::PermissionDenied,
                &McpErrorKind::StartupTimeout,
                &McpErrorKind::HandshakeTimeout,
                &McpErrorKind::ToolListTimeout,
                &McpErrorKind::ProtocolVersionMismatch,
                &McpErrorKind::Unauthorized401,
                &McpErrorKind::SignInRequired,
                &McpErrorKind::TlsError,
                &McpErrorKind::ServerError {
                    code: 503,
                    message: "The server returned HTTP 503.".to_owned(),
                },
                &McpErrorKind::TransportUnsupported,
                &McpErrorKind::ConsentRequired,
                &McpErrorKind::Cancelled,
                &McpErrorKind::ConnectionFailed,
            ])
        );
    }
}
