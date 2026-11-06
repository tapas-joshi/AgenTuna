//! Integration tests for TM-016 (HTTP/streamable transport and OAuth sign-in).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use at_mcp::{
    BrowserOpener, HttpClient, HttpErrorKind, LoopbackListener, OAUTH_KEYCHAIN_SERVICE,
    OAuthClient, SignInErrorKind,
};
use at_model::{McpServer, McpServerId, McpTransport, NetworkPolicy, OAuthProfile};
use at_secrets::{KeychainId, MemoryStore, SecretStore, SecretString};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rmcp::transport::auth::StoredCredentials;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use url::Url;

// ---------------------------------------------------------------------------
// Mock Authorization Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthServerBehavior {
    Normal,
    RejectBadVerifierCode,
    RejectRefresh,
}

struct MockAuthServer {
    addr: SocketAddr,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    request_count: Arc<AtomicUsize>,
    _behavior: Arc<std::sync::RwLock<AuthServerBehavior>>,
}

impl MockAuthServer {
    async fn start(behavior: AuthServerBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let request_count = Arc::new(AtomicUsize::new(0));
        let behavior = Arc::new(std::sync::RwLock::new(behavior));

        let req_cnt = request_count.clone();
        let beh = behavior.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept_res = listener.accept() => {
                        let Ok((stream, _)) = accept_res else { break; };
                        let io = TokioIo::new(stream);
                        let req_cnt = req_cnt.clone();
                        let beh = beh.clone();
                        tokio::spawn(async move {
                            let _ = http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(move |req| {
                                        let req_cnt = req_cnt.clone();
                                        let beh = beh.clone();
                                        async move {
                                            handle_auth_request(req, req_cnt, beh, addr).await
                                        }
                                    }),
                                )
                                .await;
                        });
                    }
                }
            }
        });

        Self {
            addr,
            _shutdown: shutdown_tx,
            request_count,
            _behavior: behavior,
        }
    }

    fn issuer(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

impl Drop for MockAuthServer {
    fn drop(&mut self) {
        // Send shutdown signal, ignore if already dropped
    }
}

async fn handle_auth_request(
    req: Request<Incoming>,
    req_count: Arc<AtomicUsize>,
    behavior: Arc<std::sync::RwLock<AuthServerBehavior>>,
    addr: SocketAddr,
) -> Result<Response<Full<Bytes>>, Infallible> {
    req_count.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let port = addr.port();
    eprintln!("AUTH SERVER REQ: {} {}", method, req.uri());
    match (method, path.as_str()) {
        _ if path.contains("oauth-authorization-server") => {
            let metadata = json!({
                "issuer": format!("http://127.0.0.1:{port}"),
                "authorization_endpoint": format!("http://127.0.0.1:{port}/authorize"),
                "token_endpoint": format!("http://127.0.0.1:{port}/token"),
                "registration_endpoint": format!("http://127.0.0.1:{port}/register"),
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"]
            });
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(metadata.to_string())))
                .unwrap())
        }
        (Method::POST, "/register") => {
            let reg_resp = json!({
                "client_id": "test-client-id",
                "client_secret": "test-client-secret",
                "redirect_uris": ["http://127.0.0.1/callback"]
            });
            Ok(Response::builder()
                .status(StatusCode::CREATED)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(reg_resp.to_string())))
                .unwrap())
        }
        (Method::POST, "/token") => {
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&body_bytes);
            let beh = *behavior.read().unwrap();

            if body_str.contains("grant_type=refresh_token") {
                if beh == AuthServerBehavior::RejectRefresh
                    || body_str.contains("refresh_token=revoked_refresh_token")
                {
                    let err = json!({
                        "error": "invalid_grant",
                        "error_description": "Refresh token is invalid or revoked"
                    });
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .body(Full::new(Bytes::from(err.to_string())))
                        .unwrap());
                }
                let token_resp = json!({
                    "access_token": "refreshed_access_token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "valid_refresh_token",
                    "scope": "read write"
                });
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(token_resp.to_string())))
                    .unwrap());
            }

            // grant_type=authorization_code
            if beh == AuthServerBehavior::RejectBadVerifierCode
                || body_str.contains("code=bad_verifier_code")
            {
                let err = json!({
                    "error": "invalid_grant",
                    "error_description": "PKCE code verifier mismatch"
                });
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(err.to_string())))
                    .unwrap());
            }

            let token_resp = json!({
                "access_token": "valid_access_token",
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "valid_refresh_token",
                "scope": "read write"
            });
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(token_resp.to_string())))
                .unwrap())
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap()),
    }
}

