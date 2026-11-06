//! OAuth 2.0 sign-in for remote MCP servers (RFC 8414, RFC 9728, RFC 7591, PKCE S256).
//!
//! # Where secrets live
//!
//! Access and refresh tokens are stored through [`SecretStore`] under the account
//! `oauth:<server-id>:<issuer>` — a reference, never a value in `mcp.json` or a log.
//! The rmcp [`AuthorizationManager`] is given a [`KeychainCredentialStore`] so every
//! save it performs lands in the OS credential store; the in-memory store is never
//! used in production paths.
//!
//! # The loopback listener
//!
//! Sign-in opens the system browser to the authorization URL and waits on
//! `http://127.0.0.1:<ephemeral>/callback` for up to [`SIGN_IN_TIMEOUT`]. A
//! [`CancellationToken`] or dropping the listener closes the socket; no partial
//! token rows remain because nothing is written until the exchange succeeds.
//!
//! # Explicit action only
//!
//! Every HTTP request the OAuth machinery makes is gated by the same
//! [`NetworkPolicy`] decision as the MCP connection itself: [`OAuthClient::sign_in`]
//! calls [`authorize_connect`] with [`ConnectIntent::UserTest`] before any socket
//! is opened. There is no code path that fetches metadata or contacts an
//! authorization server without the caller having asked for sign-in.

use oauth2::TokenResponse;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use at_model::{
    McpServer, McpServerId, McpTransport, NetworkAction, NetworkEvent, NetworkEventLog,
    NetworkHost, NetworkPolicy, NetworkTrigger,
};
use at_secrets::{KeychainId, SecretStore, SecretString, SecretsError};
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession, CredentialStore,
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest, StoredCredentials,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::policy::{ConnectIntent, McpAccess, authorize_connect};

/// How long the loopback listener waits for the browser callback.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(120);

/// How close to expiry a token is refreshed before use (60 s skew).
pub const REFRESH_SKEW: Duration = Duration::from_secs(60);

/// Keychain service used for OAuth tokens.
pub const OAUTH_KEYCHAIN_SERVICE: &str = "dev.agentuna.mcp.oauth";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a sign-in or authenticated call failed.
///
/// The variants match the test-connection taxonomy where one exists
/// ([`SignInErrorKind::SignInRequired`], [`SignInErrorKind::TlsError`]) so the
/// UI can render the same suggested action for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum SignInErrorKind {
    /// The state parameter did not match what the flow issued.
    #[error("OAuthStateMismatch")]
    OAuthStateMismatch,
    /// The authorization code could not be exchanged for a token.
    #[error("OAuthTokenExchange")]
    OAuthTokenExchange,
    /// The token store refused the write or read.
    #[error("Keychain")]
    Keychain,
    /// The server or authorization endpoint failed TLS validation.
    #[error("TlsError")]
    TlsError,
    /// The server requires sign-in; no usable token exists.
    #[error("SignInRequired")]
    SignInRequired,
    /// The user cancelled the sign-in or the caller's token fired.
    #[error("Cancelled")]
    Cancelled,
    /// The network policy refused a request.
    #[error("Denied")]
    Denied,
    /// The browser could not be opened and the URL was copied to the clipboard.
    #[error("BrowserUnavailable")]
    BrowserUnavailable,
    /// Any other OAuth protocol failure.
    #[error("OAuthError")]
    OAuthError,
}

/// A failed sign-in or authenticated request with a UI-ready message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInError {
    kind: SignInErrorKind,
    message: String,
}

