//! The only place in `at-mcp` that may reach the network.
//!
//! An HTTP MCP server is contacted only when the user opened a saved server or pressed Test on
//! it, or when the health scheduler runs a check the user opted in to. Both paths go through
//! [`authorize_connect`]: it asks the [`NetworkPolicy`] for the destination host and returns the
//! sealed [`AuthorizedHttpConnect`] token that [`probe_endpoint`] requires. There is no other
//! way to open a connection from this crate, so an unpermitted call cannot reach the socket
//! layer, let alone a name lookup.
//!
//! A stdio server is a local child process: it consumes no network action and is allowed even in
//! offline mode, which is what makes a stdio health check work with `background_health=local`.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use at_model::network::{
    NetworkAction, NetworkDenied, NetworkEvent, NetworkEventLog, NetworkHost, NetworkPolicy,
    NetworkTrigger,
};
use at_model::{McpServer, McpServerId, McpTransport};
use time::OffsetDateTime;

/// How long one reachability probe may take before it is abandoned.
pub const MCP_TCP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the caller is connecting to an HTTP MCP server.
///
/// There is deliberately no "discovered" intent: a definition that was only found on disk is not
/// a [`McpServer`] yet, so it cannot be passed to [`authorize_connect`] before the user adopts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConnectIntent {
    /// The user opened a saved server.
    SavedServer,
    /// The user pressed Test on a saved server.
    UserTest,
    /// The background health scheduler ran a check.
    Scheduled,
}

impl ConnectIntent {
    /// Who started the connection.
    #[must_use]
    pub const fn trigger(self) -> NetworkTrigger {
        match self {
            Self::SavedServer | Self::UserTest => NetworkTrigger::User,
            Self::Scheduled => NetworkTrigger::Background,
        }
    }

    /// The network actions this intent may use.
    #[must_use]
    pub const fn actions(self) -> &'static [NetworkAction] {
        match self {
            Self::SavedServer | Self::UserTest => {
                &[NetworkAction::McpRemoteConnect, NetworkAction::McpOAuth]
            }
            Self::Scheduled => &[NetworkAction::ScheduledHttpHealthCheck],
        }
    }

    /// Stable id used in logs and tests.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::SavedServer => "saved_server",
            Self::UserTest => "user_test",
            Self::Scheduled => "scheduled",
        }
    }
}

impl std::fmt::Display for ConnectIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Proof that one HTTP connection to a saved server passed the network policy.
///
/// The fields are private and the type has no public constructor, so only
/// [`authorize_connect`] can mint one. A transport client takes `&AuthorizedHttpConnect`, which
/// makes "connect without an action" impossible to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedHttpConnect {
    server_id: McpServerId,
    action: NetworkAction,
    host: NetworkHost,
    port: u16,
}

impl AuthorizedHttpConnect {
    /// The saved server this connection belongs to.
    #[must_use]
    pub const fn server_id(&self) -> McpServerId {
        self.server_id
    }

    /// The registry action that was permitted.
    #[must_use]
    pub const fn action(&self) -> NetworkAction {
        self.action
    }

    /// Normalized destination host.
    #[must_use]
    pub const fn host(&self) -> &NetworkHost {
        &self.host
    }

    /// Destination port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

/// What the policy allowed for this server and intent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpAccess {
    /// A stdio child process: local, no network action consumed.
    Local,
    /// An HTTP endpoint with a permitted connection.
    Http(AuthorizedHttpConnect),
}

/// Failure of a gated MCP connection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpConnectError {
    /// The network policy refused the connection; no socket was opened.
    #[error(transparent)]
    Denied(NetworkDenied),
    /// The action does not belong to the intent, so the gate refuses to guess.
    #[error("`{action}` cannot run a {intent} connection")]
    WrongAction {
        /// Action the caller named.
        action: NetworkAction,
        /// Intent the caller named.
        intent: ConnectIntent,
    },
    /// The endpoint URL has no host or no usable port.
    #[error("the server URL has no usable host and port")]
    UnsupportedEndpoint,
    /// The connection itself failed.
    #[error("could not connect to {host}:{port}: {source}")]
    Connect {
        /// Destination host.
        host: NetworkHost,
        /// Destination port.
        port: u16,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
}

