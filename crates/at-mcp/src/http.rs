//! Streamable-HTTP MCP client sessions.
//!
//! [`HttpClient`] opens a remote MCP server over rmcp's
//! [`StreamableHttpClientTransport`], gated by the same [`authorize_connect`]
//! policy as every other network call in this crate. When the server carries an
//! [`OAuthProfile`], the HTTP client inside the transport is wrapped in rmcp's
//! [`AuthClient`] so a 401 triggers one silent refresh and one retry before the
//! error surfaces as [`HttpErrorKind::SignInRequired`].
//!
//! TLS failures map to [`HttpErrorKind::TlsError`] and are never retried.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use at_model::{EnvValue, McpServer, McpServerId, McpTransport, NetworkPolicy, OAuthProfile};
use at_secrets::SecretStore;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, PaginatedRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService, serve_client_with_ct};
use rmcp::transport::auth::{AuthClient, AuthError, AuthorizationManager};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::oauth::KeychainCredentialStore;
use crate::policy::{ConnectIntent, McpAccess, authorize_connect};
use crate::session::{ToolListing, ToolSummary};

/// Budget for the HTTP `initialize` handshake.
pub const HTTP_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for one `tools/list` page over HTTP.
pub const HTTP_TOOL_PAGE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum tools collected across all pages.
pub const HTTP_TOOL_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Stable classification of an HTTP session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HttpErrorKind {
    /// The network policy refused the connection.
    Denied,
    /// The endpoint URL has no usable host or port.
    UnsupportedEndpoint,
    /// The server requires sign-in; no usable token exists.
    SignInRequired,
    /// TLS validation failed.
    TlsError,
    /// The handshake or a request timed out.
    Timeout,
    /// The peer answered with something that is not a valid MCP message.
    ProtocolError,
    /// The server returned an HTTP error status.
    HttpStatus,
    /// The caller's cancellation token fired.
    Cancelled,
    /// Any other transport failure.
    Transport,
}

/// A failed HTTP session with a UI-ready message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    kind: HttpErrorKind,
    message: String,
}