impl SignInError {
    /// Builds an error from a kind and message.
    #[must_use]
    pub fn new(kind: SignInErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The classification.
    #[must_use]
    pub fn kind(&self) -> SignInErrorKind {
        self.kind
    }

    /// The message a screen may render.
    #[must_use]
    pub fn ui_message(&self) -> &str {
        &self.message
    }

    fn state_mismatch() -> Self {
        Self::new(
            SignInErrorKind::OAuthStateMismatch,
            "The sign-in callback did not match the request AgenTuna started. Try signing in again.",
        )
    }

    fn cancelled() -> Self {
        Self::new(SignInErrorKind::Cancelled, "The sign-in was cancelled.")
    }

    fn sign_in_required() -> Self {
        Self::new(
            SignInErrorKind::SignInRequired,
            "This server needs you to sign in before AgenTuna can connect.",
        )
    }

    fn tls() -> Self {
        Self::new(
            SignInErrorKind::TlsError,
            "The connection failed TLS validation. Check the server's certificate and try again.",
        )
    }

    fn from_auth(error: &AuthError) -> Self {
        match error {
            AuthError::AuthorizationRequired | AuthError::TokenRefreshRejected(_) => {
                Self::sign_in_required()
            }
            AuthError::HttpError(source) => {
                if is_tls_error(source) {
                    Self::tls()
                } else {
                    Self::new(
                        SignInErrorKind::OAuthError,
                        format!("The OAuth request failed: {source}"),
                    )
                }
            }
            _ => Self::new(
                SignInErrorKind::OAuthError,
                format!("The OAuth request failed: {error}"),
            ),
        }
    }
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SignInError {}

/// Whether a reqwest error is a TLS failure.
pub(crate) fn is_tls_error(error: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        let text = current.to_string().to_ascii_lowercase();
        if text.contains("tls")
            || text.contains("certificate")
            || text.contains("cert")
            || text.contains("rustls")
            || text.contains("handshake")
            || text.contains("invalid dns name")
        {
            return true;
        }
        source = current.source();
    }
    false
}

// ---------------------------------------------------------------------------
// Loopback listener
// ---------------------------------------------------------------------------

/// The ephemeral loopback listener that receives the OAuth redirect.
///
/// Binds `127.0.0.1:0`, serves exactly one request at `/callback`, and resolves
/// with the raw callback URL. Dropping it closes the socket and fails any wait.
/// The incoming redirect request on the loopback listener.
pub struct CallbackRequest {
    url: String,
    responder: Option<tokio::sync::oneshot::Sender<CallbackResponse>>,
}

impl CallbackRequest {
    /// The complete callback URL including query parameters.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Responds to the browser with either a success or error page.
    pub fn respond(mut self, success: bool, error_message: Option<&str>) {
        if let Some(responder) = self.responder.take() {
            let res = if success {
                CallbackResponse::Success
            } else {
                CallbackResponse::Error(
                    error_message
                        .unwrap_or("Sign-in verification failed.")
                        .to_string(),
                )
            };
            let _ = responder.send(res);
        }
    }
}

impl Drop for CallbackRequest {
    fn drop(&mut self) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.send(CallbackResponse::Error("Sign-in cancelled.".to_string()));
        }
    }
}

enum CallbackResponse {
    Success,
    Error(String),
}

/// The ephemeral loopback listener that receives the OAuth redirect.
///
/// Binds `127.0.0.1:0`, serves exactly one request at `/callback`, and resolves
/// with the callback request. Dropping it closes the socket and fails any wait.
pub struct LoopbackListener {
    addr: SocketAddr,
    task: Option<tokio::task::JoinHandle<Result<CallbackRequest, SignInError>>>,
}

impl LoopbackListener {
    /// Binds an ephemeral port on the loopback interface.
    ///
    /// # Errors
    /// [`SignInError`] when the socket cannot be bound.
    pub async fn bind() -> Result<Self, SignInError> {
        let socket = tokio::net::TcpSocket::new_v4().map_err(|error| {
            SignInError::new(
                SignInErrorKind::OAuthError,
                format!("Could not create sign-in socket: {error}"),
            )
        })?;
        let _ = socket.set_reuseaddr(true);
        #[cfg(unix)]
        let _ = socket.set_reuseport(true);
        socket
            .bind("127.0.0.1:0".parse().unwrap())
            .map_err(|error| {
                SignInError::new(
                    SignInErrorKind::OAuthError,
                    format!("Could not bind sign-in socket: {error}"),
                )
            })?;
        let listener = socket.listen(1).map_err(|error| {
            SignInError::new(
                SignInErrorKind::OAuthError,
                format!("Could not listen on sign-in socket: {error}"),
            )
        })?;
        let addr = listener.local_addr().map_err(|error| {
            SignInError::new(
                SignInErrorKind::OAuthError,
                format!("Could not read the sign-in listener address: {error}"),
            )
        })?;
        let task = tokio::spawn(serve_once(listener));
        Ok(Self {
            addr,
            task: Some(task),
        })
    }