impl McpConnectError {
    /// The policy refusal, when this failure is one.
    #[must_use]
    pub fn denied(&self) -> Option<&NetworkDenied> {
        match self {
            Self::Denied(denied) => Some(denied),
            _ => None,
        }
    }

    /// Whether the health scheduler should store this as `skipped_offline`.
    ///
    /// True for a policy refusal — `background_health=local` or `off`, offline mode, a missing
    /// opt-in. It is not an error the user needs to see: the check simply did not run. TM-009 owns
    /// the persisted status vocabulary, so this answers the question without spelling it.
    #[must_use]
    pub fn is_policy_skip(&self) -> bool {
        self.denied().is_some()
    }
}

impl From<NetworkDenied> for McpConnectError {
    fn from(denied: NetworkDenied) -> Self {
        Self::Denied(denied)
    }
}

/// Decides whether `server` may be connected to for `intent`, using `action`.
///
/// A stdio server returns [`McpAccess::Local`] without consulting the policy: it runs on this
/// machine. An HTTP server needs a saved [`McpServer`], an action that matches the intent, a
/// resolvable host and a permitted [`NetworkPolicy`] decision.
///
/// # Errors
///
/// [`McpConnectError::Denied`] for a policy refusal (including offline mode, a missing
/// marketplace-style opt-in and the `background_health` gate),
/// [`McpConnectError::WrongAction`] for an inconsistent intent and action, and
/// [`McpConnectError::UnsupportedEndpoint`] for a URL without a host or port.
pub fn authorize_connect(
    policy: &NetworkPolicy,
    server: &McpServer,
    intent: ConnectIntent,
    action: NetworkAction,
) -> Result<McpAccess, McpConnectError> {
    let McpTransport::Http { url, .. } = &server.transport else {
        return Ok(McpAccess::Local);
    };
    if !intent.actions().contains(&action) {
        return Err(McpConnectError::WrongAction { action, intent });
    }
    let Some(host) = NetworkHost::from_url(url.as_str()) else {
        tracing::warn!(
            network = true,
            action = %action.wire_name(),
            intent = %intent,
            reason = "invalid_host",
            "blocked MCP network connection"
        );
        return Err(McpConnectError::Denied(NetworkDenied::invalid_host(action)));
    };
    let host = match policy.authorize(action, host.as_str(), intent.trigger()) {
        Ok(host) => host,
        Err(denied) => {
            tracing::warn!(
                network = true,
                action = %action.wire_name(),
                intent = %intent,
                host = %host,
                reason = %denied.reason.id(),
                "blocked MCP network connection"
            );
            return Err(McpConnectError::Denied(denied));
        }
    };
    let Some(port) = url.port_or_known_default() else {
        return Err(McpConnectError::UnsupportedEndpoint);
    };
    Ok(McpAccess::Http(AuthorizedHttpConnect {
        server_id: server.id,
        action,
        host,
        port,
    }))
}

/// Opens one TCP connection to the endpoint an [`AuthorizedHttpConnect`] names.
///
/// This is the only place in at-mcp that resolves a name or opens a socket; the policy decision
/// happened in [`authorize_connect`] and the token proves it. The attempt is recorded in `log`
/// with a zero byte count, because a reachability probe transfers no payload.
///
/// # Errors
///
/// [`McpConnectError::Connect`] when the name does not resolve or the connection is refused.
pub fn probe_endpoint(
    authorized: &AuthorizedHttpConnect,
    at: OffsetDateTime,
    log: &mut NetworkEventLog,
    connector: &dyn TcpConnector,
) -> Result<(), McpConnectError> {
    tracing::info!(
        network = true,
        action = %authorized.action.wire_name(),
        host = %authorized.host,
        port = authorized.port,
        "MCP network connection"
    );
    let result = connector.connect(authorized.host(), authorized.port, MCP_TCP_PROBE_TIMEOUT);
    log.record(NetworkEvent::new(
        at,
        authorized.action,
        Some(authorized.host().clone()),
        0,
    ));
    result.map_err(|source| McpConnectError::Connect {
        host: authorized.host().clone(),
        port: authorized.port,
        source,
    })
}