impl HttpError {
    /// Builds an error from a kind and message.
    #[must_use]
    pub fn new(kind: HttpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The classification.
    #[must_use]
    pub fn kind(&self) -> HttpErrorKind {
        self.kind
    }

    /// The message a screen may render.
    #[must_use]
    pub fn ui_message(&self) -> &str {
        &self.message
    }

    fn denied(error: impl std::fmt::Display) -> Self {
        Self::new(
            HttpErrorKind::Denied,
            format!("The network policy blocked the connection: {error}"),
        )
    }

    fn sign_in_required() -> Self {
        Self::new(
            HttpErrorKind::SignInRequired,
            "This server needs you to sign in before AgenTuna can connect.",
        )
    }

    fn tls() -> Self {
        Self::new(
            HttpErrorKind::TlsError,
            "The connection failed TLS validation. Check the server's certificate and try again.",
        )
    }

    fn timeout() -> Self {
        Self::new(
            HttpErrorKind::Timeout,
            "The server did not respond in time.",
        )
    }

    fn cancelled() -> Self {
        Self::new(
            HttpErrorKind::Cancelled,
            "The connection check was cancelled.",
        )
    }

    fn protocol(detail: impl Into<String>) -> Self {
        Self::new(
            HttpErrorKind::ProtocolError,
            format!(
                "The server replied with something that is not a valid MCP message. {}",
                detail.into()
            ),
        )
    }

    fn transport(detail: impl Into<String>) -> Self {
        Self::new(HttpErrorKind::Transport, detail.into())
    }

    /// Builds an HTTP status error.
    #[must_use]
    pub fn http_status(status: u16) -> Self {
        Self::new(
            HttpErrorKind::HttpStatus,
            format!("The server returned HTTP {status}."),
        )
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// The long-lived half of the HTTP client.
///
/// Holds the network policy, credential store and reported identity; one
/// instance serves every remote server check.
#[derive(Clone)]
pub struct HttpClient {
    policy: Arc<NetworkPolicy>,
    secrets: Arc<dyn SecretStore>,
    log: Arc<tokio::sync::Mutex<at_model::NetworkEventLog>>,
    client: Implementation,
}

impl HttpClient {
    /// A client that reports itself as `AgenTuna/<crate version>`.
    #[must_use]
    pub fn new(policy: Arc<NetworkPolicy>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            policy,
            secrets,
            log: Arc::new(tokio::sync::Mutex::new(at_model::NetworkEventLog::new())),
            client: Implementation::new("AgenTuna", env!("CARGO_PKG_VERSION")),
        }
    }

    /// Replaces the network event log.
    #[must_use]
    pub fn with_log(mut self, log: Arc<tokio::sync::Mutex<at_model::NetworkEventLog>>) -> Self {
        self.log = log;
        self
    }

    /// Replaces the reported client identity (`clientInfo`).
    #[must_use]
    pub fn with_client_identity(
        mut self,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        self.client = Implementation::new(name.into(), version.into());
        self
    }

    /// Opens an HTTP session to `server`.
    ///
    /// The call is gated by [`authorize_connect`] with [`ConnectIntent::UserTest`]
    /// and [`at_model::NetworkAction::McpRemoteConnect`]; when the server carries
    /// an OAuth profile the transport's HTTP client is wrapped in rmcp's
    /// `AuthClient` so a 401 triggers one refresh and one retry before surfacing
    /// [`HttpErrorKind::SignInRequired`].
    ///
    /// # Errors
    /// [`HttpError`] on policy denial, TLS failure, timeout, protocol error or
    /// auth failure.
    pub async fn open(
        &self,
        server: &McpServer,
        cancel: &CancellationToken,
    ) -> Result<HttpSession, HttpError> {
        if cancel.is_cancelled() {
            return Err(HttpError::cancelled());
        }

        let McpTransport::Http {
            url,
            headers,
            oauth,
        } = &server.transport
        else {
            return Err(HttpError::new(
                HttpErrorKind::Transport,
                "This server uses stdio; there is no HTTP endpoint to open.",
            ));
        };

        // Policy gate: no socket opens before this passes.
        let authorized = authorize_connect(
            self.policy.as_ref(),
            server,
            ConnectIntent::UserTest,
            at_model::NetworkAction::McpRemoteConnect,
        )
        .map_err(HttpError::denied)?;
        let McpAccess::Http(authorized) = authorized else {
            return Err(HttpError::new(
                HttpErrorKind::Transport,
                "The server is not remote.",
            ));
        };

        debug!(
            host = %authorized.host(),
            port = authorized.port(),
            "opening HTTP MCP session"
        );

        // Build custom headers from literal EnvValues; secret refs are resolved
        // by the caller before this point (same rule as stdio env).
        let mut custom_headers: HashMap<http::HeaderName, http::HeaderValue> = HashMap::new();
        for (name, value) in headers {
            if let EnvValue::Literal(text) = value {
                let header_name = http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| HttpError::protocol(format!("invalid header name `{name}`")))?;
                let header_value = http::HeaderValue::from_str(text).map_err(|_| {
                    HttpError::protocol(format!("invalid header value for `{name}`"))
                })?;
                custom_headers.insert(header_name, header_value);
            }
        }

        let mut config = StreamableHttpClientTransportConfig::with_uri(url.as_str().to_owned());
        if !custom_headers.is_empty() {
            config = config.custom_headers(custom_headers);
        }

        let client_config = ClientConfig::new(ClientCapabilities::default(), self.client.clone());

        let running = if let Some(profile) = oauth {
            let manager = self.build_auth_manager(server, profile).await?;
            let auth_client = AuthClient::new(reqwest::Client::new(), manager);
            let transport = StreamableHttpClientTransport::with_client(auth_client, config);
            let handshake = serve_client_with_ct(client_config, transport, cancel.clone());
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(HttpError::cancelled()),
                result = tokio::time::timeout(HTTP_INITIALIZE_TIMEOUT, handshake) => match result {
                    Ok(Ok(running)) => running,
                    Ok(Err(error)) => return Err(map_initialize_error(&error)),
                    Err(_elapsed) => return Err(HttpError::timeout()),
                },
            }
        } else {
            let transport =
                StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
            let handshake = serve_client_with_ct(client_config, transport, cancel.clone());
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(HttpError::cancelled()),
                result = tokio::time::timeout(HTTP_INITIALIZE_TIMEOUT, handshake) => match result {
                    Ok(Ok(running)) => running,
                    Ok(Err(error)) => return Err(map_initialize_error(&error)),
                    Err(_elapsed) => return Err(HttpError::timeout()),
                },
            }
        };

        Ok(HttpSession {
            running,
            server_id: server.id,
        })
    }

    /// Builds an [`AuthorizationManager`] for the server's OAuth profile.
    async fn build_auth_manager(
        &self,
        server: &McpServer,
        profile: &OAuthProfile,
    ) -> Result<AuthorizationManager, HttpError> {
        let McpTransport::Http { url, .. } = &server.transport else {
            return Err(HttpError::new(
                HttpErrorKind::Transport,
                "The server is not remote.",
            ));
        };
        let mut manager = AuthorizationManager::new_with_oauth_http_client(
            url.as_str(),
            Arc::new(crate::oauth::PolicyOAuthHttpClient::new(
                self.policy.clone(),
                self.log.clone(),
            )),
        )
        .await
        .map_err(|error| map_auth_error(&error))?;
        manager.set_credential_store(KeychainCredentialStore::new(
            self.secrets.clone(),
            server.id,
            profile.issuer.clone(),
        ));
        let _ = manager.initialize_from_store().await;
        Ok(manager)
    }
}