    /// The port the listener is bound to.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The `http://127.0.0.1:<port>/callback` URL to register as the redirect URI.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.addr.port())
    }

    /// Waits for the callback, bounded by `timeout`.
    ///
    /// # Errors
    /// `OAuthStateMismatch` when the callback is malformed, `OAuthError` on
    /// timeout or listener failure.
    pub async fn wait(mut self, timeout: Duration) -> Result<CallbackRequest, SignInError> {
        let Some(task) = self.task.as_mut() else {
            return Err(SignInError::new(
                SignInErrorKind::OAuthError,
                "The sign-in listener was already consumed.",
            ));
        };
        tokio::time::timeout(timeout, task)
            .await
            .map_err(|_| {
                SignInError::new(
                    SignInErrorKind::OAuthError,
                    "The sign-in timed out waiting for the browser.",
                )
            })?
            .map_err(|_| {
                SignInError::new(
                    SignInErrorKind::OAuthError,
                    "The sign-in listener stopped unexpectedly.",
                )
            })?
    }
}

impl Drop for LoopbackListener {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Reads one HTTP request, extracts the callback URL, writes a minimal response.
async fn serve_once(listener: TcpListener) -> Result<CallbackRequest, SignInError> {
    let (mut stream, _) = listener.accept().await.map_err(|error| {
        SignInError::new(
            SignInErrorKind::OAuthError,
            format!("The sign-in listener failed: {error}"),
        )
    })?;

    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).await.map_err(|error| {
        SignInError::new(
            SignInErrorKind::OAuthError,
            format!("Could not read the sign-in callback: {error}"),
        )
    })?;
    let request_text = String::from_utf8_lossy(&buf[..n]);
    let path = request_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if !path.starts_with("/callback") {
        let not_found = "HTTP/1.1 404 Not Found\r\ncontent-type: text/plain\r\ncontent-length: 9\r\nconnection: close\r\n\r\nNot found";
        let _ = stream.write_all(not_found.as_bytes()).await;
        let _ = stream.shutdown().await;
        return Err(SignInError::new(
            SignInErrorKind::OAuthError,
            "The browser opened the wrong sign-in page.",
        ));
    }

    let local_addr = stream.local_addr().map_err(|error| {
        SignInError::new(
            SignInErrorKind::OAuthError,
            format!("Could not get local address: {error}"),
        )
    })?;

    let full_url = format!("http://127.0.0.1:{}{path}", local_addr.port());
    let (tx, rx) = tokio::sync::oneshot::channel::<CallbackResponse>();

    tokio::spawn(async move {
        let (status, body) = match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(CallbackResponse::Success)) => (
                "200 OK",
                "<!doctype html><title>Signed in</title><p>You can close this tab and return to AgenTuna.</p>".to_string(),
            ),
            Ok(Ok(CallbackResponse::Error(msg))) => (
                "400 Bad Request",
                format!("<!doctype html><title>Sign-in failed</title><p>Authentication failed: {msg}</p>"),
            ),
            _ => (
                "400 Bad Request",
                "<!doctype html><title>Sign-in failed</title><p>Authentication was cancelled.</p>".to_string(),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });

    Ok(CallbackRequest {
        url: full_url,
        responder: Some(tx),
    })
}

// ---------------------------------------------------------------------------
// Keychain credential store
// ---------------------------------------------------------------------------

/// rmcp [`CredentialStore`] that persists OAuth tokens in [`SecretStore`].
///
/// The account is `oauth:<server-id>:<issuer>`; the stored value is the JSON
/// serialization of rmcp's [`StoredCredentials`]. Nothing here writes to a file.
pub struct KeychainCredentialStore {
    store: Arc<dyn SecretStore>,
    server_id: McpServerId,
    issuer: String,
}

impl KeychainCredentialStore {
    /// A store bound to one server and issuer.
    #[must_use]
    pub fn new(
        store: Arc<dyn SecretStore>,
        server_id: McpServerId,
        issuer: impl Into<String>,
    ) -> Self {
        Self {
            store,
            server_id,
            issuer: issuer.into(),
        }
    }

    fn keychain_id(&self) -> Result<KeychainId, AuthError> {
        let account = format!("oauth:{}:{}", self.server_id, self.issuer);
        KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, account).map_err(|error| {
            AuthError::CredentialStoreError(format!("invalid OAuth account name: {error}"))
        })
    }
}