/// Opens TCP connections. The real implementation resolves and connects; tests record instead.
pub trait TcpConnector: Send + Sync {
    /// Connects to `host:port` within `timeout`.
    ///
    /// # Errors
    ///
    /// The operating-system error from resolution or the connection attempt.
    fn connect(&self, host: &NetworkHost, port: u16, timeout: Duration) -> io::Result<()>;
}

/// Connects through the operating system's resolver and TCP stack.
///
/// Resolution happens here, after the policy decision — a denied action never reaches it.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemTcpConnector;

impl TcpConnector for SystemTcpConnector {
    fn connect(&self, host: &NetworkHost, port: u16, timeout: Duration) -> io::Result<()> {
        let name = host
            .as_str()
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or_else(|| host.as_str());
        let addresses: Vec<SocketAddr> = (name, port).to_socket_addrs()?.collect();
        let mut last: Option<io::Error> = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(_stream) => return Ok(()),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "host resolved to no address")
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use url::Url;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn http_server(url: &str) -> McpServer {
        McpServer::from_def(
            McpServerId::new(),
            at_model::Scope::User,
            at_model::McpServerDef {
                display_name: "Remote".to_owned(),
                description: None,
                transport: McpTransport::Http {
                    url: Url::parse(url).unwrap(),
                    headers: BTreeMap::new(),
                    oauth: None,
                },
                extra: BTreeMap::new(),
            },
        )
    }

    fn stdio_server() -> McpServer {
        McpServer::from_def(
            McpServerId::new(),
            at_model::Scope::User,
            at_model::McpServerDef {
                display_name: "Local".to_owned(),
                description: None,
                transport: McpTransport::Stdio {
                    command: "npx".to_owned(),
                    args: vec!["-y".to_owned(), "some-server".to_owned()],
                    cwd: None,
                    env: BTreeMap::new(),
                },
                extra: BTreeMap::new(),
            },
        )
    }

    #[derive(Default)]
    struct CountingConnector {
        calls: AtomicUsize,
    }

    impl CountingConnector {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl TcpConnector for CountingConnector {
        fn connect(&self, _host: &NetworkHost, _port: u16, _timeout: Duration) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The production flow: authorize, then probe only if a token was minted.
    fn connect_once(
        policy: &NetworkPolicy,
        server: &McpServer,
        intent: ConnectIntent,
        action: NetworkAction,
        log: &mut NetworkEventLog,
        connector: &dyn TcpConnector,
    ) -> Result<(), McpConnectError> {
        match authorize_connect(policy, server, intent, action)? {
            McpAccess::Local => Ok(()),
            McpAccess::Http(authorized) => probe_endpoint(&authorized, NOW, log, connector),
        }
    }

    #[test]
    fn a_denied_http_server_never_reaches_the_connector() {
        let policy = NetworkPolicy::default().with_offline(true);
        let server = http_server("https://mcp.example.test/v1");
        let connector = CountingConnector::default();
        let mut log = NetworkEventLog::new();
        let error = connect_once(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
            &mut log,
            &connector,
        )
        .expect_err("offline mode refuses");
        assert_eq!(
            error.denied().expect("a policy refusal").reason,
            at_model::network::DenialReason::Offline
        );
        assert_eq!(connector.calls(), 0, "no connect call may happen");
        assert!(log.is_empty());
    }

    #[test]
    fn a_saved_http_server_connects_after_the_policy_permits_it() {
        let policy = NetworkPolicy::default();
        let server = http_server("https://user:token@mcp.example.test:8443/v1");
        let connector = CountingConnector::default();
        let mut log = NetworkEventLog::new();
        connect_once(
            &policy,
            &server,
            ConnectIntent::SavedServer,
            NetworkAction::McpRemoteConnect,
            &mut log,
            &connector,
        )
        .expect("permitted");

        assert_eq!(connector.calls(), 1);
        let events = log.to_vec();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, NetworkAction::McpRemoteConnect);
        assert_eq!(
            events[0].host.as_ref().map(NetworkHost::as_str),
            Some("mcp.example.test"),
            "user info and port never reach the log"
        );
        assert_eq!(events[0].bytes, 0);
    }