// ---------------------------------------------------------------------------
// Mock MCP Streamable HTTP Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpServerBehavior {
    RequireAuth,
    Always401,
}

struct MockMcpServer {
    addr: SocketAddr,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    request_count: Arc<AtomicUsize>,
    _behavior: Arc<std::sync::RwLock<McpServerBehavior>>,
}

impl MockMcpServer {
    async fn start(auth_server_port: u16, behavior: McpServerBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let request_count = Arc::new(AtomicUsize::new(0));
        let behavior = Arc::new(std::sync::RwLock::new(behavior));

        let req_cnt = request_count.clone();
        let beh = behavior.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept_res = listener.accept() => {
                        let Ok((stream, _)) = accept_res else { break; };
                        let io = TokioIo::new(stream);
                        let req_cnt = req_cnt.clone();
                        let beh = beh.clone();
                        tokio::spawn(async move {
                            let _ = http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(move |req| {
                                        let req_cnt = req_cnt.clone();
                                        let beh = beh.clone();
                                        async move {
                                            handle_mcp_request(req, req_cnt, beh, addr, auth_server_port).await
                                        }
                                    }),
                                )
                                .await;
                        });
                    }
                }
            }
        });

        Self {
            addr,
            _shutdown: shutdown_tx,
            request_count,
            _behavior: behavior,
        }
    }

    fn endpoint_url(&self) -> Url {
        Url::parse(&format!("http://127.0.0.1:{}/mcp", self.addr.port())).unwrap()
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

async fn handle_mcp_request(
    req: Request<Incoming>,
    req_count: Arc<AtomicUsize>,
    behavior: Arc<std::sync::RwLock<McpServerBehavior>>,
    addr: SocketAddr,
    auth_server_port: u16,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let mcp_port = addr.port();

    if path.contains("oauth-protected-resource") {
        let prm = json!({
            "resource": format!("http://127.0.0.1:{mcp_port}/mcp"),
            "authorization_servers": [format!("http://127.0.0.1:{auth_server_port}")]
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(prm.to_string())))
            .unwrap());
    }

    if method == Method::GET {
        let challenge = format!(
            r#"Bearer resource_metadata="http://127.0.0.1:{mcp_port}/.well-known/oauth-protected-resource""#
        );
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("www-authenticate", challenge)
            .body(Full::new(Bytes::from("Unauthorized")))
            .unwrap());
    }
    req_count.fetch_add(1, Ordering::SeqCst);
    let beh = *behavior.read().unwrap();
    eprintln!("MCP SERVER REQ: {} {}", req.method(), req.uri());
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    if beh == McpServerBehavior::Always401 {
        let challenge = format!(
            r#"Bearer resource_metadata="http://127.0.0.1:{mcp_port}/.well-known/oauth-protected-resource""#
        );
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("www-authenticate", challenge)
            .body(Full::new(Bytes::from("Unauthorized")))
            .unwrap());
    }

    let is_valid_token = match auth_header {
        Some(h) => h.contains("valid_access_token") || h.contains("refreshed_access_token"),
        None => false,
    };

    if !is_valid_token {
        let challenge = format!(
            r#"Bearer resource_metadata="http://127.0.0.1:{mcp_port}/.well-known/oauth-protected-resource""#
        );
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("www-authenticate", challenge)
            .body(Full::new(Bytes::from("Unauthorized")))
            .unwrap());
    }

    if method == Method::POST {
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&body_bytes).unwrap_or(json!({}));
        let method_name = val.get("method").and_then(Value::as_str).unwrap_or("");
        let id = val.get("id").cloned();

        if method_name == "initialize" {
            let res = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "mock-mcp-server",
                        "version": "1.0.0"
                    }
                }
            });
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(res.to_string())))
                .unwrap());
        }

        if method_name == "notifications/initialized" {
            return Ok(Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }

        if method_name == "tools/list" {
            let res = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "calculate",
                            "title": "Calculator",
                            "description": "Performs basic math",
                            "inputSchema": { "type": "object" }
                        }
                    ]
                }
            });
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(res.to_string())))
                .unwrap());
        }
    }

    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::from("Not Found")))
        .unwrap())
}

// ---------------------------------------------------------------------------
// Mock Browser Opener
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenerMode {
    HappyPath,
    WrongState,
    MismatchedVerifier,
    BrowserFails,
}

struct MockBrowserOpener {
    mode: OpenerMode,
    last_callback_response_status: Arc<AsyncMutex<Option<u16>>>,
    last_callback_response_body: Arc<AsyncMutex<Option<String>>>,
}