#[async_trait::async_trait]
impl CredentialStore for KeychainCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let id = self.keychain_id()?;
        match self.store.read(&id) {
            Ok(secret) => {
                let text = secret.expose_secret().to_owned();
                let mut creds: StoredCredentials = serde_json::from_str(&text)
                    .map_err(|error| AuthError::CredentialStoreError(error.to_string()))?;
                // rmcp has REFRESH_BUFFER_SECS = 30. AgenTuna's contract
                // specifies REFRESH_SKEW = 60s. We advance the apparent age
                // by (60 - 30) = 30s so rmcp triggers proactive refresh at 60s.
                if let Some(ts) = creds.token_received_at {
                    let extra_skew = REFRESH_SKEW.as_secs().saturating_sub(30);
                    creds.token_received_at = Some(ts.saturating_sub(extra_skew));
                }
                Ok(Some(creds))
            }
            Err(SecretsError::NotFound { .. }) => Ok(None),
            Err(error) => Err(AuthError::CredentialStoreError(error.to_string())),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let id = self.keychain_id()?;
        let json = serde_json::to_string(&credentials)
            .map_err(|error| AuthError::CredentialStoreError(error.to_string()))?;
        let value = SecretString::new(json);
        match self.store.exists(&id) {
            Ok(true) => self.store.rotate(&id, value),
            Ok(false) => self.store.create(&id, value),
            Err(error) => Err(error),
        }
        .map_err(|error| AuthError::CredentialStoreError(error.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let id = self.keychain_id()?;
        match self.store.delete(&id) {
            Ok(()) | Err(SecretsError::NotFound { .. }) => Ok(()),
            Err(error) => Err(AuthError::CredentialStoreError(error.to_string())),
        }
    }

    async fn on_refresh_token_rejected(
        &self,
        _credentials: &StoredCredentials,
    ) -> Result<(), AuthError> {
        self.clear().await
    }
}
// ---------------------------------------------------------------------------
// Policy-gated HTTP client for OAuth metadata
// ---------------------------------------------------------------------------

/// An [`OAuthHttpClient`] that routes every request through `reqwest` and is
/// gated by [`NetworkPolicy`] before a socket opens.
///
/// The gate uses [`NetworkAction::McpOAuth`] and [`NetworkTrigger::User`] —
/// sign-in is always an explicit user action. The policy is consulted once per
/// request; a denied action never reaches the transport.
pub struct PolicyOAuthHttpClient {
    follow_redirects: reqwest::Client,
    stop_redirects: reqwest::Client,
    policy: Arc<NetworkPolicy>,
    log: Arc<AsyncMutex<NetworkEventLog>>,
}

impl PolicyOAuthHttpClient {
    /// Builds the gated client.
    #[must_use]
    pub fn new(policy: Arc<NetworkPolicy>, log: Arc<AsyncMutex<NetworkEventLog>>) -> Self {
        crate::tls::ensure_crypto_provider();
        let follow_redirects = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::default())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let stop_redirects = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            follow_redirects,
            stop_redirects,
            policy,
            log,
        }
    }
}

impl OAuthHttpClient for PolicyOAuthHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        let OAuthHttpRequest {
            request: oauth_request,
            redirect_policy,
            timeout,
            ..
        } = request;
        let client = match redirect_policy {
            OAuthHttpRedirectPolicy::Follow => self.follow_redirects.clone(),
            OAuthHttpRedirectPolicy::Stop | _ => self.stop_redirects.clone(),
        };
        let policy = self.policy.clone();
        let log = self.log.clone();
        let timeout = timeout.unwrap_or(Duration::from_secs(30));

        Box::pin(async move {
            let url_text = oauth_request.uri().to_string();
            let parsed = Url::parse(&url_text).map_err(|error| {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    error.to_string(),
                )) as OAuthHttpClientError
            })?;
            let host = parsed.host_str().unwrap_or_default().to_owned();
            policy
                .authorize(NetworkAction::McpOAuth, &host, NetworkTrigger::User)
                .map_err(|denied| {
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("network policy denied: {}", denied.reason),
                    )) as OAuthHttpClientError
                })?;

            let method = reqwest::Method::from_bytes(oauth_request.method().as_str().as_bytes())
                .map_err(|error| {
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        error.to_string(),
                    )) as OAuthHttpClientError
                })?;
            let mut builder = client.request(method, &url_text);
            for (name, value) in oauth_request.headers() {
                builder = builder.header(name.as_str(), value.as_bytes());
            }
            if !oauth_request.body().is_empty() {
                builder = builder.body(oauth_request.body().clone());
            }
            builder = builder.timeout(timeout);

            let response = builder
                .send()
                .await
                .map_err(|error| Box::new(error) as OAuthHttpClientError)?;

            let bytes = response.content_length().unwrap_or(0);
            log.lock().await.record(NetworkEvent::new(
                time::OffsetDateTime::now_utc(),
                NetworkAction::McpOAuth,
                NetworkHost::parse(&host).ok(),
                bytes,
            ));

            let status = response.status();
            let headers = response.headers().clone();
            let body = response
                .bytes()
                .await
                .map_err(|error| Box::new(error) as OAuthHttpClientError)?;

            let mut http_response = http::Response::builder().status(status.as_u16());
            for (name, value) in headers.iter() {
                http_response = http_response.header(name.as_str(), value.as_bytes());
            }
            http_response.body(body.to_vec()).map_err(|error| {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                )) as OAuthHttpClientError
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Browser opener
// ---------------------------------------------------------------------------