    #[test]
    fn the_token_carries_the_saved_server_host_and_port() {
        let policy = NetworkPolicy::default();
        let server = http_server("http://mcp.example.test/mcp");
        let access = authorize_connect(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
        )
        .expect("permitted");
        let McpAccess::Http(authorized) = access else {
            panic!("an http server yields an http token");
        };
        assert_eq!(authorized.server_id(), server.id);
        assert_eq!(authorized.action(), NetworkAction::McpRemoteConnect);
        assert_eq!(authorized.host().as_str(), "mcp.example.test");
        assert_eq!(authorized.port(), 80, "the default port of the scheme");
    }

    #[test]
    fn oauth_is_permitted_for_a_saved_server() {
        let policy = NetworkPolicy::default();
        let server = http_server("https://mcp.example.test/v1");
        let access = authorize_connect(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpOAuth,
        )
        .expect("permitted");
        assert!(
            matches!(access, McpAccess::Http(token) if token.action() == NetworkAction::McpOAuth)
        );
    }

    #[test]
    fn scheduled_http_checks_need_background_health_all() {
        let server = http_server("https://mcp.example.test/v1");
        let mut log = NetworkEventLog::new();

        for scope in [
            at_model::network::BackgroundHealth::Off,
            at_model::network::BackgroundHealth::Local,
        ] {
            let policy = NetworkPolicy::default().with_background_health(scope);
            let connector = CountingConnector::default();
            let error = connect_once(
                &policy,
                &server,
                ConnectIntent::Scheduled,
                NetworkAction::ScheduledHttpHealthCheck,
                &mut log,
                &connector,
            )
            .expect_err("not permitted");
            assert_eq!(
                error.denied().expect("a policy refusal").reason,
                at_model::network::DenialReason::ActionDisabled,
                "{scope:?} must be stored as skipped_offline, not connected"
            );
            assert!(
                error.is_policy_skip(),
                "the scheduler stores skipped_offline, not an error dialog"
            );
            assert_eq!(connector.calls(), 0);
        }
        assert!(log.is_empty());

        let policy = NetworkPolicy::default()
            .with_background_health(at_model::network::BackgroundHealth::All);
        let connector = CountingConnector::default();
        connect_once(
            &policy,
            &server,
            ConnectIntent::Scheduled,
            NetworkAction::ScheduledHttpHealthCheck,
            &mut log,
            &connector,
        )
        .expect("background_health=all permits the scheduled check");
        assert_eq!(connector.calls(), 1);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_stdio_check_needs_no_network_action() {
        let policy = NetworkPolicy::default().with_offline(true);
        let server = stdio_server();
        let connector = CountingConnector::default();
        let mut log = NetworkEventLog::new();
        for intent in [
            ConnectIntent::SavedServer,
            ConnectIntent::UserTest,
            ConnectIntent::Scheduled,
        ] {
            let access = authorize_connect(
                &policy,
                &server,
                intent,
                NetworkAction::ScheduledHttpHealthCheck,
            )
            .expect("stdio is local and needs no action");
            assert_eq!(access, McpAccess::Local);
        }
        assert_eq!(connector.calls(), 0);
        assert!(log.is_empty());
        connect_once(
            &policy,
            &server,
            ConnectIntent::Scheduled,
            NetworkAction::ScheduledHttpHealthCheck,
            &mut log,
            &connector,
        )
        .expect("a scheduled stdio check runs without a network action");
        assert_eq!(connector.calls(), 0);
        assert!(log.is_empty());
    }

    #[test]
    fn an_action_from_another_intent_is_refused() {
        let policy = NetworkPolicy::default()
            .with_background_health(at_model::network::BackgroundHealth::All);
        let server = http_server("https://mcp.example.test/v1");

        let error = authorize_connect(
            &policy,
            &server,
            ConnectIntent::Scheduled,
            NetworkAction::McpRemoteConnect,
        )
        .expect_err("the scheduler may not use the user action");
        assert!(matches!(
            error,
            McpConnectError::WrongAction {
                action: NetworkAction::McpRemoteConnect,
                intent: ConnectIntent::Scheduled
            }
        ));

        let error = authorize_connect(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::ScheduledHttpHealthCheck,
        )
        .expect_err("a user test may not use the background action");
        assert!(matches!(
            error,
            McpConnectError::WrongAction {
                action: NetworkAction::ScheduledHttpHealthCheck,
                ..
            }
        ));
    }

    #[test]
    fn only_the_scheduled_intent_uses_the_background_action() {
        for intent in [ConnectIntent::SavedServer, ConnectIntent::UserTest] {
            assert_eq!(intent.trigger(), NetworkTrigger::User);
            assert!(
                !intent
                    .actions()
                    .contains(&NetworkAction::ScheduledHttpHealthCheck)
            );
        }
        assert_eq!(
            ConnectIntent::Scheduled.trigger(),
            NetworkTrigger::Background
        );
        assert_eq!(
            ConnectIntent::Scheduled.actions(),
            &[NetworkAction::ScheduledHttpHealthCheck]
        );
    }

    #[test]
    fn the_system_connector_reaches_a_loopback_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        // Accept while the probe connects. The probe closes its socket as soon as it returns,
        // and a connection the client closes before `accept` can be dropped from the queue, so
        // a blocking accept has to be waiting when the handshake completes.
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = accepted_tx.send(listener.accept().is_ok());
        });