impl fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClient")
            .field("client", &self.client.name)
            .field("version", &self.client.version)
            .finish_non_exhaustive()
    }
}

/// A ready HTTP session.
pub struct HttpSession {
    running: RunningService<RoleClient, ClientConfig>,
    server_id: McpServerId,
}

impl HttpSession {
    /// The server this session belongs to.
    #[must_use]
    pub fn server_id(&self) -> McpServerId {
        self.server_id
    }

    /// Lists tools with pagination, bounded by [`HTTP_TOOL_LIMIT`].
    ///
    /// # Errors
    /// [`HttpError`] on transport failure, timeout or auth failure.
    pub async fn list_tools(&self, cancel: &CancellationToken) -> Result<ToolListing, HttpError> {
        let peer = self.running.peer().clone();
        let mut tools: Vec<Tool> = Vec::new();
        let mut cursor = None;
        let mut truncated = false;

        loop {
            if cancel.is_cancelled() {
                return Err(HttpError::cancelled());
            }
            if tools.len() >= HTTP_TOOL_LIMIT {
                truncated = true;
                break;
            }

            let params = PaginatedRequestParams::default().with_cursor(cursor.clone());

            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(HttpError::cancelled()),
                result = tokio::time::timeout(
                    HTTP_TOOL_PAGE_TIMEOUT,
                    peer.list_tools(Some(params)),
                ) => match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => return Err(map_service_error(&error)),
                    Err(_elapsed) => return Err(HttpError::timeout()),
                },
            };

            tools.extend(response.tools.iter().cloned());
            cursor = response.next_cursor.clone();
            if cursor.is_none() {
                break;
            }
        }

        let mut summaries: Vec<ToolSummary> = tools
            .iter()
            .map(|tool| ToolSummary {
                name: tool.name.to_string(),
                title: tool.title.clone(),
                description: tool.description.as_ref().map(ToString::to_string),
            })
            .collect();
        summaries.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(ToolListing {
            tools: summaries,
            truncated,
        })
    }

    /// Closes the session.
    pub async fn close(self) {
        let _ = self.running.cancel().await;
    }
}

impl fmt::Debug for HttpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpSession")
            .field("server_id", &self.server_id)
            .finish_non_exhaustive()
    }
}

/// Maps an rmcp handshake failure onto the HTTP taxonomy.
fn map_initialize_error(error: &rmcp::service::ClientInitializeError) -> HttpError {
    use rmcp::service::ClientInitializeError;
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("auth required")
        || lower.contains("authrequired")
        || lower.contains("authorization required")
        || lower.contains("authorizationrequired")
        || lower.contains("refresh token was rejected")
        || lower.contains("token refresh was rejected")
        || lower.contains("tokenrefreshrejected")
        || lower.contains("invalid_grant")
    {
        return HttpError::sign_in_required();
    }
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("rustls") {
        return HttpError::tls();
    }
    match error {
        ClientInitializeError::Cancelled => HttpError::cancelled(),
        ClientInitializeError::ConflictInitResponseId(expected, got) => HttpError::protocol(
            format!("conflict initialized response id: expected {expected}, got {got}"),
        ),
        ClientInitializeError::ConnectionClosed(_) => {
            HttpError::protocol("The server closed the connection during initialize.")
        }
        _ => HttpError::protocol(text),
    }
}

/// Maps an rmcp service error onto the HTTP taxonomy.
fn map_service_error(error: &rmcp::ServiceError) -> HttpError {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("auth required")
        || lower.contains("authrequired")
        || lower.contains("authorization required")
        || lower.contains("authorizationrequired")
        || lower.contains("refresh token was rejected")
        || lower.contains("token refresh was rejected")
        || lower.contains("tokenrefreshrejected")
        || lower.contains("invalid_grant")
    {
        return HttpError::sign_in_required();
    }
    if lower.contains("tls") || lower.contains("certificate") || lower.contains("rustls") {
        return HttpError::tls();
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return HttpError::timeout();
    }
    HttpError::transport(text)
}

/// Maps an rmcp auth error onto the HTTP taxonomy.
fn map_auth_error(error: &AuthError) -> HttpError {
    match error {
        AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_) => {
            HttpError::sign_in_required()
        }
        AuthError::HttpError(source) => {
            if crate::oauth::is_tls_error(source) {
                HttpError::tls()
            } else if source.is_timeout() {
                HttpError::timeout()
            } else {
                HttpError::transport(source.to_string())
            }
        }
        _ => {
            let lower = error.to_string().to_ascii_lowercase();
            if lower.contains("tls") || lower.contains("certificate") || lower.contains("rustls") {
                HttpError::tls()
            } else {
                HttpError::transport(error.to_string())
            }
        }
    }
}