/// Opens a URL in the system browser.
///
/// Production uses [`SystemBrowser`]; tests inject a fake that performs the
/// redirect to the loopback listener without touching the desktop.
pub trait BrowserOpener: Send + Sync {
    /// Opens `url`.
    ///
    /// # Errors
    /// The platform error when the browser cannot be launched.
    fn open(&self, url: &str) -> Result<(), String>;
}

/// The real opener: shells out to the OS URL handler.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        webbrowser::open(url).map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Sign-in result and flow
// ---------------------------------------------------------------------------

/// What a successful sign-in produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInOutcome {
    /// The issuer the tokens belong to.
    pub issuer: String,
    /// The client id used (stored or dynamically registered).
    pub client_id: String,
    /// Scopes the grant returned.
    pub scopes: Vec<String>,
    /// Whether a refresh token was stored.
    pub has_refresh_token: bool,
}

/// Drives one OAuth sign-in for a saved HTTP MCP server.
///
/// Construct with [`OAuthClient::new`]; call [`OAuthClient::sign_in`] only from a
/// user gesture. The flow:
///
/// 1. `authorize_connect` with [`NetworkAction::McpOAuth`] — no request is made
///    before this passes.
/// 2. Bind the loopback listener.
/// 3. `AuthorizationManager` resolves RFC 9728 protected-resource metadata then
///    RFC 8414 authorization-server metadata; registers a client via RFC 7591
///    when the server advertises `registration_endpoint` and no client id is
///    stored.
/// 4. `get_authorization_url` issues PKCE S256 + state; the browser opens it.
/// 5. The listener waits for `/callback`; state and code are verified by
///    `exchange_code_for_token` (which re-derives the PKCE verifier from state).
/// 6. The token lands in [`SecretStore`] under `oauth:<server-id>:<issuer>`.
pub struct OAuthClient {
    server: McpServer,
    policy: Arc<NetworkPolicy>,
    secrets: Arc<dyn SecretStore>,
    log: Arc<AsyncMutex<NetworkEventLog>>,
    opener: Arc<dyn BrowserOpener>,
}

impl OAuthClient {
    /// Builds a client for `server`.
    #[must_use]
    pub fn new(
        server: McpServer,
        policy: Arc<NetworkPolicy>,
        secrets: Arc<dyn SecretStore>,
        log: Arc<AsyncMutex<NetworkEventLog>>,
    ) -> Self {
        Self {
            server,
            policy,
            secrets,
            log,
            opener: Arc::new(SystemBrowser),
        }
    }

    /// Replaces the browser opener (tests only).
    #[must_use]
    pub fn with_opener(mut self, opener: Arc<dyn BrowserOpener>) -> Self {
        self.opener = opener;
        self
    }

    /// The HTTP endpoint URL of the server, when it is remote.
    fn endpoint_url(&self) -> Result<Url, SignInError> {
        match &self.server.transport {
            McpTransport::Http { url, .. } => Ok(url.clone()),
            McpTransport::Stdio { .. } => Err(SignInError::new(
                SignInErrorKind::OAuthError,
                "This server uses stdio; there is nothing to sign in to.",
            )),
        }
    }