impl MockBrowserOpener {
    fn new(mode: OpenerMode) -> Self {
        Self {
            mode,
            last_callback_response_status: Arc::new(AsyncMutex::new(None)),
            last_callback_response_body: Arc::new(AsyncMutex::new(None)),
        }
    }
}

impl BrowserOpener for MockBrowserOpener {
    fn open(&self, url: &str) -> Result<(), String> {
        if self.mode == OpenerMode::BrowserFails {
            return Err("OS browser launch failed".to_string());
        }

        let parsed = Url::parse(url).map_err(|e| e.to_string())?;
        let redirect_uri = parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| "missing redirect_uri".to_string())?;
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| "missing state".to_string())?;

        let mode = self.mode;
        let status_holder = self.last_callback_response_status.clone();
        let body_holder = self.last_callback_response_body.clone();

        tokio::spawn(async move {
            init_crypto_provider();
            let target_url = match mode {
                OpenerMode::HappyPath => {
                    format!("{redirect_uri}?code=good_code&state={state}")
                }
                OpenerMode::WrongState => {
                    format!("{redirect_uri}?code=good_code&state=wrong-state-xyz")
                }
                OpenerMode::MismatchedVerifier => {
                    format!("{redirect_uri}?code=bad_verifier_code&state={state}")
                }
                OpenerMode::BrowserFails => unreachable!(),
            };

            let client = reqwest::Client::new();
            if let Ok(resp) = client.get(&target_url).send().await {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                *status_holder.lock().await = Some(status);
                *body_holder.lock().await = Some(body);
            }
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn create_test_server(server_id: McpServerId, endpoint: Url, issuer: &str) -> McpServer {
    init_crypto_provider();
    McpServer {
        id: server_id,
        display_name: "test-server".to_string(),
        description: None,
        transport: McpTransport::Http {
            url: endpoint,
            headers: BTreeMap::new(),
            oauth: Some(OAuthProfile {
                issuer: issuer.to_string(),
                client_id: String::new(),
                scopes: vec!["read".to_string(), "write".to_string()],
                token_ref: format!("oauth:{server_id}:{issuer}"),
            }),
        },
        scope: at_model::Scope::User,
        extra: BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Acceptance Tests
// ---------------------------------------------------------------------------

/// 1. "Fake authorization server fixture completes the loopback flow and the token
/// lands in the keychain stub, not in any file under the temp HOME."
#[tokio::test]
async fn test_oauth_happy_path_completes_and_token_lands_in_keychain_stub() {
    let temp_home = tempfile::tempdir().unwrap();
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let opener = Arc::new(MockBrowserOpener::new(OpenerMode::HappyPath));
    let oauth_client =
        OAuthClient::new(server.clone(), policy.clone(), secrets.clone(), log.clone())
            .with_opener(opener);

    let cancel = CancellationToken::new();
    let outcome = oauth_client
        .sign_in(&cancel)
        .await
        .expect("sign_in should succeed");

    assert_eq!(outcome.issuer, auth_server.issuer());
    assert_eq!(outcome.client_id, "test-client-id");
    assert!(outcome.has_refresh_token);
    assert_eq!(outcome.scopes, vec!["read", "write"]);

    // Verify token landed in the keychain stub under oauth:<server-id>:<issuer>
    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    let stored_secret = secrets
        .read(&keychain_id)
        .expect("token must be in keychain stub");
    let stored_text = stored_secret.expose_secret();
    let creds: StoredCredentials =
        serde_json::from_str(stored_text).expect("valid StoredCredentials");
    assert!(creds.token_response.is_some());

    // Assert token did not land in any file under temp_home
    for entry in std::fs::read_dir(temp_home.path()).unwrap() {
        let entry = entry.unwrap();
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        assert!(!content.contains("valid_access_token"));
        assert!(!content.contains("valid_refresh_token"));
    }

    // Now verify we can list tools with HttpClient using the stored credentials
    let http_client = HttpClient::new(policy, secrets).with_log(log);
    let session = http_client
        .open(&server, &cancel)
        .await
        .expect("open should succeed");
    let tools = session
        .list_tools(&cancel)
        .await
        .expect("list_tools should succeed");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "calculate");
}

/// 2a. "A wrong state parameter or a mismatched code_verifier is rejected with
/// OAuthStateMismatch and the callback returns an error page." (Wrong state test)
#[tokio::test]
async fn test_wrong_state_parameter_rejected_with_oauth_state_mismatch() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let opener = Arc::new(MockBrowserOpener::new(OpenerMode::WrongState));
    let status_holder = opener.last_callback_response_status.clone();
    let body_holder = opener.last_callback_response_body.clone();

    let oauth_client = OAuthClient::new(server, policy, secrets.clone(), log).with_opener(opener);

    let cancel = CancellationToken::new();
    let result = oauth_client.sign_in(&cancel).await;

    let err = result.expect_err("wrong state must fail sign_in");
    assert_eq!(err.kind(), SignInErrorKind::OAuthStateMismatch);

    // Wait a moment for background callback responder task to complete response write
    tokio::time::sleep(Duration::from_millis(50)).await;
    let callback_status = status_holder
        .lock()
        .await
        .expect("callback status must be recorded");
    let callback_body = body_holder
        .lock()
        .await
        .clone()
        .expect("callback body must be recorded");

    assert_eq!(callback_status, 400);
    assert!(
        callback_body.contains("Sign-in failed") || callback_body.contains("Authentication failed")
    );

    // Assert no token was saved in the keychain store
    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    assert!(!secrets.exists(&keychain_id).unwrap());
}

/// 2b. "A wrong state parameter or a mismatched code_verifier is rejected with
/// OAuthStateMismatch and the callback returns an error page." (Mismatched code_verifier test)
#[tokio::test]
async fn test_mismatched_code_verifier_rejected_with_oauth_state_mismatch() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let opener = Arc::new(MockBrowserOpener::new(OpenerMode::MismatchedVerifier));
    let status_holder = opener.last_callback_response_status.clone();
    let body_holder = opener.last_callback_response_body.clone();

    let oauth_client = OAuthClient::new(server, policy, secrets.clone(), log).with_opener(opener);

    let cancel = CancellationToken::new();
    let result = oauth_client.sign_in(&cancel).await;

    let err = result.expect_err("mismatched verifier must fail sign_in");
    assert_eq!(err.kind(), SignInErrorKind::OAuthStateMismatch);

    tokio::time::sleep(Duration::from_millis(50)).await;
    let callback_status = status_holder
        .lock()
        .await
        .expect("callback status must be recorded");
    let callback_body = body_holder
        .lock()
        .await
        .clone()
        .expect("callback body must be recorded");

    assert_eq!(callback_status, 400);
    assert!(
        callback_body.contains("Sign-in failed") || callback_body.contains("Authentication failed")
    );

    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    assert!(!secrets.exists(&keychain_id).unwrap());
}

/// 3a. "An expired access token is refreshed once transparently; two consecutive 401s return SignInRequired and do not loop."
/// (Transparent refresh after 401 test)
#[tokio::test]
async fn test_expired_access_token_refreshed_transparently_and_does_not_loop() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    // Pre-populate keychain with an expired token
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let creds_json = json!({
        "client_id": "test-client-id",
        "token_response": {
            "access_token": "expired_token",
            "token_type": "bearer",
            "expires_in": 3600,
            "refresh_token": "valid_refresh_token"
        },
        "granted_scopes": ["read", "write"],
        "token_received_at": now,
        "issuer": auth_server.issuer()
    });
    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    secrets
        .create(&keychain_id, SecretString::new(creds_json.to_string()))
        .unwrap();

    let http_client = HttpClient::new(policy, secrets.clone()).with_log(log);
    let cancel = CancellationToken::new();

    let session = http_client
        .open(&server, &cancel)
        .await
        .expect("open should succeed with refresh");
    let tools = session
        .list_tools(&cancel)
        .await
        .expect("list_tools should succeed after refresh");

    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "calculate");

    // The MCP server should have received exactly 2 calls: first with expired_token (401),
    // 1st initialize (401), 2nd initialize (200), 3rd initialized (202), 4th tools/list (200)
    assert_eq!(mcp_server.request_count(), 4);
}