        let policy = NetworkPolicy::default();
        let server = http_server(&format!("http://127.0.0.1:{port}/mcp"));
        let access = authorize_connect(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
        )
        .expect("permitted");
        let McpAccess::Http(authorized) = access else {
            panic!("an http server yields an http token");
        };
        let mut log = NetworkEventLog::new();
        probe_endpoint(&authorized, NOW, &mut log, &SystemTcpConnector).expect("loopback connects");

        assert!(
            accepted_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the permitted probe must actually reach the listener"),
            "the permitted probe must actually reach the listener"
        );
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_denied_probe_leaves_the_loopback_listener_untouched() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");

        let policy = NetworkPolicy::default().with_offline(true);
        let server = http_server(&format!("http://127.0.0.1:{port}/mcp"));
        let mut log = NetworkEventLog::new();
        let error = connect_once(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
            &mut log,
            &SystemTcpConnector,
        )
        .expect_err("offline mode refuses");
        assert!(error.denied().is_some());
        assert!(
            listener.accept().is_err(),
            "no connection may reach the listener"
        );
    }

    #[test]
    fn a_url_without_a_host_is_refused() {
        let policy = NetworkPolicy::default();
        let server = http_server("file:///tmp/socket");
        let error = authorize_connect(
            &policy,
            &server,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
        )
        .expect_err("no host");
        assert_eq!(
            error.denied().expect("a policy refusal").reason,
            at_model::network::DenialReason::InvalidHost
        );
    }

    #[test]
    fn the_wrong_action_error_does_not_leak_credentials() {
        let error = McpConnectError::WrongAction {
            action: NetworkAction::GitFetch,
            intent: ConnectIntent::UserTest,
        };
        let text = error.to_string();
        assert!(text.contains("git_fetch"));
        assert!(!text.contains("token"));
    }

    #[test]
    fn distinct_servers_get_distinct_tokens() {
        let policy = NetworkPolicy::default();
        let first = http_server("https://one.example.test/mcp");
        let second = http_server("https://two.example.test/mcp");
        let a = authorize_connect(
            &policy,
            &first,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
        )
        .expect("permitted");
        let b = authorize_connect(
            &policy,
            &second,
            ConnectIntent::UserTest,
            NetworkAction::McpRemoteConnect,
        )
        .expect("permitted");
        let (McpAccess::Http(a), McpAccess::Http(b)) = (a, b) else {
            panic!("http servers yield tokens");
        };
        assert_ne!(a.server_id(), b.server_id());
        assert_ne!(a.host(), b.host());
    }

    #[test]
    fn a_refusal_is_a_network_denied_report() {
        let policy = NetworkPolicy::default().with_offline(true);
        let server = http_server("https://mcp.example.test/v1");
        let error = authorize_connect(
            &policy,
            &server,
            ConnectIntent::SavedServer,
            NetworkAction::McpRemoteConnect,
        )
        .expect_err("offline");
        let denied = error.denied().expect("a policy refusal");
        assert_eq!(denied.report().kind, at_model::ErrorKind::NetworkDenied);
        assert!(!denied.report().retryable);
    }
}