    /// Runs the full sign-in flow.
    ///
    /// `cancel` aborts the listener wait and any in-flight request. The caller
    /// must have obtained explicit user consent; this method itself performs
    /// the policy check before the first network call.
    ///
    /// # Errors
    /// [`SignInError`] on policy denial, listener failure, state mismatch,
    /// token-exchange failure, or store failure.
    pub async fn sign_in(&self, cancel: &CancellationToken) -> Result<SignInOutcome, SignInError> {
        if cancel.is_cancelled() {
            return Err(SignInError::cancelled());
        }

        let endpoint = self.endpoint_url()?;

        // Policy gate: no socket opens before this passes.
        match authorize_connect(
            self.policy.as_ref(),
            &self.server,
            ConnectIntent::UserTest,
            NetworkAction::McpOAuth,
        ) {
            Ok(McpAccess::Http(authorized)) => {
                debug!(host = %authorized.host(), port = authorized.port(), "OAuth sign-in authorized");
            }
            Ok(McpAccess::Local) => {
                return Err(SignInError::new(
                    SignInErrorKind::OAuthError,
                    "The server is not remote; there is nothing to sign in to.",
                ));
            }
            Err(error) => {
                return Err(SignInError::new(
                    SignInErrorKind::Denied,
                    format!("The network policy blocked sign-in: {error}"),
                ));
            }
        }

        // Bind the loopback listener before any discovery request.
        let listener = LoopbackListener::bind().await?;
        let redirect_uri = listener.redirect_uri();

        let mut manager = AuthorizationManager::new_with_oauth_http_client(
            endpoint.as_str(),
            Arc::new(PolicyOAuthHttpClient::new(
                self.policy.clone(),
                self.log.clone(),
            )),
        )
        .await
        .map_err(|error| SignInError::from_auth(&error))?;

        // Discover metadata (RFC 9728 protected resource, then RFC 8414 AS).
        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(|error| SignInError::from_auth(&error))?;
        manager.set_metadata(resolution.metadata.clone());

        let issuer = resolution
            .metadata
            .issuer
            .clone()
            .unwrap_or_else(|| endpoint.as_str().trim_end_matches('/').to_owned());

        // Install the credential store now that the issuer is known.
        manager.set_credential_store(KeychainCredentialStore::new(
            self.secrets.clone(),
            self.server.id,
            issuer.clone(),
        ));

        // Configure the OAuth client and produce the authorization URL.
        let scopes = self.requested_scopes();
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let stored_client_id = self.stored_client_id();

        let request = match stored_client_id {
            Some(client_id) => AuthorizationRequest::new(redirect_uri.clone())
                .with_preregistered_client(client_id)
                .with_scopes(scope_refs.clone()),
            None => AuthorizationRequest::new(redirect_uri.clone()).with_scopes(scope_refs.clone()),
        };

        let session = match AuthorizationSession::new(manager, request).await {
            Ok(session) => session,
            Err((_manager, error)) => {
                return Err(match &error {
                    AuthError::RegistrationFailed(reason) => SignInError::new(
                        SignInErrorKind::OAuthError,
                        format!("Dynamic client registration failed: {reason}"),
                    ),
                    other => SignInError::from_auth(other),
                });
            }
        };

        let auth_url = session.get_authorization_url().to_owned();

        // Open the browser; on failure the user can still complete the flow by
        // pasting the URL into a browser that does open.
        if let Err(reason) = self.opener.open(&auth_url) {
            debug!(reason, "browser open failed; waiting for manual redirect");
        }

        // Wait for the loopback callback; cancellation drops the listener.
        let callback_req = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(SignInError::cancelled()),
            result = listener.wait(SIGN_IN_TIMEOUT) => result?,
        };

        let token_response = match session.handle_callback_url(callback_req.url()).await {
            Ok(response) => {
                callback_req.respond(true, None);
                response
            }
            Err(error) => {
                let err_text = error.to_string();
                let lower = err_text.to_ascii_lowercase();
                let is_mismatch = lower.contains("state")
                    || lower.contains("csrf")
                    || lower.contains("verifier")
                    || lower.contains("pkce")
                    || lower.contains("invalid_grant");

                callback_req.respond(false, Some(&err_text));

                if is_mismatch {
                    return Err(SignInError::state_mismatch());
                }
                return Err(SignInError::from_auth(&error));
            }
        };

        let has_refresh_token = token_response.refresh_token().is_some();
        let granted_scopes = token_response
            .scopes()
            .map(|scopes| scopes.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let client_id = session
            .get_credentials()
            .await
            .map(|(id, _)| id)
            .unwrap_or_default();

        Ok(SignInOutcome {
            issuer,
            client_id,
            scopes: granted_scopes,
            has_refresh_token,
        })
    }

    /// Reads the OAuth profile scopes from the server definition.
    fn requested_scopes(&self) -> Vec<String> {
        match &self.server.transport {
            McpTransport::Http {
                oauth: Some(profile),
                ..
            } => profile.scopes.clone(),
            _ => Vec::new(),
        }
    }

    /// Reads the stored client id from the OAuth profile.
    fn stored_client_id(&self) -> Option<String> {
        match &self.server.transport {
            McpTransport::Http {
                oauth: Some(profile),
                ..
            } if !profile.client_id.is_empty() => Some(profile.client_id.clone()),
            _ => None,
        }
    }
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient")
            .field("server_id", &self.server.id)
            .finish_non_exhaustive()
    }
}