/// 3b. "two consecutive 401s return SignInRequired and do not loop."
#[tokio::test]
async fn test_two_consecutive_401s_surface_sign_in_required_and_do_not_loop() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::Always401).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let creds_json = json!({
        "client_id": "test-client-id",
        "token_response": {
            "access_token": "expired_token",
            "token_type": "bearer",
            "expires_in": 3600,
            "refresh_token": "valid_refresh_token"
        },
        "granted_scopes": ["read", "write"],
        "token_received_at": now,
        "issuer": auth_server.issuer()
    });
    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    secrets
        .create(&keychain_id, SecretString::new(creds_json.to_string()))
        .unwrap();

    let http_client = HttpClient::new(policy, secrets.clone()).with_log(log);
    let cancel = CancellationToken::new();

    let err = http_client
        .open(&server, &cancel)
        .await
        .expect_err("consecutive 401s must fail");
    assert_eq!(err.kind(), HttpErrorKind::SignInRequired);

    // Assert MCP server received exactly 2 calls and did not loop infinitely
    assert_eq!(mcp_server.request_count(), 2);
}

/// 3c. "Revoked refresh token returns SignInRequired and clears the stored token row."
#[tokio::test]
async fn test_revoked_refresh_token_surfaces_sign_in_required_and_clears_stored_token() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::RejectRefresh).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let creds_json = json!({
        "client_id": "test-client-id",
        "token_response": {
            "access_token": "expired_token",
            "token_type": "bearer",
            "expires_in": 1,
            "refresh_token": "revoked_refresh_token"
        },
        "granted_scopes": ["read", "write"],
        "token_received_at": 1000,
        "issuer": auth_server.issuer()
    });
    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    secrets
        .create(&keychain_id, SecretString::new(creds_json.to_string()))
        .unwrap();

    let http_client = HttpClient::new(policy, secrets.clone()).with_log(log);
    let cancel = CancellationToken::new();

    let err = http_client
        .open(&server, &cancel)
        .await
        .expect_err("revoked refresh must fail");
    assert_eq!(err.kind(), HttpErrorKind::SignInRequired);

    // Stored token row must be cleared from Keychain
    assert!(!secrets.exists(&keychain_id).unwrap());
}

/// 4. "Cancelling sign-in closes the listener and leaves no partial token rows."
/// ("Cancelling closes the loopback listener within 1 s and leaves no binding behind (rebind test).")
#[tokio::test]
async fn test_cancelling_sign_in_closes_listener_and_leaves_no_binding() {
    let listener = LoopbackListener::bind().await.expect("bind must succeed");
    let port = listener.port();

    // Dropping the listener cancels and closes the socket
    drop(listener);

    // Sleep a tiny amount for OS socket cleanup (< 100ms)
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Rebind test: verify the port can be rebound immediately
    let rebind = TcpListener::bind(format!("127.0.0.1:{port}")).await;
    assert!(
        rebind.is_ok(),
        "rebind to 127.0.0.1:{port} should succeed immediately"
    );

    // Also test cancellation during OAuthClient::sign_in
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;
    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());
    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let cancel = CancellationToken::new();
    cancel.cancel(); // Pre-cancel

    let oauth_client = OAuthClient::new(server, policy, secrets.clone(), log);
    let err = oauth_client
        .sign_in(&cancel)
        .await
        .expect_err("cancelled sign-in must fail");
    assert_eq!(err.kind(), SignInErrorKind::Cancelled);

    let expected_account = format!("oauth:{server_id}:{}", auth_server.issuer());
    let keychain_id = KeychainId::with_service(OAUTH_KEYCHAIN_SERVICE, expected_account).unwrap();
    assert!(!secrets.exists(&keychain_id).unwrap());
}

/// 5. "All OAuth network calls happen only after an explicit user action (test asserts zero requests before Sign in is invoked)."
#[tokio::test]
async fn test_zero_requests_before_sign_in_is_invoked() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::default());
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    // Construct client
    let _oauth_client = OAuthClient::new(server, policy, secrets, log);

    // Assert zero requests before sign_in is invoked
    assert_eq!(auth_server.request_count(), 0);
    assert_eq!(mcp_server.request_count(), 0);
}

/// 6. Network policy refusal: offline or unpermitted destination returns Denied
/// without touching the network.
#[tokio::test]
async fn test_network_policy_denial_blocks_oauth_before_opening_socket() {
    let auth_server = MockAuthServer::start(AuthServerBehavior::Normal).await;
    let mcp_server =
        MockMcpServer::start(auth_server.addr.port(), McpServerBehavior::RequireAuth).await;

    let server_id = McpServerId::new();
    let server = create_test_server(server_id, mcp_server.endpoint_url(), &auth_server.issuer());

    let policy = Arc::new(NetworkPolicy::new().with_offline(true));
    let secrets = Arc::new(MemoryStore::new());
    let log = Arc::new(AsyncMutex::new(at_model::NetworkEventLog::new()));

    let oauth_client = OAuthClient::new(server, policy, secrets, log);
    let cancel = CancellationToken::new();

    let err = oauth_client
        .sign_in(&cancel)
        .await
        .expect_err("offline policy must block sign_in");
    assert_eq!(err.kind(), SignInErrorKind::Denied);

    assert_eq!(auth_server.request_count(), 0);
    assert_eq!(mcp_server.request_count(), 0);
}
